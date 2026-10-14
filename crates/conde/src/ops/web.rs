//! The web UI's reads: what the signed-in owner's own agents can read, derived from ownership
//! and enforced in the store's SQL (an agent of the owner is a DM party or a channel member),
//! plus the fleet. Nothing here writes; nothing needs an actor.
//!
//! - `owner.list`: per agent of the owner, its DM conversations (any counterpart) and the
//!   channels it belongs to.
//! - `owner.history`: one of those conversations in full, paged, with per-message delivery
//!   state for messages the owner's agents sent (every recipient) or received (theirs).
//! - `owner.feed`: everything the owner's agents can read, newest first, between two seq bounds,
//!   by agent, direction and place.
//! - `owner.message`, `owner.attachment`: one message or attachment under the same rule.
//! - `owner.search`: `search` over the same rule (the remote MCP's search).
//! - `fleet.list`: every bound agent with host bridge state; delivery health for own agents.

use std::collections::{BTreeMap, HashMap};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use super::agents::{self, Agent};
use super::channels;
use super::messages::{self, MsgRow};
use crate::address::{Kind, split_qualified};
use crate::dispatch::{Ctx, OpResult, bad_request, forbidden, not_found, opt_i64, opt_str, str_param};
use crate::protocol::PAGE;
use crate::store::{Delivery, FeedBy, FeedPosts, FeedRange, Party, Tx};
use crate::util::{iso_from_ms, is_ulid};

/// A bridge is online while its last heartbeat is younger than this (three ping periods).
pub const ONLINE_TTL_MS: i64 = 90_000;
/// A notification still undelivered this long after its message counts as stuck.
pub const STUCK_AFTER_MS: i64 = 60_000;
/// Default page of a conversation's history.
const HISTORY_PAGE: i64 = 100;
/// Default and largest page of `owner.feed`.
const FEED_PAGE: i64 = 50;
const FEED_MAX: i64 = 100;
/// The recipient states `owner.feed {problems}` keeps (`recipient_state`).
const PROBLEMS: &[&str] = &["retrying", "held", "denied", "refused", "gone"];
/// Problem candidates read per round trip.
const PROBLEM_CHUNK: usize = 100;

/// Attempt outcomes that end an attempt without reaching the recipient.
const FAILED: &[&str] = &["expired", "dropped", "failed", "abandoned", "denied", "refused"];

/// The owner view is the signed-in human's, never an agent's: an agent reads its own
/// conversations only (DESIGN §3). Only the browser principal passes; an actor never does.
fn browser_only(ctx: &Ctx) -> OpResult<()> {
    if !ctx.p.browser || ctx.actor.is_some() {
        return Err(forbidden("the owner view is for the signed-in owner in the browser, not for agents"));
    }
    Ok(())
}

/// `owner.list`: every agent of the owner with its DMs and channels.
pub fn list(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    let owner = ctx.p.user_id;
    let mut agents: BTreeMap<String, Value> = BTreeMap::new();
    for a in ctx.db.owner_agents(owner)? {
        agents.insert(
            a.id.clone(),
            json!({"name": a.canonical(), "kind": a.address.as_ref().map(|x| x.kind.as_str()),
                   "address": a.address.as_ref().map(ToString::to_string), "dms": [], "channels": []}),
        );
    }
    for t in ctx.db.owner_dm_threads(owner)? {
        if let Some(Value::Array(dms)) = agents.get_mut(&t.agent_id).map(|a| &mut a["dms"]) {
            dms.push(json!({"with": t.partner, "last_seq": t.last_seq, "last_at": t.last_at, "unread": t.unread}));
        }
    }
    for c in ctx.db.owner_channel_threads(owner)? {
        if let Some(Value::Array(chs)) = agents.get_mut(&c.agent_id).map(|a| &mut a["channels"]) {
            chs.push(json!({"channel": format!("#{}", c.name), "topic": c.topic, "last_seq": c.last_seq,
                            "last_at": c.last_at, "unread": c.unread}));
        }
    }
    let mut agents: Vec<Value> = agents.into_values().collect();
    agents.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(json!({"owner": ctx.p.user, "agents": agents}))
}

