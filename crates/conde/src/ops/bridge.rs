//! Bridge ops: claim due notifications as attempts, report attempt outcomes.
//!
//! A notification (message + recipient) gets one attempt per dispatch, each
//! with a random id and the claiming bridge's instance id. Receipts settle
//! only their own attempt; the notification follows its latest attempt.
//! Accepted (no receipt yet) and held stay revisable; delivered, denied,
//! refused, expired, dropped, failed and abandoned close the attempt for good.
//! expired / dropped / failed retry with a fresh attempt after the backoff;
//! denied / refused end the notification.
//!
//! Ownership: an attempt is settled only by the bridge (host + instance) that
//! claimed it. Fencing: in cloud mode a newer instance of a host's bridge
//! fences the older one (`bridge.fence`); a fenced instance claims nothing.

use conde_remote::OutcomeReason;
use serde_json::{Value, json};

use super::agents;
use super::channels;
use super::messages::msg_by_id;
use crate::address::Kind;
use crate::dispatch::{Ctx, OpResult, bad_request, forbidden, opt_i64, opt_str, str_param};
use crate::store::topics;
use crate::util::new_ulid;

const TERMINAL: &[&str] = &["delivered", "denied", "refused", "expired", "dropped", "failed", "abandoned"];

fn instance<'a>(ctx: &'a Ctx) -> OpResult<&'a str> {
    ctx.p.bridge_instance.as_deref().ok_or_else(|| forbidden("not a bridge connection"))
}

/// Refuses an instance that a fence superseded (cloud mode; local mode never fences).
fn ensure_not_fenced(ctx: &Ctx, me: &str) -> OpResult<()> {
    let own = ctx.db.bridge_instance(me)?.map(|(_, state)| state);
    let active = ctx.db.active_bridges(&ctx.p.host)?;
    if own.as_deref() == Some("fenced") || active.iter().any(|a| a != me) {
        return Err(forbidden(format!(
            "bridge instance {me} is fenced: a newer bridge is active for host {}",
            ctx.p.host
        )));
    }
    Ok(())
}

/// One delivery job as the bridge sees it.
fn job(
    attempt_id: &str,
    notification_id: &str,
    address: &str,
    text: &str,
    recipient: &str,
    message_id: &str,
    binding_version: i64,
) -> Value {
    json!({"attempt_id": attempt_id, "notification_id": notification_id, "address": address, "text": text,
           "recipient": recipient, "message_id": message_id, "binding_version": binding_version})
}

