//! Queries and the one mutation behind the web UI: the conversation rail, a conversation's
//! history with per-message delivery state, marking a conversation read, and the fleet.
//! Every rule of the other ops holds: DMs by sender or recipient, channels by membership,
//! delivery state of messages the caller's own agents sent (or addressed to the caller).

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

use super::agents::{self, Agent};
use super::channels;
use super::messages::{self, MsgRow};
use crate::address::Kind;
use crate::dispatch::{Ctx, OpResult, bad_request, not_found, opt_i64, opt_str};
use crate::protocol::PAGE;
use crate::store::{Delivery, Tx};
use crate::util::iso_from_ms;

/// A bridge is online while its last heartbeat is younger than this (three ping periods).
pub const ONLINE_TTL_MS: i64 = 90_000;
/// A notification still undelivered this long after its message counts as stuck.
pub const STUCK_AFTER_MS: i64 = 60_000;
/// Default page of a conversation's history.
const HISTORY_PAGE: i64 = 100;

/// Attempt outcomes that end an attempt without reaching the recipient.
const FAILED: &[&str] = &["expired", "dropped", "failed", "abandoned", "denied", "refused"];

/// `conversation.list`: the caller's DM partners and channels with unread counts.
pub fn list(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let dms: Vec<Value> = ctx
        .db
        .dm_threads(&me.id)?
        .into_iter()
        .map(|t| json!({"with": t.partner, "last_seq": t.last_seq, "last_at": t.last_at, "unread": t.unread}))
        .collect();
    let channels: Vec<Value> = ctx
        .db
        .channel_threads(&me.id)?
        .into_iter()
        .map(|c| {
            json!({"channel": format!("#{}", c.name), "topic": c.topic, "last_seq": c.last_seq,
                   "last_at": c.last_at, "unread": c.unread})
        })
        .collect();
    Ok(json!({"me": me.canonical(), "dms": dms, "channels": channels}))
}

/// The conversation a request names: `{"channel": "#name"}` or `{"with": "owner/name"}`.
enum Conversation {
    Channel(channels::Channel),
    Dm(Agent),
}

fn conversation(ctx: &Ctx, me: &Agent, body: &Value) -> OpResult<Conversation> {
    if let Some(raw) = opt_str(body, "channel")? {
        return Ok(Conversation::Channel(channels::member_channel(ctx, me, raw)?));
    }
    if let Some(name) = opt_str(body, "with")? {
        let partner = agents::find(ctx, name)?.ok_or_else(|| not_found(format!("no agent named '{name}'")))?;
        return Ok(Conversation::Dm(partner));
    }
    Err(bad_request("name the conversation: {\"channel\": \"#name\"} or {\"with\": \"owner/name\"}"))
}

/// `conversation.history`: one DM thread or channel, oldest first, a page before `before`.
pub fn history(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let before = opt_i64(body, "before")?.unwrap_or(i64::MAX);
    let limit = opt_i64(body, "limit")?.unwrap_or(HISTORY_PAGE).clamp(1, PAGE as i64);
    let (mut rows, head) = match conversation(ctx, &me, body)? {
        Conversation::Channel(c) => {
            let read_seq = channels::read_seq(ctx.db, &c.id, &me.id)?;
            let rows = ctx.db.channel_history(&c.id, before, limit + 1)?;
            (rows, json!({"channel": format!("#{}", c.name), "topic": c.topic, "read_seq": read_seq}))
        }
        Conversation::Dm(partner) => {
            let rows = ctx.db.dm_history(&me.id, &partner.id, before, limit + 1)?;
            let unread = ctx.db.unread_dms_from(&me.id, &partner.id, i64::MAX)?.len();
            let kind = partner.address.as_ref().map(|a| a.kind.as_str());
            (rows, json!({"with": partner.canonical(), "kind": kind, "unread": unread}))
        }
    };
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    rows.reverse();
    let mut out = Vec::with_capacity(rows.len());
    for m in &rows {
        let mut v = messages::view(ctx.db, m)?;
        v["delivery"] = delivery(ctx, &me, m)?;
        out.push(v);
    }
    let mut result = head;
    result["me"] = json!(me.canonical());
    result["messages"] = Value::Array(out);
    result["more"] = json!(more);
    Ok(result)
}

/// Per-recipient delivery state of `m`: every recipient when one of the caller's agents sent
/// it, else only the caller's own notification. Channel posts by the caller's agents also
/// carry how many members have read past them.
fn delivery(ctx: &Ctx, me: &Agent, m: &MsgRow) -> OpResult<Value> {
    let own = m.sender.split_once('/').is_some_and(|(owner, _)| owner == ctx.p.user);
    let mut recipients = Vec::new();
    let mut seen = Vec::new();
    for d in ctx.db.deliveries(&m.id)? {
        if !own && d.recipient_id != me.id {
            continue;
        }
        seen.push(d.recipient_id.clone());
        recipients.push(recipient_state(ctx.db, m, &d)?);
    }
    // A DM without a notification (imported history): its read state alone.
    if own
        && let (None, Some(rcpt), Some(name)) = (&m.channel_id, &m.recipient_id, &m.recipient)
        && !seen.contains(rcpt)
    {
        let read_at = ctx.db.dm_read_at(&m.id, rcpt)?;
        let state = if read_at.is_some() { "read" } else { "sent" };
        recipients.push(json!({"recipient": name, "state": state, "read_at": read_at}));
    }
    let read_by = match (&m.channel_id, own) {
        (Some(c), true) => {
            let (read, total) = ctx.db.channel_read_count(c, &m.sender_id, m.seq)?;
            json!({"read": read, "total": total})
        }
        _ => Value::Null,
    };
    Ok(json!({"recipients": recipients, "read_by": read_by}))
}

