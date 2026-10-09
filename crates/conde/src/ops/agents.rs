//! Registry: names per owner bound to harness addresses.

use serde_json::{Value, json};

use super::channels;
use crate::address::{Address, Kind, split_qualified};
use crate::dispatch::{
    Ctx, OpResult, bad_request, conflict, forbidden, not_found, one_line, opt_str, str_param,
};
pub use crate::store::Agent;
use crate::store::Tx;
use crate::util::new_ulid;

/// The channel a newly registered name joins, when it exists.
pub const LOBBY: &str = "lobby";

pub fn by_id(db: &dyn Tx, id: &str) -> OpResult<Option<Agent>> {
    Ok(db.agent_by_id(id)?)
}

pub fn by_name(db: &dyn Tx, owner_id: i64, name: &str) -> OpResult<Option<Agent>> {
    Ok(db.agent_by_name(owner_id, name)?)
}

pub fn at_address(db: &dyn Tx, owner_id: i64, address: &Address) -> OpResult<Vec<Agent>> {
    Ok(db.agents_at_address(owner_id, address)?)
}

/// Resolve `name` (caller's namespace) or `owner/name` to an agent row,
/// bound or not.
pub fn find(ctx: &Ctx, input: &str) -> OpResult<Option<Agent>> {
    let (owner, name) =
        split_qualified(input).ok_or_else(|| bad_request(format!("invalid name '{input}'")))?;
    let owner_id = match owner {
        None => ctx.p.user_id,
        Some(o) => match ctx.db.user_id(&o)? {
            Some(id) => id,
            None => return Ok(None),
        },
    };
    by_name(ctx.db, owner_id, &name)
}

/// Like `find`, but the name must currently be bound to an address.
pub fn lookup_bound(ctx: &Ctx, input: &str) -> OpResult<Agent> {
    find(ctx, input)?
        .filter(|a| a.address.is_some())
        .ok_or_else(|| not_found(format!("no agent registered as '{input}'")))
}

fn set_binding(db: &dyn Tx, agent_id: &str, address: Option<&Address>) -> OpResult<()> {
    Ok(db.set_binding(agent_id, address)?)
}

fn ensure_row(ctx: &Ctx, name: &str) -> OpResult<Agent> {
    if let Some(a) = by_name(ctx.db, ctx.p.user_id, name)? {
        return Ok(a);
    }
    ctx.db.insert_agent(&new_ulid(), ctx.p.user_id, name)?;
    by_name(ctx.db, ctx.p.user_id, name)?.ok_or_else(|| not_found(name.to_string()))
}

/// Bind a name to the caller's own address (self-only; names move, pins hold).
pub fn register(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let name = ctx.own_name(str_param(body, "name")?)?;
    if name == "everybody" {
        return Err(bad_request("'everybody' is reserved (channel-wide mention)"));
    }
    let own = ctx.actor_address()?;
    if let Some(explicit) = opt_str(body, "address")? {
        let explicit = Address::parse(explicit, &ctx.p.host).map_err(|e| bad_request(e.to_string()))?;
        if explicit != own {
            return Err(forbidden(format!(
                "registration is self-only: this shell is {own}, not {explicit}. \
                 Run `conde register {name}` inside the session that should own the name."
            )));
        }
    }
    let agent = ensure_row(ctx, &name)?;
    // `keep_sessions` (the browser naming its human): never move a name off an agent session,
    // checked in the same transaction as the binding.
    if body.get("keep_sessions").and_then(Value::as_bool) == Some(true)
        && let Some(current) = agent.address.as_ref().filter(|a| a.kind != Kind::Human)
    {
        return Err(conflict(format!("{} is bound to the session {current}", agent.canonical())));
    }
    if let Some(pin) = &agent.pinned
        && *pin != own.to_string() {
            return Err(forbidden(format!(
                "{} is owner-pinned to {pin}; refusing {own}. Unpin it first (conde unpin {name}).",
                agent.canonical()
            )));
        }
    if agent.address.as_ref() != Some(&own) {
        set_binding(ctx.db, &agent.id, Some(&own))?;
    }
    let joined = if agent.binding_version == 0 { join_lobby(ctx, &agent)? } else { None };
    Ok(json!({"name": agent.canonical(), "address": own.to_string(), "joined": joined}))
}

/// A name's first registration joins `#lobby` when it exists: a plain
/// membership at the end of the log, no join post, no notification.
fn join_lobby(ctx: &Ctx, agent: &Agent) -> OpResult<Option<String>> {
    let Some(lobby) = ctx.db.channel_by_name(LOBBY)? else { return Ok(None) };
    if ctx.db.read_seq(&lobby.id, &agent.id)?.is_some() {
        return Ok(None);
    }
    channels::add_member(ctx, &lobby, &agent.id)?;
    Ok(Some(format!("#{}", lobby.name)))
}