/// `bridge.claim`. A v2 instance (body `window`, a host migrated to v2) gets new attempts only
/// up to its window minus its attempts without a first outcome; everything beyond stays pending
/// here. Its jobs carry the structured notify fields instead of a text, and the result carries
/// the host's routes from the same snapshot.
pub fn claim(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = instance(ctx)?;
    let window = opt_i64(body, "window")?;
    match (window.is_some(), ctx.mailbox_only) {
        (true, false) => return Err(forbidden(format!("host '{}' has not migrated to v2", ctx.p.host))),
        (false, true) => {
            return Err(forbidden(format!("host '{}' migrated to v2 and takes no v1 bridge; upgrade conde", ctx.p.host)));
        }
        _ => {}
    }
    ensure_not_fenced(ctx, me)?;
    abandon_stale(ctx, me)?;
    let limit = match window {
        Some(w) => w.clamp(1, 1000) - ctx.db.count_dispatching(me)?,
        None => opt_i64(body, "limit")?.unwrap_or(100).clamp(1, 1000),
    };
    let v2 = window.is_some();

    let due = if limit > 0 { ctx.db.due_notifications(ctx.now_ms, &ctx.p.host, limit)? } else { Vec::new() };
    if !due.is_empty() {
        ctx.db.announce(topics::DELIVERY);
    }
    let mut jobs = Vec::new();
    for n in due {
        let (notification_id, message_id, rcpt_id, kind) = (n.id, n.message_id, n.recipient_id, n.kind);
        let set_status = |status: &str| ctx.db.set_notification_status(&notification_id, status);
        let (Some(m), Some(rcpt)) = (msg_by_id(ctx.db, &message_id)?, agents::by_id(ctx.db, &rcpt_id)?) else {
            set_status("gone")?;
            continue;
        };
        let Some(address) = rcpt.address.clone() else { continue };
        let member_seq = match &m.channel_id {
            Some(c) => channels::read_seq(ctx.db, c, &rcpt.id)?,
            None => None,
        };
        let already_read = match &m.channel_id {
            Some(_) => member_seq.is_some_and(|s| s >= m.seq),
            None => ctx.db.dm_read_at(&m.id, &rcpt.id)?.is_some(),
        };
        if already_read {
            set_status("read")?;
            continue;
        }
        if rcpt.is_human() {
            // No push transport for humans: unread badges are the notification.
            set_status("badge")?;
            continue;
        }
        let place = m.place();
        let mut text = match kind.as_str() {
            "dm" => format!("conde: new message {} from {}. Read it with: conde read {}", m.id, m.sender, m.id),
            "mention" => format!(
                "conde: you were mentioned in {place} by {} (message {}). Read it with: conde read '{place}'{}",
                m.sender,
                m.id,
                if member_seq.is_some() { String::new() } else { format!(" after joining: conde join '{place}'") }
            ),
            _ => format!("conde: {} posted in {place} (message {}). Read it with: conde read '{place}'", m.sender, m.id),
        };
        let badges = unread_badges(ctx, &rcpt.id)?;
        if !badges.is_empty() {
            text.push_str(&format!(" | Also unread: {}", badges.join(", ")));
        }
        // v2: the daemon renders the text from the fields with its alias for the board.
        let text = if v2 { String::new() } else { text };
        let attempt_id = new_ulid();
        ctx.db.insert_attempt(&attempt_id, &notification_id, me, rcpt.binding_version, &ctx.p.host, &text)?;
        set_status("dispatching")?;
        let mut j = job(
            &attempt_id,
            &notification_id,
            &address.to_string(),
            &text,
            &rcpt.canonical(),
            &m.id,
            rcpt.binding_version,
        );
        if v2 {
            j["fields"] = fields(ctx, &kind, &m, &rcpt.id, member_seq.is_some())?;
        }
        jobs.push(j);
    }

    let held: Vec<Value> = ctx
        .db
        .held_attempts(me)?
        .into_iter()
        .map(|(attempt_id, address)| json!({"attempt_id": attempt_id, "address": address}))
        .collect();
    let mut out = json!({"jobs": jobs, "held": held});
    if body.get("open").and_then(Value::as_bool) == Some(true) {
        out["open"] = Value::Array(open_attempts(ctx, me, v2)?);
    }
    if v2 {
        out["routes"] = json!(super::mailbox::routes(ctx)?);
        out["routes_revision"] = json!(super::mailbox::revision(ctx)?);
    }
    Ok(out)
}

/// The structured fields of a v2 notify (DESIGN-v2.md §4): `kind`, `sender`, `place`, the
/// recipient's unread channels as `badges`, and whether it is a `member` of the place.
fn fields(ctx: &Ctx, kind: &str, m: &super::messages::MsgRow, rcpt: &str, member: bool) -> OpResult<Value> {
    let badges: Vec<Value> = ctx
        .db
        .unread_badges(rcpt)?
        .into_iter()
        .map(|(channel, unread)| json!({"place": format!("#{channel}"), "unread": unread}))
        .collect();
    Ok(json!({
        "kind": kind, "sender": m.sender, "place": m.place(), "badges": badges,
        "member": m.channel_id.is_none() || member,
    }))
}

/// Every attempt this instance claimed that no outcome settled yet (in flight or
/// held): what a reconnecting bridge must be offered again, whatever its window.
fn open_attempts(ctx: &Ctx, me: &str, v2: bool) -> OpResult<Vec<Value>> {
    let mut out = Vec::new();
    for a in ctx.db.open_attempts(me)? {
        let mut j =
            job(&a.attempt_id, &a.notification_id, &a.address, &a.text, &a.recipient, &a.message_id, a.binding_version);
        if v2 && let Some(m) = msg_by_id(ctx.db, &a.message_id)? {
            let member = match &m.channel_id {
                Some(c) => channels::read_seq(ctx.db, c, &a.recipient_id)?.is_some(),
                None => true,
            };
            j["fields"] = fields(ctx, &a.kind, &m, &a.recipient_id, member)?;
        }
        out.push(j);
    }
    Ok(out)
}

