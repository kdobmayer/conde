//! A person's agent machine (`machines`, one per person). `machine.setup` creates the record, or
//! adopts a host the person joined by hand, or clears a failed deployment for a retry; its answer
//! says `deploy` when a deployment PUT is due, which conde-remote issues (the Azure client lives
//! there). `machine.status` reads the owner's own record with the agent's state. The startup exchange: the
//! first live endpoint of the person's `vm.agent` on the machine's host gets one DM from the
//! person's human row, and the agent's reply to it marks the machine ready.

use serde_json::{Value, json};

use super::agents::{self, Agent, PUBLIC_AGENT};
use super::messages::{Target, insert_message};
use super::web::{ONLINE_TTL_MS, browser_only};
use crate::address::Kind;
use crate::dispatch::{Ctx, MachineSettings, OpResult, conflict, forbidden, not_found, opt_str};
use crate::humans;
use crate::store::{MachineDeploy, MachineRow, NewMachine, Tx, topics};
use crate::util::{iso_from_ms, ms_from_iso};

/// A deployment accepted this long ago whose machine never joined the board is not connected.
pub const JOIN_DEADLINE_MS: i64 = 30 * 60 * 1000;
/// The machine's name, host id and deployment name: this prefix and the owner name.
pub const HOST_PREFIX: &str = "vm-agent-";
pub const PROBE_TEXT: &str = "Your machine is set up. Reply to this message in one line so the board knows you are reachable.";

fn settings<'a>(ctx: &Ctx<'a>) -> OpResult<&'a MachineSettings> {
    ctx.machines
        .ok_or_else(|| forbidden("this board sets up no agent machines"))
}

/// The hosts the user holds a live key of.
fn live_hosts(db: &dyn Tx, user: &str) -> OpResult<Vec<String>> {
    Ok(db
        .credentials()?
        .into_iter()
        .filter(|c| c.user == user && c.revoked_at.is_none())
        .map(|c| c.host)
        .collect())
}

/// The user's published `vm.agent`.
fn public_agent(db: &dyn Tx, user_id: i64) -> OpResult<Option<Agent>> {
    Ok(agents::by_name(db, user_id, PUBLIC_AGENT)?
        .filter(|a| a.address.as_ref().is_some_and(|x| x.kind == Kind::Mailbox)))
}

/// A host joined by hand that the record adopts: `vm-agent-<owner>` with a live key, else the
/// host of the published `vm.agent` with a live key (the pilot VMs joined under a short name).
fn adoptable(ctx: &Ctx) -> OpResult<Option<String>> {
    let live = live_hosts(ctx.db, &ctx.p.user)?;
    let named = format!("{HOST_PREFIX}{}", ctx.p.user);
    if live.contains(&named) {
        return Ok(Some(named));
    }
    Ok(public_agent(ctx.db, ctx.p.user_id)?
        .and_then(|a| a.address)
        .map(|a| a.host)
        .filter(|h| live.contains(h)))
}