/// The owner's agent a request names (`"agent": "name"` or `"owner/name"` in the owner's namespace).
fn own_agent(ctx: &Ctx, body: &Value) -> OpResult<Agent> {
    let raw = str_param(body, "agent")?;
    let (owner, name) = split_qualified(raw).ok_or_else(|| bad_request(format!("invalid name '{raw}'")))?;
    if owner.as_deref().is_some_and(|o| o != ctx.p.user) {
        return Err(forbidden(format!("'{raw}' is not one of your agents")));
    }
    agents::by_name(ctx.db, ctx.p.user_id, &name)?.ok_or_else(|| not_found(format!("you have no agent named '{name}'")))
}

/// `owner.history`: `{"agent", "with"}` (a DM thread) or `{"agent", "channel"}` (a channel the
/// agent belongs to), oldest first, a page before `before`.
pub fn history(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    let owner = ctx.p.user_id;
    let agent = own_agent(ctx, body)?;
    let before = opt_i64(body, "before")?.unwrap_or(i64::MAX);
    let limit = opt_i64(body, "limit")?.unwrap_or(HISTORY_PAGE).clamp(1, PAGE as i64);
    let (mut rows, mut head) = if let Some(raw) = opt_str(body, "channel")? {
        let channel = channels::member_channel(ctx, &agent, raw)?;
        let rows = ctx.db.owner_channel_history(owner, &channel.id, before, limit + 1)?;
        (rows, json!({"channel": format!("#{}", channel.name), "topic": channel.topic}))
    } else if let Some(name) = opt_str(body, "with")? {
        let partner = agents::find(ctx, name)?.ok_or_else(|| not_found(format!("no agent named '{name}'")))?;
        let rows = ctx.db.owner_dm_history(owner, &agent.id, &partner.id, before, limit + 1)?;
        let kind = partner.address.as_ref().map(|a| a.kind.as_str());
        (rows, json!({"with": partner.canonical(), "kind": kind}))
    } else {
        return Err(bad_request("name the conversation: {\"agent\", \"with\": \"owner/name\"} or {\"agent\", \"channel\": \"#name\"}"));
    };
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    rows.reverse();
    let mut out = Vec::with_capacity(rows.len());
    for m in &rows {
        let mut v = messages::view(ctx.db, m)?;
        v["delivery"] = delivery(ctx, m)?;
        out.push(v);
    }
    head["agent"] = json!(agent.canonical());
    head["messages"] = Value::Array(out);
    head["more"] = json!(more);
    Ok(head)
}