/// `bridge.fence`: make this instance the host's one active bridge. A newer
/// instance fences every older one; a fenced instance never comes back. On a shared board the
/// hello's `proto` must match the host: v2 once it migrated, v1 until then (`refused`).
pub fn fence(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = instance(ctx)?;
    if ctx.board.is_some() {
        let proto = opt_i64(body, "proto")?.unwrap_or(1);
        let refused = match (proto, ctx.mailbox_only) {
            (1, true) => Some(format!("upgrade conde: host '{}' migrated to v2 and takes no v1 bridge", ctx.p.host)),
            (2, false) => Some(format!(
                "host '{}' has not migrated to v2 on this board; its human runs conde setup before a v2 bridge connects",
                ctx.p.host
            )),
            (1 | 2, _) => None,
            (other, _) => Some(format!("unsupported bridge protocol {other}")),
        };
        if let Some(why) = refused {
            return Ok(json!({"active": false, "refused": why}));
        }
    }
    let own = ctx.db.bridge_instance(me)?;
    if let Some((host, state)) = &own
        && (state == "fenced" || *host != ctx.p.host)
    {
        return Ok(json!({"active": false}));
    }
    let fenced = ctx.db.fence(&ctx.p.host, me, &ctx.now_iso())?;
    ctx.db.announce(topics::FLEET);
    Ok(json!({"active": true, "fenced": fenced}))
}

/// `bridge.endpoints`: the bridge's full set of live endpoints: harness endpoints from a v1
/// host, mailbox endpoints from a host migrated to v2. Holds on a Claude session or a mailbox
/// that is no longer live are abandoned and re-attempted (Codex threads are not enumerable, so
/// their holds are left alone).
pub fn endpoints(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = instance(ctx)?;
    ensure_not_fenced(ctx, me)?;
    let list = body
        .get("endpoints")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("missing array field 'endpoints'"))?;
    let mut live: Vec<(&str, &str)> = Vec::with_capacity(list.len());
    for e in list {
        let (kind, target) = (str_param(e, "kind")?, str_param(e, "target")?);
        match Kind::parse(kind) {
            Some(Kind::Mailbox) if ctx.mailbox_only => {}
            Some(Kind::Claude | Kind::Codex) if ctx.mailbox_only => {
                return Err(forbidden(format!(
                    "host '{}' migrated to v2 and reports mailbox endpoints only, never sessions",
                    ctx.p.host
                )));
            }
            Some(k) if k != Kind::Mailbox && !ctx.mailbox_only => {}
            _ => return Err(bad_request(format!("unknown endpoint kind '{kind}'"))),
        }
        live.push((kind, target));
    }
    ctx.db.replace_endpoints(&ctx.p.host, me, &live, &ctx.now_iso())?;
    ctx.db.announce(topics::FLEET);
    let gone = ctx.db.gone_holds(me, &ctx.p.host)?;
    let why = if ctx.mailbox_only { "recipient offline" } else { "recipient session gone" };
    for attempt_id in &gone {
        apply(ctx, attempt_id, "abandoned", Some(why), None)?;
    }
    Ok(json!({"endpoints": list.len(), "abandoned": gone}))
}

/// Holds, in-flight dispatches and provisional acceptances can only settle
/// through the bridge instance that sent them (its receipts reach no other
/// process), to the incarnation they targeted; otherwise they are abandoned
/// and re-attempted at once. A provisional acceptance counts only while it is
/// the notification's standing state; confirmed deliveries are never touched.
fn abandon_stale(ctx: &Ctx, me: &str) -> OpResult<()> {
    let stale = ctx.db.stale_attempts(me, &ctx.p.host)?;
    for (attempt_id, reason) in stale {
        apply(ctx, &attempt_id, "abandoned", Some(&reason), None)?;
    }
    Ok(())
}

fn unread_badges(ctx: &Ctx, agent_id: &str) -> OpResult<Vec<String>> {
    Ok(ctx.db.unread_badges(agent_id)?.into_iter().map(|(channel, n)| format!("#{channel} {n}")).collect())
}