/// `machine.setup {}`: the browser's "Set up my agent". No record: adopts a hand-joined host
/// (minted, no deployment) or creates the record for `vm-agent-<owner>` with the owner's Entra
/// object id; a record never issued or failed is cleared for a retry; otherwise nothing changes.
/// The answer is `machine.status`'s plus `deploy` (the PUT's parameters) when a PUT is due.
pub fn setup(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    let settings = settings(ctx)?;
    let uid = ctx.p.user_id;
    let oid = || -> OpResult<String> {
        ctx.db
            .identity_subject(uid, &settings.issuer)?
            .ok_or_else(|| forbidden("your account has no Entra sign-in on this board"))
    };
    let deploy = match ctx.db.machine_of_user(uid)? {
        Some(m) if m.deleting_at.is_some() => {
            return Err(conflict(
                "your machine is being removed; set it up again once it is gone",
            ));
        }
        Some(m)
            if m.minted_at.is_none() && (m.requested_at.is_none() || m.deploy_error.is_some()) =>
        {
            ctx.db.machine_set_deploy(uid, &MachineDeploy::default())?;
            true
        }
        Some(_) => false,
        None => match adoptable(ctx)? {
            Some(host) => {
                ctx.db.machine_request(&NewMachine {
                    user_id: uid,
                    host_id: &host,
                    owner_object_id: &oid()?,
                    vm_size: &settings.vm_size,
                    minted_at: Some(&ctx.now_iso()),
                })?;
                // a running VM's endpoints may never change again: probe from those on record
                let stored: Vec<(String, String)> = ctx
                    .db
                    .endpoint_rows()?
                    .into_iter()
                    .filter(|e| e.host == host)
                    .map(|e| (e.kind, e.target))
                    .collect();
                let live: Vec<(&str, &str)> = stored
                    .iter()
                    .map(|(k, t)| (k.as_str(), t.as_str()))
                    .collect();
                probe_host(ctx, uid, &host, &live)?;
                false
            }
            None => {
                ctx.db.machine_request(&NewMachine {
                    user_id: uid,
                    host_id: &format!("{HOST_PREFIX}{}", ctx.p.user),
                    owner_object_id: &oid()?,
                    vm_size: &settings.vm_size,
                    minted_at: None,
                })?;
                true
            }
        },
    };
    let m = ctx
        .db
        .machine_of_user(uid)?
        .ok_or_else(|| not_found("the machine record vanished"))?;
    let mut out = status_json(ctx.db, &m, ctx.now_ms)?;
    out["deploy"] = if deploy {
        json!({"owner": ctx.p.user, "host_id": m.host_id, "owner_object_id": m.owner_object_id,
               "vm_size": m.vm_size})
    } else {
        Value::Null
    };
    Ok(out)
}

/// `machine.status {}`: the signed-in owner's own record, in the browser only (a service reads a
/// person's state through `service.presence`); `not_found` without a record.
pub fn status(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    if opt_str(body, "owner")?.is_some_and(|o| o.to_lowercase() != ctx.p.user) {
        return Err(forbidden("the browser reads its own machine only"));
    }
    let m = ctx
        .db
        .machine_of_user(ctx.p.user_id)?
        .ok_or_else(|| not_found("no machine is set up for you"))?;
    status_json(ctx.db, &m, ctx.now_ms)
}

/// The record as `machine.status` answers it.
pub fn status_json(db: &dyn Tx, m: &MachineRow, now_ms: i64) -> OpResult<Value> {
    let admins: Vec<String> = db
        .users()?
        .into_iter()
        .filter(|u| u.admin && !u.disabled)
        .map(|u| u.name)
        .collect();
    Ok(json!({
        "state": state_of(db, m, now_ms)?, "host_id": m.host_id, "requested_at": m.requested_at,
        "minted_at": m.minted_at, "deploy_state": m.deploy_state, "deploy_error": m.deploy_error,
        "probe_id": m.probe_id, "ready_at": m.ready_at, "session_online_at": m.session_online_at,
        "sign_in": sign_in_json(m), "relay": null, "admins": admins, "now": iso_from_ms(now_ms),
    }))
}

/// The record's sign-in flows as JSON (null when none were reported or unreadable).
pub fn sign_in_json(m: &MachineRow) -> Value {
    m.sign_in
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null)
}

/// Whether the machine joined the board: minted, or (until the enrol route) joined by hand with a
/// live key of its host.
fn joined(db: &dyn Tx, m: &MachineRow) -> OpResult<bool> {
    if m.minted_at.is_some() {
        return Ok(true);
    }
    let owner = db
        .user_by_id(m.user_id)?
        .map(|u| u.name)
        .unwrap_or_default();
    Ok(live_hosts(db, &owner)?.contains(&m.host_id))
}

