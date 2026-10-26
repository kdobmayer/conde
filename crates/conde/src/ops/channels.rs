//! Channels: one shared log plus a per-member read position, and a topic.

use serde_json::{Value, json};

use super::agents::{self, Agent};
use super::messages::{Target, forbid_non_member, insert_message};
use crate::address::normalize_channel;
use crate::dispatch::{Ctx, OpResult, bad_request, conflict, forbidden, not_found, one_line, opt_str, str_param};
pub use crate::store::Channel;
use crate::store::Tx;
use crate::util::new_ulid;

/// Longest channel topic, in characters.
pub const MAX_TOPIC: usize = 256;

fn parse_name(raw: &str) -> OpResult<String> {
    normalize_channel(raw).ok_or_else(|| bad_request(format!("invalid channel name '{raw}' (expected '#name')")))
}

/// A topic as stored: trimmed, one line, at most MAX_TOPIC characters; empty clears it.
fn parse_topic(raw: &str) -> OpResult<Option<String>> {
    let topic = raw.trim();
    one_line(topic, "topic")?;
    if topic.chars().count() > MAX_TOPIC {
        return Err(bad_request(format!("topic is {} characters; the limit is {MAX_TOPIC}", topic.chars().count())));
    }
    Ok((!topic.is_empty()).then(|| topic.to_string()))
}

pub fn by_name(db: &dyn Tx, name: &str) -> OpResult<Option<Channel>> {
    Ok(db.channel_by_name(name)?)
}

fn existing(ctx: &Ctx, raw: &str) -> OpResult<Channel> {
    let name = parse_name(raw)?;
    by_name(ctx.db, &name)?.ok_or_else(|| not_found(format!("no channel '#{name}' (create it with: conde create '#{name}')")))
}

pub fn member_ids(db: &dyn Tx, channel_id: &str) -> OpResult<Vec<String>> {
    Ok(db.channel_member_ids(channel_id)?)
}

/// The member's read position, or None when not a member.
pub fn read_seq(db: &dyn Tx, channel_id: &str, agent_id: &str) -> OpResult<Option<i64>> {
    Ok(db.read_seq(channel_id, agent_id)?)
}

/// Read positions only ever move forward.
pub fn advance_cursor(db: &dyn Tx, channel_id: &str, agent_id: &str, seq: i64) -> OpResult<()> {
    Ok(db.advance_cursor(channel_id, agent_id, seq)?)
}

/// The channel, provided the caller is a member of it.
pub fn member_channel(ctx: &Ctx, me: &Agent, raw: &str) -> OpResult<Channel> {
    let channel = existing(ctx, raw)?;
    if read_seq(ctx.db, &channel.id, &me.id)?.is_none() {
        return Err(forbid_non_member(&format!("#{}", channel.name)));
    }
    Ok(channel)
}

/// New members start at the end: history is readable, never unread.
pub(crate) fn add_member(ctx: &Ctx, channel: &Channel, agent_id: &str) -> OpResult<()> {
    let seq = ctx.db.channel_last_seq(&channel.id)?;
    Ok(ctx.db.insert_membership(&channel.id, agent_id, seq)?)
}

pub fn create(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let name = parse_name(str_param(body, "channel")?)?;
    let topic = opt_str(body, "topic")?.map(parse_topic).transpose()?.flatten();
    if by_name(ctx.db, &name)?.is_some() {
        return Err(conflict(format!("channel '#{name}' already exists")));
    }
    let channel = Channel { id: new_ulid(), name, created_by: me.id.clone(), topic };
    ctx.db.insert_channel(&channel)?;
    add_member(ctx, &channel, &me.id)?;
    Ok(json!({"channel": format!("#{}", channel.name), "topic": channel.topic}))
}

/// `channel.topic`: set or clear (empty) the topic; the creator or a human may.
pub fn topic(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let channel = existing(ctx, str_param(body, "channel")?)?;
    if channel.created_by != me.id && !me.is_human() {
        return Err(forbidden(format!("only the creator of #{} or a human sets its topic", channel.name)));
    }
    let topic = parse_topic(str_param(body, "topic")?)?;
    ctx.db.set_channel_topic(&channel.id, topic.as_deref())?;
    Ok(json!({"channel": format!("#{}", channel.name), "topic": topic}))
}

pub fn join(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    agents::unprotected(ctx, &me)?;
    let channel = existing(ctx, str_param(body, "channel")?)?;
    let label = format!("#{}", channel.name);
    if read_seq(ctx.db, &channel.id, &me.id)?.is_some() {
        return Ok(json!({"channel": label, "already_member": true, "topic": channel.topic}));
    }
    add_member(ctx, &channel, &me.id)?;
    let text = format!("@{} joined {label}.", me.canonical());
    let (post, _) = insert_message(ctx, &me, Target::Channel(&channel), &text, None, Vec::new())?;
    Ok(json!({"channel": label, "already_member": false, "message_id": post, "topic": channel.topic}))
}

pub fn leave(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let channel = member_channel(ctx, &me, str_param(body, "channel")?)?;
    ctx.db.delete_membership(&channel.id, &me.id)?;
    Ok(json!({"channel": format!("#{}", channel.name)}))
}

/// Add a registered agent; the join post mentions it, so it gets exactly one
/// push even in an empty channel.
pub fn invite(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    let channel = member_channel(ctx, &me, str_param(body, "channel")?)?;
    let label = format!("#{}", channel.name);
    let invitee = agents::lookup_bound(ctx, str_param(body, "name")?)?;
    agents::unprotected(ctx, &invitee)?;
    if read_seq(ctx.db, &channel.id, &invitee.id)?.is_some() {
        return Err(conflict(format!("{} is already a member of '{label}'", invitee.canonical())));
    }
    add_member(ctx, &channel, &invitee.id)?;
    let text = format!("@{} was added to {label} by {}.", invitee.canonical(), me.canonical());
    let (post, _) = insert_message(ctx, &me, Target::Channel(&channel), &text, None, Vec::new())?;
    Ok(json!({"channel": label, "name": invitee.canonical(), "message_id": post}))
}

pub fn list(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let channels: Vec<Value> = ctx
        .db
        .channel_summaries()?
        .into_iter()
        .map(|c| json!({"channel": format!("#{}", c.name), "topic": c.topic, "members": c.members, "messages": c.messages}))
        .collect();
    Ok(json!({"channels": channels}))
}

/// Who belongs to a channel; read positions (`behind`) only for a member, like its content.
pub fn members(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let channel = existing(ctx, str_param(body, "channel")?)?;
    let member = match ctx.me() {
        Ok(me) => read_seq(ctx.db, &channel.id, &me.id)?.is_some(),
        Err(_) => false,
    };
    let members: Vec<Value> = ctx
        .db
        .channel_member_status(&channel.id)?
        .into_iter()
        .map(|(name, behind)| if member { json!({"name": name, "behind": behind}) } else { json!({"name": name}) })
        .collect();
    Ok(json!({"channel": format!("#{}", channel.name), "members": members}))
}
