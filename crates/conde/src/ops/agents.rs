//! Registry: names per owner bound to harness addresses.

use serde_json::{Value, json};

use super::channels;
use crate::address::{Address, Kind, split_qualified};
use crate::dispatch::{
    Ctx, OpError, OpResult, bad_request, conflict, forbidden, not_found, one_line, opt_str, str_param,
};
use crate::humans::{self, HUMAN_NAME};
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

/// Like `find`, but the name must currently be bound to an address. On a shared board an
/// unknown, unbound or unpublished name all read `recipient unavailable` (DESIGN-v2.md §6).
pub fn lookup_bound(ctx: &Ctx, input: &str) -> OpResult<Agent> {
    find(ctx, input)?.filter(|a| a.address.is_some()).ok_or_else(|| match ctx.board {
        Some(_) => unavailable(ctx, input),
        None => not_found(format!("no agent registered as '{input}'")),
    })
}

/// The shared board's one answer for a name that cannot be addressed, whether it never existed
/// or is not published: `recipient unavailable: <owner/name>`.
pub fn unavailable(ctx: &Ctx, input: &str) -> OpError {
    let canonical = match split_qualified(input) {
        Some((owner, name)) => format!("{}/{name}", owner.unwrap_or_else(|| ctx.p.user.clone())),
        None => input.to_string(),
    };
    not_found(format!("recipient unavailable: {canonical}"))
}

/// A row as a shared board lists it: kind and host, never a target (session, thread or pin).
fn target_free(a: &Agent) -> Value {
    json!({
        "name": a.canonical(),
        "kind": a.address.as_ref().map(|x| x.kind.as_str()),
        "host": a.address.as_ref().map(|x| x.host.as_str()),
        "display": a.display,
    })
}

fn set_binding(db: &dyn Tx, agent_id: &str, address: Option<&Address>) -> OpResult<()> {
    Ok(db.set_binding(agent_id, address)?)
}

/// On a shared board a new name is never `human` (each owner's human name is the board's own).
pub(crate) fn not_reserved(ctx: &Ctx, name: &str) -> OpResult<()> {
    if ctx.board.is_some() && name == HUMAN_NAME {
        return Err(bad_request(format!("'{HUMAN_NAME}' is reserved on this board for each owner's human name")));
    }
    Ok(())
}

/// The refusal of a binding or membership change of the owner's human row on a shared board.
pub(crate) fn unprotected(ctx: &Ctx, a: &Agent) -> OpResult<()> {
    if ctx.board.is_some() && a.is_human() {
        return Err(forbidden(humans::protected(a)));
    }
    Ok(())
}

/// A name taken on a shared board: an existing row must not be the human row, a new one must
/// not be the reserved name.
pub(crate) fn may_bind(ctx: &Ctx, name: &str) -> OpResult<()> {
    match by_name(ctx.db, ctx.p.user_id, name)? {
        Some(existing) => unprotected(ctx, &existing),
        None => not_reserved(ctx, name),
    }
}

pub(crate) fn ensure_row(ctx: &Ctx, name: &str) -> OpResult<Agent> {
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
    if ctx.board.is_some() && own.kind == Kind::Human {
        return Err(forbidden(
            "a human's name on this board is the board's own; humans do not register here (your agents page you)",
        ));
    }
    may_bind(ctx, &name)?;
    if let Some(explicit) = opt_str(body, "address")? {
        let explicit = Address::parse_v1(explicit, &ctx.p.host).map_err(|e| bad_request(e.to_string()))?;
        if explicit != own {
            return Err(forbidden(format!(
                "registration is self-only: this shell is {own}, not {explicit}. \
                 Run `conde register {name}` inside the session that should own the name."
            )));
        }
    }
    let agent = ensure_row(ctx, &name)?;
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
pub(crate) fn join_lobby(ctx: &Ctx, agent: &Agent) -> OpResult<Option<String>> {
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
    unprotected(ctx, &agent)?;
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
        let expected = Address::parse_v1(expected, &ctx.p.host).map_err(|e| bad_request(e.to_string()))?;
        if expected != *current {
            return Err(conflict(format!(
                "not removing {canonical}: registered as '{current}', not '{expected}'"
            )));
        }
    }
    set_binding(ctx.db, &agent.id, None)?;
    Ok(json!({"name": canonical, "was_registered": true, "address": current.to_string()}))
}

/// Retire one of the owner's names that nothing holds any more (the human owner only): it leaves
/// the owner's lists and the directory, its history stays readable, and binding it again (a
/// register, or a publish on a shared board) revives it.
pub fn retire(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    if ctx.actor_address()?.kind != Kind::Human {
        return Err(forbidden("retiring a name is the human owner's; ask your human"));
    }
    let name = ctx.own_name(str_param(body, "name")?)?;
    let canonical = format!("{}/{name}", ctx.p.user);
    let agent = by_name(ctx.db, ctx.p.user_id, &name)?.ok_or_else(|| not_found(format!("no agent named '{canonical}'")))?;
    if let Some(address) = &agent.address {
        return Err(forbidden(format!(
            "{canonical} is bound to {address}; unbind it first (conde unregister {name}, or conde unpublish on a shared board)"
        )));
    }
    if !ctx.db.retire_agent(&agent.id, &ctx.now_iso())? {
        return Err(conflict(format!("{canonical} was bound meanwhile; not retired")));
    }
    Ok(json!({"name": canonical, "retired": true}))
}

