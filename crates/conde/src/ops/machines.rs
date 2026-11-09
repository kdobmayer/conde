//! A person's agent machine (`machines`, one per person). `machine.setup` creates the record, or
//! adopts a host the person joined by hand, or clears a failed deployment for a retry; its answer
//! says `deploy` when a deployment PUT is due, which conde-remote issues (the Azure client lives
//! there). `machine.status` reads the owner's own record with the agent's state. The startup exchange: the
//! first live endpoint of the person's `praxy` on the machine's host gets one DM from the
//! person's human row, and the agent's reply to it marks the machine ready. The sign-in relay: the
//! page starts a flow, sends the pasted Claude code or cancels; the command waits in the record
//! until the bridge connection of the machine's host sends it.

use serde_json::{Value, json};

use super::agents::{self, Agent, PUBLIC_AGENT};
use super::messages::{Target, insert_message};
use super::web::{ONLINE_TTL_MS, browser_only};
use crate::address::Kind;
use crate::dispatch::{
    Ctx, MachineSettings, OpResult, bad_request, conflict, forbidden, not_found, opt_str, str_param,
};
use crate::humans;
use crate::store::{MachineDeploy, MachineRow, NewMachine, StoreResult, Tx, topics};
use crate::util::{iso_from_ms, ms_from_iso};

/// A deployment accepted this long ago whose machine never joined the board is not connected.
pub const JOIN_DEADLINE_MS: i64 = 30 * 60 * 1000;
/// The machine's name, host id and deployment name: this prefix and the owner name.
pub const HOST_PREFIX: &str = "vm-agent-";
pub const PROBE_TEXT: &str = "Your machine is set up. Reply to this message in one line so the board knows you are reachable.";
/// A relay no bridge took within this long of its start reads as expired, and its start is not
/// sent any more.
pub const RELAY_START_TTL_MS: i64 = 60_000;
/// The sign-in flows a relay runs (`relay-<flow>` on the machine).
pub const RELAY_FLOWS: &[&str] = &["claude", "github", "codex"];

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

/// The user's published `praxy`.
fn public_agent(db: &dyn Tx, user_id: i64) -> OpResult<Option<Agent>> {
    Ok(agents::by_name(db, user_id, PUBLIC_AGENT)?
        .filter(|a| a.address.as_ref().is_some_and(|x| x.kind == Kind::Mailbox)))
}

/// A host joined by hand that the record adopts: `vm-agent-<owner>` with a live key, else the
/// host of the published `praxy` with a live key (the pilot VMs joined under a short name).
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

/// `machine.setup {}`: the browser's "Set up Praxy". No record: adopts a hand-joined host
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
        "sign_in": sign_in_json(m), "relay": relay_json(m, now_ms), "admins": admins,
        "now": iso_from_ms(now_ms),
    }))
}

/// Whether a relay in `starting` since `at` was taken by no bridge in time.
pub fn relay_start_stale(at: Option<&str>, now_ms: i64) -> bool {
    at.and_then(ms_from_iso)
        .is_none_or(|at| now_ms - at > RELAY_START_TTL_MS)
}

/// The record's relay as `machine.status` answers it; null when none was started.
fn relay_json(m: &MachineRow, now_ms: i64) -> Value {
    let Some(relay_id) = &m.relay_id else {
        return Value::Null;
    };
    let stale = m.relay_state.as_deref() == Some("starting")
        && relay_start_stale(m.relay_at.as_deref(), now_ms);
    let (state, reason) = if stale {
        (Some("failed"), Some("expired"))
    } else {
        (m.relay_state.as_deref(), m.relay_reason.as_deref())
    };
    json!({"relay_id": relay_id, "flow": m.relay_flow, "state": state, "url": m.relay_url,
           "user_code": m.relay_user_code, "expires_at": m.relay_expires_at, "reason": reason,
           "at": m.relay_at})
}

/// The signed-in owner's own record, for the relay ops.
fn own_machine(ctx: &Ctx) -> OpResult<MachineRow> {
    browser_only(ctx)?;
    ctx.db
        .machine_of_user(ctx.p.user_id)?
        .ok_or_else(|| not_found("no machine is set up for you"))
}