fn recipient_state(db: &dyn Tx, m: &MsgRow, d: &Delivery) -> OpResult<Value> {
    let read_at = match &m.channel_id {
        None => db.dm_read_at(&m.id, &d.recipient_id)?,
        Some(_) => None,
    };
    let read = read_at.is_some()
        || d.status == "read"
        || match &m.channel_id {
            Some(c) => db.read_seq(c, &d.recipient_id)?.is_some_and(|s| s >= m.seq),
            None => false,
        };
    // A human has no push transport: until read, their notification is the unread badge
    // (a bridge marks it so; one on the browser's host never runs).
    let human = || -> OpResult<bool> { Ok(agents::by_id(db, &d.recipient_id)?.is_some_and(|a| a.is_human())) };
    let state = match d.status.as_str() {
        _ if read => "read",
        "pending" | "dispatching" if human()? => "unread",
        "pending" if d.attempts > 0 => "retrying",
        "pending" => "queued",
        "dispatching" => "sending",
        // A human recipient: the unread badge is the notification.
        "badge" => "unread",
        other => other,
    };
    let retry_at = (state == "retrying").then(|| iso_from_ms(d.next_attempt_at));
    let last_error = match (&d.last_status, &d.last_reason) {
        (Some(s), reason) if FAILED.contains(&s.as_str()) => json!({"status": s, "reason": reason}),
        _ => Value::Null,
    };
    Ok(json!({
        "recipient": d.recipient, "kind": d.kind, "state": state, "status": d.status,
        "attempts": d.attempts, "retry_at": retry_at, "last_error": last_error, "read_at": read_at,
    }))
}

/// `conversation.read`: marks the conversation read up to `up_to` (a commit_seq): DMs from
/// the partner to the caller, or the caller's channel read position (forward only).
pub fn read(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let up_to = opt_i64(body, "up_to")?.ok_or_else(|| bad_request("missing integer field 'up_to'"))?;
    let marked = match conversation(ctx, &me, body)? {
        Conversation::Channel(c) => {
            let last = ctx.db.channel_last_seq(&c.id)?;
            let before = channels::read_seq(ctx.db, &c.id, &me.id)?.unwrap_or(0);
            let to = up_to.min(last);
            channels::advance_cursor(ctx.db, &c.id, &me.id, to)?;
            if to > before { 1 } else { 0 }
        }
        Conversation::Dm(partner) => {
            let ids = ctx.db.unread_dms_from(&me.id, &partner.id, up_to)?;
            let now = ctx.now_iso();
            for id in &ids {
                ctx.db.mark_dm_read(id, &me.id, &now)?;
            }
            ids.len()
        }
    };
    // Self-announcing (the dispatcher leaves it out): a read that marks nothing wakes no browser.
    if marked > 0 {
        ctx.db.announce(crate::store::topics::MESSAGES | crate::store::topics::DELIVERY);
    }
    Ok(json!({"marked": marked}))
}

/// `fleet.list`: every bound agent with its host's bridge state, whether the bridge reports
/// its session live (discovery), and for the caller's own agents the delivery health (held,
/// stuck, last reached: verified reachability).
pub fn fleet(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let now = ctx.now_ms;
    let online_after = iso_from_ms(now - ONLINE_TTL_MS);
    let stuck_before = iso_from_ms(now - STUCK_AFTER_MS);
    let mut hosts: BTreeMap<(String, String), Value> = BTreeMap::new();
    for h in ctx.db.host_bridges()? {
        let online = h.heartbeat.as_ref().is_some_and(|t| *t >= online_after);
        hosts.insert(
            (h.owner.clone(), h.host.clone()),
            json!({"host": h.host, "owner": h.owner, "bridge": true, "online": online,
                   "heartbeat": h.heartbeat, "last_seen": h.last_seen}),
        );
    }
    let endpoints: HashMap<(String, String, String), String> = ctx
        .db
        .endpoint_rows()?
        .into_iter()
        .map(|e| ((e.host, e.kind, e.target), e.seen_at))
        .collect();
    let health: HashMap<String, _> =
        ctx.db.agent_health(ctx.p.user_id, &stuck_before)?.into_iter().map(|h| (h.agent_id.clone(), h)).collect();
    let mut agents = Vec::new();
    for a in ctx.db.bound_agents()? {
        let Some(address) = &a.address else { continue };
        hosts.entry((a.owner.clone(), address.host.clone())).or_insert_with(|| {
            json!({"host": address.host, "owner": a.owner, "bridge": false, "online": false,
                   "heartbeat": null, "last_seen": null})
        });
        let discovered = match address.kind {
            Kind::Human => None,
            kind => endpoints.get(&(address.host.clone(), kind.as_str().to_string(), address.target.clone())),
        };
        let own = a.owner_id == ctx.p.user_id;
        let h = health.get(&a.id);
        let reached = h.and_then(|h| h.last_reached.clone());
        agents.push(json!({
            "name": a.canonical(), "owner": a.owner, "display": a.display, "kind": address.kind.as_str(),
            "host": address.host, "address": address.to_string(), "pinned": a.pinned,
            "discovered": discovered,
            "held": own.then(|| h.map_or(0, |h| h.held)),
            "stuck": own.then(|| h.map_or(0, |h| h.stuck)),
            "last_reached": if own { json!(reached) } else { Value::Null },
            "own": own,
        }));
    }
    Ok(json!({
        "now": iso_from_ms(now), "online_ttl_s": ONLINE_TTL_MS / 1000,
        "hosts": hosts.into_values().collect::<Vec<_>>(), "agents": agents,
    }))
}