/// Give a name a new one in place: the row keeps its id, binding, inbox, history and
/// memberships, and the old name is free. Self-only for agents (`old` omitted means the
/// caller's own name); the human owner may rename any unpinned name in the namespace.
pub fn rename(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let new = ctx.own_name(str_param(body, "new")?)?;
    if new == "everybody" {
        return Err(bad_request("'everybody' is reserved (channel-wide mention)"));
    }
    let caller = ctx.actor_address()?;
    let agent = match opt_str(body, "old")? {
        None => ctx.me()?,
        Some(old) => {
            let old = ctx.own_name(old)?;
            let agent = by_name(ctx.db, ctx.p.user_id, &old)?
                .ok_or_else(|| not_found(format!("no agent named '{old}'")))?;
            if caller.kind != Kind::Human && agent.address.as_ref() != Some(&caller) {
                let bound = agent.address.as_ref().map_or("no session".to_string(), ToString::to_string);
                return Err(forbidden(format!(
                    "rename is self-only: {} is bound to {bound}, not to this session ({caller}). \
                     Only that session or the human owner can rename it.",
                    agent.canonical()
                )));
            }
            agent
        }
    };
    let old = agent.canonical();
    unprotected(ctx, &agent)?;
    if agent.name != new {
        not_reserved(ctx, &new)?;
    }
    if let Some(pin) = &agent.pinned {
        return Err(forbidden(format!(
            "{old} is owner-pinned to {pin}; refusing rename. Unpin it first (conde unpin {}).",
            agent.name
        )));
    }
    if agent.name != new {
        if let Some(taken) = by_name(ctx.db, ctx.p.user_id, &new)? {
            let why = match (&taken.pinned, &taken.address) {
                (Some(pin), _) => format!("is owner-pinned to {pin}"),
                (None, Some(at)) => format!("is registered to {at}"),
                // Taking it would merge two inboxes; register takes an old name back whole.
                (None, None) => format!(
                    "is an earlier name that keeps its own messages; if it was yours, take it back with \
                     `conde register {new}` and then `conde unregister {}`",
                    agent.name
                ),
            };
            return Err(conflict(format!("cannot rename {old} to {}: it {why}", taken.canonical())));
        }
        ctx.db.rename_agent(&agent.id, &new)?;
    }
    let agent = by_id(ctx.db, &agent.id)?.ok_or_else(|| not_found(old.clone()))?;
    Ok(json!({
        "name": agent.canonical(),
        "old": old,
        "address": agent.address.as_ref().map(ToString::to_string),
    }))
}

/// Owner pin: the sole address a role may be bound to (human only).
pub fn pin(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    if ctx.actor_address()?.kind != Kind::Human {
        return Err(forbidden("pins are set by the human owner only"));
    }
    let name = ctx.own_name(str_param(body, "name")?)?;
    may_bind(ctx, &name)?;
    let agent = ensure_row(ctx, &name)?;
    match opt_str(body, "address")? {
        Some(raw) => {
            let address = Address::parse_v1(raw, &ctx.p.host).map_err(|e| bad_request(e.to_string()))?;
            if ctx.board.is_some() && address.kind == Kind::Human {
                return Err(forbidden(
                    "a human address is never pinned on this board: each owner's human name is the board's own",
                ));
            }
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
    if ctx.board.is_some() {
        let mut row = target_free(&agent);
        row.as_object_mut().map(|o| o.remove("display"));
        return Ok(row);
    }
    Ok(json!({"name": agent.canonical(), "address": agent.address.map(|a| a.to_string())}))
}

pub fn list(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let agents: Vec<Value> = ctx
        .db
        .bound_agents()?
        .into_iter()
        .map(|a| match ctx.board {
            Some(_) => target_free(&a),
            None => json!({
                "name": a.canonical(),
                "address": a.address.as_ref().map(ToString::to_string),
                "display": a.display,
                "pinned": a.pinned,
            }),
        })
        .collect();
    Ok(json!({"agents": agents}))
}

pub fn profile_get(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let input = str_param(body, "name")?;
    // On a shared board another owner's unpublished name reads as one that never existed.
    let agent = find(ctx, input)?
        .filter(|a| ctx.board.is_none() || a.owner_id == ctx.p.user_id || a.address.is_some())
        .ok_or_else(|| not_found(format!("no agent named '{input}'")))?;
    if ctx.board.is_some() {
        return Ok(target_free(&agent));
    }
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