/// Self-only for agents (only the caller's own binding); the human owner may
/// remove any unpinned name in the namespace.
pub fn unregister(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let name = ctx.own_name(str_param(body, "name")?)?;
    let caller = ctx.actor_address()?;
    let canonical = format!("{}/{name}", ctx.p.user);
    let Some(agent) = by_name(ctx.db, ctx.p.user_id, &name)? else {
        return Ok(json!({"name": canonical, "was_registered": false}));
    };
    let Some(current) = &agent.address else {
        return Ok(json!({"name": canonical, "was_registered": false}));
    };
    if let Some(pin) = &agent.pinned {
        return Err(forbidden(format!(
            "{canonical} is owner-pinned to {pin}; refusing unregister. Unpin it first."
        )));
    }
    if caller.kind != Kind::Human && *current != caller {
        return Err(forbidden(format!(
            "unregister is self-only: {canonical} is bound to {current}, not to this session ({caller}). \
             Only that session or the human owner can remove it."
        )));
    }
    if let Some(expected) = opt_str(body, "if_address")? {
        let expected = Address::parse(expected, &ctx.p.host).map_err(|e| bad_request(e.to_string()))?;
        if expected != *current {
            return Err(conflict(format!(
                "not removing {canonical}: registered as '{current}', not '{expected}'"
            )));
        }
    }
    set_binding(ctx.db, &agent.id, None)?;
    Ok(json!({"name": canonical, "was_registered": true, "address": current.to_string()}))
}

/// Owner pin: the sole address a role may be bound to (human only).
pub fn pin(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    if ctx.actor_address()?.kind != Kind::Human {
        return Err(forbidden("pins are set by the human owner only"));
    }
    let name = ctx.own_name(str_param(body, "name")?)?;
    let agent = ensure_row(ctx, &name)?;
    match opt_str(body, "address")? {
        Some(raw) => {
            let address = Address::parse(raw, &ctx.p.host).map_err(|e| bad_request(e.to_string()))?;
            ctx.db.set_pin(&agent.id, Some(&address.to_string()))?;
            if agent.address.as_ref() != Some(&address) {
                set_binding(ctx.db, &agent.id, Some(&address))?;
            }
            Ok(json!({"name": agent.canonical(), "pinned": address.to_string()}))
        }
        None => {
            ctx.db.set_pin(&agent.id, None)?;
            Ok(json!({"name": agent.canonical(), "pinned": null}))
        }
    }
}

pub fn whoami(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let me = ctx.me()?;
    Ok(json!({"name": me.canonical(), "address": me.address.map(|a| a.to_string())}))
}

pub fn resolve(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let agent = lookup_bound(ctx, str_param(body, "name")?)?;
    Ok(json!({"name": agent.canonical(), "address": agent.address.map(|a| a.to_string())}))
}

pub fn list(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let agents: Vec<Value> = ctx
        .db
        .bound_agents()?
        .into_iter()
        .map(|a| {
            json!({
                "name": a.canonical(),
                "address": a.address.as_ref().map(ToString::to_string),
                "display": a.display,
                "pinned": a.pinned,
            })
        })
        .collect();
    Ok(json!({"agents": agents}))
}

pub fn profile_get(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let input = str_param(body, "name")?;
    let agent = find(ctx, input)?.ok_or_else(|| not_found(format!("no agent named '{input}'")))?;
    Ok(json!({
        "name": agent.canonical(),
        "address": agent.address.as_ref().map(ToString::to_string),
        "display": agent.display,
        "pinned": agent.pinned,
    }))
}

/// Profiles are human-edited: agents never write them.
pub fn profile_set(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    if ctx.actor_address()?.kind != Kind::Human {
        return Err(forbidden("profiles are human-edited; agents do not edit profiles (ask the human)"));
    }
    let name = ctx.own_name(str_param(body, "name")?)?;
    let key = str_param(body, "key")?;
    if !key.eq_ignore_ascii_case("display") {
        return Err(bad_request(format!("unknown profile field '{key}' (stored fields: Display)")));
    }
    let value = str_param(body, "value")?.trim();
    one_line(value, "Display")?;
    let agent = by_name(ctx.db, ctx.p.user_id, &name)?
        .ok_or_else(|| not_found(format!("no agent named '{name}'")))?;
    let value = (!value.is_empty()).then_some(value);
    ctx.db.set_display(&agent.id, value)?;
    Ok(json!({"name": agent.canonical(), "key": "Display", "value": value}))
}
