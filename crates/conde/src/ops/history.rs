//! `history.copy`: the temporary owner-only read of the v2 switch (DESIGN-v2 section 10, H3),
//! removed with the copy tool after H4. It returns whole rows (text, attachment bytes, read
//! state, reactions) for message ids that are owner-internal for the caller: a DM between two
//! of the caller's agents, or a post in a channel whose every sender ever and every current
//! member is the caller's. Any other id refuses the whole request. Whether an id is in the kond
//! snapshot (class a) is not known here; the copy tool checks it against the frozen manifest.

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use crate::dispatch::{Ctx, OpResult, bad_request, forbidden, not_found};
use crate::store::{Message, Tx};
use crate::util::sha256_hex;

/// Ids per request: the answer carries attachment bytes inline.
pub const MAX_IDS: usize = 100;

pub fn copy(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    if ctx.p.browser || ctx.p.bridge_instance.is_some() {
        return Err(forbidden("history.copy is for the owner's host credential only"));
    }
    let ids = body
        .get("ids")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("missing list field 'ids'"))?
        .iter()
        .map(|v| v.as_str().map(str::to_string).ok_or_else(|| bad_request("'ids' must be strings")))
        .collect::<OpResult<Vec<String>>>()?;
    if ids.is_empty() || ids.len() > MAX_IDS {
        return Err(bad_request(format!("'ids' takes 1 to {MAX_IDS} message ids")));
    }
    let db = ctx.db;
    let mut owners: HashMap<String, i64> = HashMap::new();
    let mut channels: HashMap<String, bool> = HashMap::new();
    let mut rows = Vec::with_capacity(ids.len());
    let mut refused = Vec::new();
    for id in &ids {
        let m = db.message_by_id(id)?.ok_or_else(|| not_found(format!("no message '{id}'")))?;
        let internal = match &m.channel_id {
            Some(c) => match channels.get(c) {
                Some(v) => *v,
                None => {
                    let v = channel_internal(db, c, ctx.p.user_id, &mut owners)?;
                    channels.insert(c.clone(), v);
                    v
                }
            },
            None => {
                let rcpt = m.recipient_id.clone().unwrap_or_default();
                owner_of(db, &m.sender_id, &mut owners)? == ctx.p.user_id
                    && owner_of(db, &rcpt, &mut owners)? == ctx.p.user_id
            }
        };
        if internal {
            rows.push(row(db, &m)?);
        } else {
            refused.push(id.clone());
        }
    }
    if !refused.is_empty() {
        return Err(forbidden(format!("not owner-internal for {}: {}", ctx.p.user, refused.join(" "))));
    }
    Ok(json!({"messages": rows}))
}

fn owner_of(db: &dyn Tx, agent_id: &str, cache: &mut HashMap<String, i64>) -> OpResult<i64> {
    if let Some(o) = cache.get(agent_id) {
        return Ok(*o);
    }
    let o = db.agent_by_id(agent_id)?.map_or(-1, |a| a.owner_id);
    cache.insert(agent_id.to_string(), o);
    Ok(o)
}

/// Every sender ever and every current member of the channel is `user_id`'s.
fn channel_internal(db: &dyn Tx, channel_id: &str, user_id: i64, owners: &mut HashMap<String, i64>) -> OpResult<bool> {
    for member in db.channel_member_ids(channel_id)? {
        if owner_of(db, &member, owners)? != user_id {
            return Ok(false);
        }
    }
    for id in db.channel_message_ids(channel_id)? {
        let Some(m) = db.message_by_id(&id)? else { continue };
        if owner_of(db, &m.sender_id, owners)? != user_id {
            return Ok(false);
        }
    }
    Ok(true)
}

fn row(db: &dyn Tx, m: &Message) -> OpResult<Value> {
    let mut attachments = Vec::new();
    for meta in db.attachment_list(&m.id)? {
        let Some(a) = db.attachment(&meta.id)? else { continue };
        attachments.push(json!({
            "filename": a.filename, "size": a.bytes.len(), "sha256": sha256_hex(&a.bytes), "data": B64.encode(&a.bytes),
        }));
    }
    let read = match (&m.channel_id, &m.recipient_id) {
        (Some(c), _) => {
            let mut members = Vec::new();
            for agent in db.channel_member_ids(c)? {
                let Some(a) = db.agent_by_id(&agent)? else { continue };
                let read = db.read_seq(c, &agent)?.is_some_and(|s| s >= m.seq);
                members.push(json!({"name": a.canonical(), "read": read}));
            }
            json!({"members": members})
        }
        (None, Some(r)) => json!({"read_at": db.dm_read_at(&m.id, r)?}),
        (None, None) => Value::Null,
    };
    let reactions: Vec<Value> =
        db.reactions(&m.id)?.into_iter().map(|(emoji, by)| json!({"emoji": emoji, "by": by})).collect();
    Ok(json!({
        "message_id": m.id, "seq": m.seq, "from": m.sender, "to": m.recipient, "channel": m.channel,
        "created_at": m.created_at, "reply_to": m.reply_to, "text": m.text, "sha256": sha256_hex(m.text.as_bytes()),
        "attachments": attachments, "read": read, "reactions": reactions,
    }))
}
