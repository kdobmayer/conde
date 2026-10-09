//! Messages: send (with notification intents), inbox, read, reactions,
//! search, receipts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};

use super::agents::{self, Agent};
use super::channels::{self, Channel};
use crate::address::normalize_channel;
use crate::dispatch::{
    Ctx, OpResult, bad_request, forbidden, not_found, one_line, opt_i64, opt_str, str_param, too_large,
};
use crate::protocol::{MAX_ATTACHMENT, MAX_MESSAGE_ATTACHMENTS, MAX_TEXT, PAGE};
use crate::store::{NewMessage, NewNotification, SearchScope, Tx};
use crate::util::{is_ulid, new_ulid};

static MENTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@([A-Za-z0-9_](?:[A-Za-z0-9_./\-]*[A-Za-z0-9_])?)").expect("mention regex")
});

/// Lowercased `@name` / `@owner/name` tokens in a body.
pub fn mentions_in(text: &str) -> BTreeSet<String> {
    MENTION.captures_iter(text).map(|c| c[1].to_lowercase()).collect()
}

pub enum Target<'a> {
    Agent(&'a Agent),
    Channel(&'a Channel),
}

pub struct NewAttachment {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// Commit a message plus its attachments and notification intents.
/// DMs and @mentions push; plain channel posts only badge; a human's channel
/// post pushes to every member; the sender is never notified.
pub fn insert_message(
    ctx: &Ctx,
    sender: &Agent,
    target: Target,
    text: &str,
    reply_to: Option<&str>,
    attachments: Vec<NewAttachment>,
) -> OpResult<(String, i64)> {
    let id = new_ulid();
    let (recipient, channel) = match target {
        Target::Agent(a) => (Some(a.id.as_str()), None),
        Target::Channel(c) => (None, Some(c.id.as_str())),
    };
    let created_at = ctx.now_iso();
    let seq = ctx.db.insert_message(&NewMessage {
        id: &id,
        sender_id: &sender.id,
        recipient_id: recipient,
        channel_id: channel,
        text,
        reply_to,
        created_at: &created_at,
    })?;
    for a in attachments {
        ctx.db.insert_attachment(&new_ulid(), &id, &a.filename, &a.bytes)?;
    }
    let mut push: BTreeMap<String, &'static str> = BTreeMap::new();
    match target {
        Target::Agent(r) => {
            push.insert(r.id.clone(), "dm");
        }
        Target::Channel(c) => {
            let members = channels::member_ids(ctx.db, &c.id)?;
            if sender.is_human() {
                push.extend(members.iter().map(|m| (m.clone(), "post")));
            }
            for m in mentions_in(text) {
                if m == "everybody" {
                    push.extend(members.iter().map(|m| (m.clone(), "mention")));
                } else if let Some(agent) = mentioned_agent(ctx, sender, &m)? {
                    push.insert(agent.id, "mention");
                }
            }
        }
    }
    push.remove(&sender.id);
    for (rcpt, kind) in push {
        let Some(agent) = agents::by_id(ctx.db, &rcpt)? else { continue };
        ctx.db.insert_notification(&NewNotification {
            id: &format!("{id}.{rcpt}"),
            message_id: &id,
            recipient_id: &rcpt,
            kind,
            binding_version: agent.binding_version,
            next_attempt_at: ctx.now_ms,
        })?;
    }
    Ok((id, seq))
}

/// A mention resolves in the sender's namespace unless qualified; only
/// registered names count.
fn mentioned_agent(ctx: &Ctx, sender: &Agent, token: &str) -> OpResult<Option<Agent>> {
    let qualified = if token.contains('/') { token.to_string() } else { format!("{}/{token}", sender.owner) };
    Ok(agents::find(ctx, &qualified).unwrap_or(None).filter(|a| a.address.is_some()))
}

// --- message rows -----------------------------------------------------------

pub use crate::store::Message as MsgRow;

pub fn msg_by_id(db: &dyn Tx, id: &str) -> OpResult<Option<MsgRow>> {
    Ok(db.message_by_id(id)?)
}

/// DM parties and channel members see a message; nobody else does.
fn visible(db: &dyn Tx, me: &Agent, m: &MsgRow) -> OpResult<bool> {
    Ok(match &m.channel_id {
        Some(c) => channels::read_seq(db, c, &me.id)?.is_some(),
        None => m.sender_id == me.id || m.recipient_id.as_deref() == Some(me.id.as_str()),
    })
}

/// A message by id, or not_found when it does not exist for `me`.
fn visible_msg(ctx: &Ctx, me: &Agent, id: &str) -> OpResult<MsgRow> {
    let missing = || not_found(format!("no message '{id}' visible to you"));
    if !is_ulid(id) {
        return Err(missing());
    }
    let m = msg_by_id(ctx.db, id)?.ok_or_else(missing)?;
    if !visible(ctx.db, me, &m)? {
        return Err(missing());
    }
    Ok(m)
}

pub(crate) fn view(db: &dyn Tx, m: &MsgRow) -> OpResult<Value> {
    let attachments: Vec<Value> = db
        .attachment_list(&m.id)?
        .into_iter()
        .map(|a| json!({"attachment_id": a.id, "filename": a.filename, "size": a.size}))
        .collect();
    let mut reactions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (emoji, who) in db.reactions(&m.id)? {
        reactions.entry(emoji).or_default().push(who);
    }
    let reactions: Vec<Value> =
        reactions.into_iter().map(|(emoji, by)| json!({"emoji": emoji, "by": by})).collect();
    Ok(json!({
        "message_id": m.id, "seq": m.seq, "place": m.place(), "from": m.sender, "to": m.to(),
        "date": m.created_at, "reply_to": m.reply_to, "text": m.text,
        "attachments": attachments, "reactions": reactions,
    }))
}

// --- ops ----------------------------------------------------------------------

pub fn send(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let text = str_param(body, "text")?;
    if text.len() > MAX_TEXT {
        return Err(too_large(format!("text is {} bytes; the limit is {MAX_TEXT}", text.len())));
    }
    let reply_to = opt_str(body, "reply_to")?;
    if let Some(r) = reply_to {
        visible_msg(ctx, &me, r)?;
    }
    let attachments = decode_attachments(body)?;
    let to = body.get("to").ok_or_else(|| bad_request("missing field 'to'"))?;
    let (id, seq, to_name) = if let Some(name) = to.get("agent").and_then(Value::as_str) {
        let rcpt = agents::find(ctx, name)?
            .ok_or_else(|| not_found(format!("no agent registered as '{name}'")))?;
        let (id, seq) = insert_message(ctx, &me, Target::Agent(&rcpt), text, reply_to, attachments)?;
        (id, seq, rcpt.canonical())
    } else if let Some(raw) = to.get("channel").and_then(Value::as_str) {
        let channel = channels::member_channel(ctx, &me, raw)?;
        let (id, seq) = insert_message(ctx, &me, Target::Channel(&channel), text, reply_to, attachments)?;
        (id, seq, format!("#{}", channel.name))
    } else {
        return Err(bad_request("'to' must be {\"agent\": name} or {\"channel\": \"#name\"}"));
    };
    Ok(json!({"message_id": id, "seq": seq, "to": to_name}))
}

fn decode_attachments(body: &Value) -> OpResult<Vec<NewAttachment>> {
    let Some(list) = body.get("attachments").filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let list = list.as_array().ok_or_else(|| bad_request("'attachments' must be a list"))?;
    let mut out: Vec<NewAttachment> = Vec::new();
    let mut total = 0usize;
    for item in list {
        let name = str_param(item, "filename")?;
        let base = name.rsplit('/').next().unwrap_or_default().trim();
        if base.is_empty() || base == "." || base == ".." || base.len() > 255 {
            return Err(bad_request(format!("bad attachment filename '{name}'")));
        }
        one_line(base, "attachment filename")?;
        let bytes = B64
            .decode(str_param(item, "data")?)
            .map_err(|e| bad_request(format!("attachment '{base}': bad base64: {e}")))?;
        if bytes.len() > MAX_ATTACHMENT {
            return Err(too_large(format!("attachment '{base}' is {} bytes; the limit is {MAX_ATTACHMENT}", bytes.len())));
        }
        total += bytes.len();
        if total > MAX_MESSAGE_ATTACHMENTS {
            return Err(too_large(format!("attachments exceed {MAX_MESSAGE_ATTACHMENTS} bytes per message")));
        }
        let mut filename = base.to_string();
        let mut n = 1;
        while out.iter().any(|a| a.filename == filename) {
            n += 1;
            filename = match base.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{n}.{ext}"),
                _ => format!("{base}-{n}"),
            };
        }
        out.push(NewAttachment { filename, bytes });
    }
    Ok(out)
}

/// Unread DMs plus unread channel posts by others, oldest first, paged.
pub fn inbox(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let after = opt_i64(body, "after")?.unwrap_or(0);
    let rows = ctx.db.inbox(&me.id, after, PAGE as i64 + 1)?;
    let (items, next) = page(rows);
    let items: Vec<Value> = items
        .iter()
        .map(|m| json!({"kind": m.place(), "message_id": m.id, "seq": m.seq, "from": m.sender,
                        "date": m.created_at, "preview": m.preview()}))
        .collect();
    Ok(json!({"items": items, "next_cursor": next}))
}

fn page(mut rows: Vec<MsgRow>) -> (Vec<MsgRow>, Option<i64>) {
    if rows.len() > PAGE {
        rows.truncate(PAGE);
        let next = rows.last().map(|m| m.seq);
        (rows, next)
    } else {
        (rows, None)
    }
}

fn mark_read(ctx: &Ctx, me: &Agent, m: &MsgRow) -> OpResult<()> {
    match &m.channel_id {
        Some(c) => channels::advance_cursor(ctx.db, c, &me.id, m.seq),
        None if m.recipient_id.as_deref() == Some(me.id.as_str()) => {
            ctx.db.mark_dm_read(&m.id, &me.id, &ctx.now_iso())?;
            Ok(())
        }
        None => Ok(()),
    }
}

/// Return and mark read: one message by id, one channel's unread, or all.
pub fn read(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let rows = if let Some(id) = opt_str(body, "id")? {
        vec![visible_msg(ctx, &me, id)?]
    } else if let Some(raw) = opt_str(body, "channel")? {
        let channel = channels::member_channel(ctx, &me, raw)?;
        ctx.db.unread_in_channel(&channel.id, &me.id, PAGE as i64 + 1)?
    } else {
        ctx.db.unread_all(&me.id, PAGE as i64 + 1)?
    };
    let (rows, next) = page(rows);
    let mut messages = Vec::with_capacity(rows.len());
    for m in &rows {
        mark_read(ctx, &me, m)?;
        messages.push(view(ctx.db, m)?);
    }
    Ok(json!({"messages": messages, "more": next.is_some()}))
}

pub fn get(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let m = visible_msg(ctx, &me, str_param(body, "id")?)?;
    Ok(json!({"message": view(ctx.db, &m)?}))
}

pub fn attachment_get(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let id = str_param(body, "attachment_id")?;
    let missing = || not_found(format!("no attachment '{id}' visible to you"));
    let att = ctx.db.attachment(id)?.ok_or_else(missing)?;
    let (message_id, filename, bytes) = (att.message_id, att.filename, att.bytes);
    visible_msg(ctx, &me, &message_id).map_err(|_| missing())?;
    Ok(json!({"attachment_id": id, "message_id": message_id, "filename": filename,
              "size": bytes.len(), "data": B64.encode(&bytes)}))
}

/// Set (not toggle) one reaction; reactions never notify.
pub fn reaction_set(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let m = visible_msg(ctx, &me, str_param(body, "message_id")?)?;
    let emoji = str_param(body, "emoji")?.trim();
    if emoji.is_empty() || emoji.chars().count() > 16 {
        return Err(bad_request("emoji must be 1-16 characters"));
    }
    one_line(emoji, "emoji")?;
    let present = body
        .get("present")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad_request("missing boolean field 'present'"))?;
    ctx.db.set_reaction(&m.id, &me.id, emoji, present)?;
    Ok(json!({"message_id": m.id, "emoji": emoji, "present": present, "place": m.place()}))
}