/// `owner.feed {after?, before?, limit?, agent?, direction?, place?, problems?}`: what the owner's agents can
/// read with `after < seq < before` (both exclusive, both optional), newest first, at most
/// `limit` (1 to 100, default 50); `more` when matching rows remain below the last one, inside the
/// bounds. `agent`: the own agent is the sender, the DM recipient or a current member of the
/// channel. `direction`: `out` (an own agent posted, or sent a DM to another owner's agent),
/// `own` (a DM between two own agents), `in` (the rest). `place`: `dm`, or one `#channel`. Each
/// row is the history view with `direction` and `via` (the own agents that can read it).
/// `problems: true` keeps the messages where a recipient the delivery view shows the owner (every
/// recipient of an own sender's message, else the own ones) is retrying, held, denied, refused or
/// gone; it reads every candidate in the bounds (no scan budget), so an empty page with `more:
/// false` means none.
///
/// Read as index ranges, never a walk of the board: each own agent as sender and as DM recipient,
/// each channel with an own member, every range newest first and cut at the page, merged without
/// duplicates. Together they are exactly what the owner's agents can read.
pub fn feed(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    let owner = ctx.p.user_id;
    let after = opt_i64(body, "after")?.unwrap_or(0);
    let before = opt_i64(body, "before")?.unwrap_or(i64::MAX);
    let limit = opt_i64(body, "limit")?.unwrap_or(FEED_PAGE).clamp(1, FEED_MAX);
    let agent = match opt_str(body, "agent")? {
        Some(_) => Some(own_agent(ctx, body)?),
        None => None,
    };
    let direction = opt_str(body, "direction")?;
    let problems = match body.get("problems") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad_request("problems is a boolean")),
    };
    if direction.is_some_and(|d| !["in", "out", "own"].contains(&d)) {
        return Err(bad_request("direction is one of in, out, own"));
    }
    let memberships = ctx.db.owner_memberships(owner)?;
    let mut owned_channels: Vec<String> = memberships.iter().map(|m| m.channel_id.clone()).collect();
    owned_channels.sort();
    owned_channels.dedup();
    // place: DMs only, one channel an own agent is in, or both
    let (dms_wanted, place_channel) = match opt_str(body, "place")? {
        None => (true, None),
        Some("dm") => (true, Some(None)),
        Some(raw) => {
            let name = crate::address::normalize_channel(raw).ok_or_else(|| bad_request(format!("invalid place '{raw}'")))?;
            let m = memberships
                .iter()
                .find(|m| m.channel == name)
                .ok_or_else(|| forbidden(format!("none of your agents is in #{name}")))?;
            (false, Some(Some(m.channel_id.clone())))
        }
    };
    let in_place = |c: &String| place_channel.as_ref().is_none_or(|p| p.as_ref() == Some(c));
    let own_ids: Vec<String> = ctx.db.owner_agents(owner)?.into_iter().map(|a| a.id).collect();
    let picked = |id: &String| agent.as_ref().is_none_or(|a| &a.id == id);
    let owned_here: Vec<String> = owned_channels.iter().filter(|c| in_place(c)).cloned().collect();
    // the channels the rows may be in: the agent's (it is a current member), else every own one
    let channels: Vec<String> = match &agent {
        Some(a) => memberships.iter().filter(|m| m.agent_id == a.id && in_place(&m.channel_id)).map(|m| m.channel_id.clone()).collect(),
        None => owned_here.clone(),
    };
    let mut ranges = Vec::new();
    let mut range = |by: FeedBy, id: &String, dms: Option<Party>, posts: Option<FeedPosts>| {
        ranges.push(FeedRange { by, id: id.clone(), dms: dms.filter(|_| dms_wanted), posts });
    };
    if direction == Some("out") {
        // Every `out` row has an own sender: the own senders' ranges alone, never a channel's
        // range filtered down to its few own posts. The named agent's own posts reach every
        // channel an own agent is in (it may have left one); the others', the agent's channels.
        for id in &own_ids {
            let posts = if agent.is_some() && picked(id) { &owned_here } else { &channels };
            range(FeedBy::Sender, id, picked(id).then_some(Party::Other), Some(FeedPosts::In(posts.clone())));
        }
    } else {
        // The party at the other end of a DM, from a sender range and from a recipient range,
        // and the senders a channel range keeps.
        let (sent_to, received_from, posted_by) = match direction {
            None => (Some(Party::Any), Some(Party::Any), Some(Party::Any)),
            Some("own") => (Some(Party::Own), Some(Party::Own), None),
            _ => (None, Some(Party::Other), Some(Party::Other)),
        };
        // An agent's own posts in a channel it left are still readable through another own member.
        let left_posts = (agent.is_some() && direction.is_none()).then(|| FeedPosts::In(owned_here.clone()));
        for id in own_ids.iter().filter(|id| picked(id)) {
            range(FeedBy::Sender, id, sent_to, left_posts.clone());
            range(FeedBy::Recipient, id, received_from, None);
        }
        if let Some(p) = posted_by {
            for c in &channels {
                range(FeedBy::Channel, c, None, Some(FeedPosts::By(p)));
            }
        }
    }
    let direction_of = |m: &MsgRow| {
        let (from_own, to_own) = (owned(ctx, &m.sender), m.recipient.as_deref().is_some_and(|r| owned(ctx, r)));
        match (&m.channel_id, from_own, to_own) {
            (None, true, true) => "own",
            (_, true, _) => "out",
            _ => "in",
        }
    };
    // (row, its delivery view when already computed)
    let mut rows: Vec<(MsgRow, Option<Value>)> = Vec::new();
    if after.saturating_add(1) >= before {
    } else if problems {
        // Candidates by notification status, then the exact state each recipient shows; the
        // other predicates are checked here, on the candidates only.
        let keeps = |m: &MsgRow| {
            let in_place = match &place_channel {
                None => true,
                Some(None) => m.channel_id.is_none(),
                Some(Some(c)) => m.channel_id.as_ref() == Some(c),
            };
            let by_agent = agent.as_ref().is_none_or(|a| {
                m.sender_id == a.id
                    || m.recipient_id.as_ref() == Some(&a.id)
                    || m.channel_id.as_ref().is_some_and(|c| memberships.iter().any(|x| x.agent_id == a.id && &x.channel_id == c))
            });
            in_place && by_agent && direction.is_none_or(|d| d == direction_of(m))
        };
        let seqs = ctx.db.owner_problem_seqs(owner, after, before)?;
        'scan: for chunk in seqs.chunks(PROBLEM_CHUNK) {
            for m in ctx.db.messages_at(chunk)? {
                if !keeps(&m) {
                    continue;
                }
                let d = delivery(ctx, &m)?;
                let problem = d["recipients"].as_array().is_some_and(|rs| rs.iter().any(|r| PROBLEMS.contains(&r["state"].as_str().unwrap_or(""))));
                if problem {
                    rows.push((m, Some(d)));
                    if rows.len() as i64 > limit {
                        break 'scan;
                    }
                }
            }
        }
    } else {
        rows = ctx.db.owner_feed(owner, &ranges, after, before, limit + 1)?.into_iter().map(|m| (m, None)).collect();
    }
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let mut out = Vec::with_capacity(rows.len());
    for (m, d) in rows {
        let m = &m;
        let (from_own, to_own) = (owned(ctx, &m.sender), m.recipient.as_deref().is_some_and(|r| owned(ctx, r)));
        let direction = direction_of(m);
        let via: Vec<&str> = match &m.channel_id {
            Some(c) => memberships.iter().filter(|x| &x.channel_id == c).map(|x| x.agent.as_str()).collect(),
            None => [from_own.then_some(m.sender.as_str()), m.recipient.as_deref().filter(|_| to_own)]
                .into_iter()
                .flatten()
                .collect(),
        };
        let mut v = messages::view(ctx.db, m)?;
        v["delivery"] = match d {
            Some(d) => d,
            None => delivery(ctx, m)?,
        };
        v["direction"] = json!(direction);
        v["via"] = json!(via);
        out.push(v);
    }
    Ok(json!({"messages": out, "more": more}))
}