/// `bridge.report`: an outcome for one attempt, from the bridge that claimed
/// it. `event_seq` (remote bridges) makes a resent outcome a no-op.
pub fn report(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = instance(ctx)?;
    let attempt_id = str_param(body, "attempt_id")?;
    let outcome = str_param(body, "outcome")?;
    if !["accepted", "held"].contains(&outcome) && !TERMINAL.contains(&outcome) {
        return Err(bad_request(format!("unknown outcome '{outcome}'")));
    }
    // A host migrated to v2 reports the closed set of reasons only (DESIGN-v2.md §5).
    if ctx.mailbox_only
        && let Some(reason) = opt_str(body, "reason")?
        && OutcomeReason::parse(reason).is_none()
    {
        return Err(bad_request(format!("unknown outcome reason '{reason}'")));
    }
    let event_seq = opt_i64(body, "event_seq")?;
    let owner = ctx.db.attempt_owner(attempt_id)?;
    let Some((claimed_by, claimed_on, last_seq)) = owner else {
        return Ok(json!({"attempt_id": attempt_id, "ignored": "unknown attempt"}));
    };
    check_owner(ctx, me, attempt_id, &claimed_by, claimed_on.as_deref())?;
    if event_seq.is_some_and(|seq| seq <= last_seq) {
        return Ok(json!({"attempt_id": attempt_id, "duplicate": true}));
    }
    // An outcome at a binding the recipient has left (a takeover, an unpublish, a re-register)
    // never ends the notification: the attempt is abandoned and re-attempted at the current
    // route, as the stale-attempt rule would at the next claim.
    if ctx.db.attempt_route(attempt_id)?.is_some_and(|(target, current)| target != current) {
        return apply(ctx, attempt_id, "abandoned", Some("recipient rebound"), event_seq);
    }
    apply(ctx, attempt_id, outcome, opt_str(body, "reason")?, event_seq)
}

/// The one ownership rule for outcomes, local and remote: the reporting
/// connection's host and bridge instance must be the attempt's.
fn check_owner(ctx: &Ctx, me: &str, attempt_id: &str, instance: &str, host: Option<&str>) -> OpResult<()> {
    if instance != me || host.is_some_and(|h| h != ctx.p.host) {
        return Err(forbidden(format!(
            "attempt {attempt_id} belongs to bridge {instance} on host {}; this connection is bridge {me} on host {}",
            host.unwrap_or("?"),
            ctx.p.host
        )));
    }
    Ok(())
}

fn apply(ctx: &Ctx, attempt_id: &str, outcome: &str, reason: Option<&str>, event_seq: Option<i64>) -> OpResult<Value> {
    let Some((notification_id, status)) = ctx.db.attempt(attempt_id)? else {
        return Ok(json!({"attempt_id": attempt_id, "ignored": "unknown attempt"}));
    };
    if TERMINAL.contains(&status.as_str()) {
        return Ok(json!({"attempt_id": attempt_id, "ignored": format!("attempt already settled as {status}")}));
    }
    if outcome == "dropped" && reason == Some("duplicate") {
        // The earlier frame with this id is still pending at the recipient.
        return Ok(json!({"attempt_id": attempt_id, "ignored": "duplicate"}));
    }
    ctx.db.update_attempt(attempt_id, outcome, reason, event_seq, &ctx.now_iso())?;
    ctx.db.announce(topics::DELIVERY | topics::FLEET);
    let current = ctx.db.latest_attempt(&notification_id)?.as_deref() == Some(attempt_id);
    if current {
        let (status, next) = match outcome {
            "accepted" => ("notified", None),
            "held" => ("held", None),
            "delivered" => ("delivered", None),
            "denied" | "refused" => (outcome, None),
            "abandoned" => ("pending", Some(ctx.now_ms)),
            _ => ("pending", Some(ctx.now_ms + ctx.retry_ms)),
        };
        ctx.db.settle_notification(&notification_id, status, next)?;
    }
    Ok(json!({"attempt_id": attempt_id, "notification_id": notification_id, "status": outcome, "current": current}))
}