/// Messages a search reads per store round trip.
const SEARCH_BATCH: i64 = 500;

/// Case-insensitive regex over what the caller can see (joined channels and
/// own DMs), optionally narrowed to one channel or DM partner.
pub fn search(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let pattern = str_param(body, "pattern")?;
    let rx = RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| bad_request(format!("bad pattern: {e}")))?;
    let after = opt_i64(body, "after")?.unwrap_or(0);
    let (channel, partner);
    let scope = match opt_str(body, "target")? {
        None => SearchScope::All,
        Some(t) if t.starts_with('#') => {
            channel = normalize_channel(t).ok_or_else(|| bad_request(format!("invalid channel '{t}'")))?;
            SearchScope::Channel(&channel)
        }
        Some(t) => {
            partner = agents::find(ctx, t)?.ok_or_else(|| not_found(format!("no agent named '{t}'")))?.id;
            SearchScope::Partner(&partner)
        }
    };
    let mut hits = Vec::new();
    let mut next = None;
    let mut cursor = after;
    'scan: loop {
        let batch = ctx.db.search_scan(&me.id, cursor, scope, SEARCH_BATCH)?;
        for m in &batch {
            cursor = m.seq;
            let Some(line) = m.text.lines().find(|l| rx.is_match(l)) else { continue };
            if hits.len() == PAGE {
                next = hits.last().and_then(|h: &Value| h["seq"].as_i64());
                break 'scan;
            }
            hits.push(json!({"place": m.place(), "message_id": m.id, "seq": m.seq, "from": m.sender,
                             "date": m.created_at, "line": line.trim()}));
        }
        if (batch.len() as i64) < SEARCH_BATCH {
            break;
        }
    }
    Ok(json!({"hits": hits, "next_cursor": next}))
}