/// `owner.message`: one message an agent of the owner can read.
pub fn message(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    let id = str_param(body, "id")?;
    let m = visible(ctx, id)?;
    let mut v = messages::view(ctx.db, &m)?;
    v["delivery"] = delivery(ctx, &m)?;
    Ok(json!({"message": v}))
}

/// `owner.search {pattern, target?, after?}`: a regex over what the owner's agents can read,
/// optionally one `#channel` or the DMs with one partner, oldest first, paged as `search`.
pub fn search(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    let owner = ctx.p.user_id;
    messages::search_in(ctx, body, |after, scope, from_day| {
        Ok(ctx.db.owner_search_scan(owner, after, scope, from_day, messages::SEARCH_BATCH)?)
    })
}

/// `owner.attachment`: one attachment of a message an agent of the owner can read.
pub fn attachment(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    browser_only(ctx)?;
    let id = str_param(body, "attachment_id")?;
    let missing = || not_found(format!("no attachment '{id}' visible to your agents"));
    let att = ctx.db.attachment(id)?.ok_or_else(missing)?;
    visible(ctx, &att.message_id).map_err(|_| missing())?;
    Ok(json!({"attachment_id": id, "message_id": att.message_id, "filename": att.filename,
              "size": att.bytes.len(), "data": B64.encode(&att.bytes)}))
}