/// `machine.relay.start {flow}` → `{relay_id}`: a new relay replaces the live one (the bridge
/// stops the running helper when the start reaches it).
pub fn relay_start(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let m = own_machine(ctx)?;
    let flow = str_param(body, "flow")?;
    if !RELAY_FLOWS.contains(&flow) {
        return Err(bad_request("flow is claude, github or codex"));
    }
    if m.deleting_at.is_some() {
        return Err(conflict("your machine is being removed"));
    }
    let relay_id = crate::util::new_ulid();
    ctx.db
        .machine_relay_start(m.user_id, &relay_id, flow, &ctx.now_iso())?;
    ctx.db.wake_host(&m.host_id);
    Ok(json!({"relay_id": relay_id}))
}

/// `machine.relay.code {relay_id, code}`: the code the person pasted, for the live Claude relay
/// showing its URL. Checked for form only; never logged and never in an answer or an error.
pub fn relay_code(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let m = own_machine(ctx)?;
    let relay_id = str_param(body, "relay_id")?;
    let code = str_param(body, "code")?;
    let len = code.chars().count();
    if !(1..=256).contains(&len) || code.chars().any(char::is_control) {
        return Err(bad_request(
            "the code is 1 to 256 characters without control characters",
        ));
    }
    if !ctx.db.machine_relay_code(m.user_id, relay_id, code)? {
        return Err(conflict("this sign-in is not waiting for a code"));
    }
    ctx.db.wake_host(&m.host_id);
    Ok(json!({"relay_id": relay_id}))
}

/// `machine.relay.cancel {relay_id}`: stops the live relay.
pub fn relay_cancel(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let m = own_machine(ctx)?;
    let relay_id = str_param(body, "relay_id")?;
    if !ctx
        .db
        .machine_relay_cancel(m.user_id, relay_id, &ctx.now_iso())?
    {
        return Err(conflict("this sign-in is not running"));
    }
    ctx.db.wake_host(&m.host_id);
    Ok(json!({"relay_id": relay_id}))
}

/// The record's sign-in flows as JSON (null when none were reported or unreadable).
pub fn sign_in_json(m: &MachineRow) -> Value {
    m.sign_in
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null)
}

/// Rewrites the sign-in flows of `host`'s record with `change`; false without a record.
fn change_flows(
    db: &dyn Tx,
    host: &str,
    change: impl FnOnce(&MachineRow, &mut serde_json::Map<String, Value>),
) -> StoreResult<bool> {
    let Some(m) = db.machine_of_host(host)? else {
        return Ok(false);
    };
    let mut flows = match sign_in_json(&m) {
        Value::Object(o) => o,
        _ => serde_json::Map::new(),
    };
    change(&m, &mut flows);
    db.machine_set_sign_in(m.user_id, Some(&Value::Object(flows).to_string()))?;
    Ok(true)
}

/// The machine's `needs_sign_in` for `flow`, or its `signed_in` (no flow): its `flows` map
/// replaces the record's, and a `needs_sign_in` without the map marks `flow` expired. Whether a
/// record took it.
pub fn needs_sign_in(
    db: &dyn Tx,
    host: &str,
    flow: Option<&str>,
    flows: Option<&Value>,
) -> StoreResult<bool> {
    match (flows.and_then(Value::as_object), flow) {
        (Some(all), _) => change_flows(db, host, |_, f| *f = all.clone()),
        (None, Some(flow)) => change_flows(db, host, |_, f| {
            f.insert(flow.to_string(), json!("expired"));
        }),
        (None, None) => Ok(false),
    }
}

/// A relay of `host` ended `done`: its flow is signed in until the machine's next check says
/// otherwise.
pub fn relay_signed_in(db: &dyn Tx, host: &str) -> StoreResult<bool> {
    change_flows(db, host, |m, f| {
        if let Some(flow) = &m.relay_flow {
            f.insert(flow.clone(), json!("ok"));
        }
    })
}

/// The machine's `online`: the `praxy` session went live. Whether a record took it.
pub fn session_online(db: &dyn Tx, host: &str, at: &str) -> StoreResult<bool> {
    let Some(m) = db.machine_of_host(host)? else {
        return Ok(false);
    };
    db.machine_set_session_online(m.user_id, at)?;
    Ok(true)
}

/// Whether the machine joined the board: minted, or joined by hand with a live key of its host
/// (a `conde login` on the VM).
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
/// `praxy` published on the host), `disconnected` (a bridge was seen, no heartbeat within the
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
/// of the owner's `praxy` published on `host`, the record's host.
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
/// `praxy` on the record's host, sending from that host, and `reply_to` is the probe.
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