/// `deleting`; before the machine joined: `failed` (a terminal deployment error),
/// `not_connected` (accepted over [`JOIN_DEADLINE_MS`] ago), else `requested`; after: `ready`
/// (reachable and the startup exchange answered), `connected` (heartbeat within the TTL and
/// `vm.agent` published on the host), `disconnected` (a bridge was seen, no heartbeat within the
/// TTL), else `up`.
pub fn state_of(db: &dyn Tx, m: &MachineRow, now_ms: i64) -> OpResult<&'static str> {
    if m.deleting_at.is_some() {
        return Ok("deleting");
    }
    if !joined(db, m)? {
        if m.deploy_error.is_some() {
            return Ok("failed");
        }
        let late = m
            .requested_at
            .as_deref()
            .and_then(ms_from_iso)
            .is_some_and(|at| now_ms - at > JOIN_DEADLINE_MS);
        return Ok(if late { "not_connected" } else { "requested" });
    }
    let online_after = iso_from_ms(now_ms - ONLINE_TTL_MS);
    let bridge = db.host_bridges()?.into_iter().find(|h| h.host == m.host_id);
    let online = bridge
        .as_ref()
        .and_then(|b| b.heartbeat.as_ref())
        .is_some_and(|t| *t >= online_after);
    if !online {
        let seen = bridge.is_some_and(|b| b.last_seen.is_some());
        return Ok(if seen { "disconnected" } else { "up" });
    }
    let published = public_agent(db, m.user_id)?
        .and_then(|a| a.address)
        .is_some_and(|a| a.host == m.host_id);
    if !published {
        return Ok("up");
    }
    let reachable = agents::public_agent_reachable(db, m.user_id, now_ms)?.is_some();
    Ok(if reachable && m.ready_at.is_some() {
        "ready"
    } else {
        "connected"
    })
}

/// Whether the owner's record says the Claude sign-in expired (the reachability rule's clause).
pub fn claude_expired(db: &dyn Tx, owner_id: i64) -> OpResult<bool> {
    Ok(db
        .machine_of_user(owner_id)?
        .and_then(|m| m.sign_in)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .is_some_and(|v| v["claude"] == "expired"))
}

/// After a host's endpoints were replaced: the startup probe of the host's owner.
pub fn probe(ctx: &Ctx, live: &[(&str, &str)]) -> OpResult<()> {
    probe_host(ctx, ctx.p.user_id, &ctx.p.host, live)
}

/// The startup probe, once per record: when `live` (the endpoints of `host`) carries the mailbox
/// of the owner's `vm.agent` published on `host`, the record's host.
fn probe_host(ctx: &Ctx, owner_id: i64, host: &str, live: &[(&str, &str)]) -> OpResult<()> {
    let Some(m) = ctx.db.machine_of_user(owner_id)? else {
        return Ok(());
    };
    if m.host_id != host || m.probe_id.is_some() || m.deleting_at.is_some() {
        return Ok(());
    }
    let Some(agent) = public_agent(ctx.db, m.user_id)? else {
        return Ok(());
    };
    let Some(address) = agent.address.as_ref().filter(|a| a.host == m.host_id) else {
        return Ok(());
    };
    if !live
        .iter()
        .any(|(kind, target)| *kind == Kind::Mailbox.as_str() && *target == address.target)
    {
        return Ok(());
    }
    let Some(human) = humans::of_owner(ctx.db, m.user_id)? else {
        return Ok(());
    };
    let sent = insert_message(
        ctx,
        &human,
        Target::Agent(&agent),
        PROBE_TEXT,
        None,
        Vec::new(),
        None,
    )?;
    ctx.db.machine_set_probe(m.user_id, &sent.id)?;
    ctx.db
        .announce(topics::MESSAGES | topics::DELIVERY | topics::MACHINE);
    Ok(())
}

/// Whether a DM from `me` replying to `reply_to` answers the startup probe: `me` is the owner's
/// `vm.agent` on the record's host, sending from that host, and `reply_to` is the probe.
pub fn is_probe_reply(ctx: &Ctx, me: &Agent, reply_to: Option<&str>) -> OpResult<bool> {
    let Some(reply_to) = reply_to else {
        return Ok(false);
    };
    if me.name != PUBLIC_AGENT {
        return Ok(false);
    }
    let Some(m) = ctx.db.machine_of_user(me.owner_id)? else {
        return Ok(false);
    };
    Ok(m.probe_id.as_deref() == Some(reply_to)
        && ctx.p.host == m.host_id
        && me.address.as_ref().is_some_and(|a| a.host == m.host_id))
}

/// The probe was answered: the machine is ready.
pub fn mark_ready(ctx: &Ctx, owner_id: i64) -> OpResult<()> {
    if ctx.db.machine_set_ready(owner_id, &ctx.now_iso())? {
        ctx.db.announce(topics::MACHINE);
    }
    Ok(())
}