fn visible(ctx: &Ctx, id: &str) -> OpResult<MsgRow> {
    let missing = || not_found(format!("no message '{id}' visible to your agents"));
    if !is_ulid(id) {
        return Err(missing());
    }
    ctx.db.owner_message(ctx.p.user_id, id)?.ok_or_else(missing)
}

fn owned(ctx: &Ctx, canonical: &str) -> bool {
    canonical.split_once('/').is_some_and(|(owner, _)| owner == ctx.p.user)
}

/// Per-recipient delivery state of `m`: every recipient when one of the owner's agents sent
/// it, else the notifications of the owner's agents. Channel posts by the owner's agents also
/// carry how many members have read past them.
fn delivery(ctx: &Ctx, m: &MsgRow) -> OpResult<Value> {
    let own = owned(ctx, &m.sender);
    let mut recipients = Vec::new();
    let mut seen = Vec::new();
    for d in ctx.db.deliveries(&m.id)? {
        if !own && !owned(ctx, &d.recipient) {
            continue;
        }
        seen.push(d.recipient_id.clone());
        recipients.push(recipient_state(ctx.db, m, &d)?);
    }
    // A DM without a notification (imported history): its read state alone.
    if let (None, Some(rcpt), Some(name)) = (&m.channel_id, &m.recipient_id, &m.recipient)
        && (own || owned(ctx, name))
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
    // (a bridge marks it so; a human on a host without a bridge stays pending).
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
    // A reached message has no delivery error: a later attempt failing under it (abandoned at
    // a host takeover) does not undo the read or the delivery.
    let last_error = match (&d.last_status, &d.last_reason) {
        _ if matches!(state, "read" | "delivered") => Value::Null,
        (Some(s), reason) if FAILED.contains(&s.as_str()) => json!({"status": s, "reason": reason}),
        _ => Value::Null,
    };
    Ok(json!({
        "recipient": d.recipient, "kind": d.kind, "state": state, "status": d.status,
        "attempts": d.attempts, "retry_at": retry_at, "last_error": last_error, "read_at": read_at,
    }))
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
    // Reached: delivered, or accepted on a codex row; a mailbox row counts delivered only, which
    // is what a confirmed Codex reach is normalized to (the rule lives in `agent_health`).
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
        // A shared board lists kind and host, never a target (DESIGN-v2.md §6).
        let (shown_address, pinned) = match ctx.board {
            Some(_) => (Value::Null, Value::Null),
            None => (json!(address.to_string()), json!(a.pinned)),
        };
        agents.push(json!({
            "name": a.canonical(), "owner": a.owner, "display": a.display, "kind": address.kind.as_str(),
            "host": address.host, "address": shown_address, "pinned": pinned,
            "discovered": discovered,
            "held": own.then(|| h.map_or(0, |h| h.held)),
            "stuck": own.then(|| h.map_or(0, |h| h.stuck)),
            "last_reached": if own { json!(reached) } else { Value::Null },
            // Reached at a time not recorded (before migration 5, or an older writer): never a
            // made-up time, and never "never".
            "ever_reached": own.then(|| h.is_some_and(|h| h.reached)),
            "own": own,
        }));
    }
    Ok(json!({
        "now": iso_from_ms(now), "online_ttl_s": ONLINE_TTL_MS / 1000,
        "hosts": hosts.into_values().collect::<Vec<_>>(), "agents": agents,
    }))
}