/// The caller's sent messages with read state (DM: read time; channel: how
/// many other members have read past it).
pub fn receipts(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let after = opt_i64(body, "after")?.unwrap_or(0);
    let rows = ctx.db.sent_by(&me.id, after, PAGE as i64 + 1)?;
    let (rows, next) = page(rows);
    let mut out = Vec::with_capacity(rows.len());
    for m in &rows {
        let status = match (&m.channel_id, &m.recipient_id) {
            (Some(c), _) => {
                let (read, total) = ctx.db.channel_read_count(c, &me.id, m.seq)?;
                format!("read by {read}/{total}")
            }
            (None, Some(r)) => ctx
                .db
                .dm_read_at(&m.id, r)?
                .map_or_else(|| "unread".to_string(), |at| format!("read {at}")),
            (None, None) => String::new(),
        };
        let delivery = match (&m.channel_id, &m.recipient_id) {
            (None, Some(r)) => dm_delivery(ctx, &m.id, r)?,
            _ => None,
        };
        out.push(json!({"message_id": m.id, "seq": m.seq, "to": m.to(), "status": status, "delivery": delivery}));
    }
    Ok(json!({"receipts": out, "next_cursor": next}))
}

/// The push state of a DM to its recipient: the notification's status, with
/// the last attempt's outcome while a retry is pending.
fn dm_delivery(ctx: &Ctx, message_id: &str, recipient: &str) -> OpResult<Option<String>> {
    let row = ctx.db.dm_delivery(message_id, recipient)?;
    Ok(row.map(|d| match (d.status.as_str(), d.last_attempt) {
        ("pending", Some(last)) => match d.reason {
            Some(why) => format!("retrying after {last} ({why})"),
            None => format!("retrying after {last}"),
        },
        ("dispatching", _) => "sending".to_string(),
        _ => d.status.clone(),
    }))
}

pub fn forbid_non_member(raw: &str) -> crate::dispatch::OpError {
    forbidden(format!("not a member of '{raw}' (conde join '{raw}')"))
}
