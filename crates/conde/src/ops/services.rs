//! A service's notices to people (`service.notify`): a board user with one unbound sender row of
//! actor kind `service` (made by the admin's services route) tells a person of an event in one
//! line and a link. The notice goes to the person's public agent when it is reachable, else to
//! the person as a page, which the Slack worker sends them. `service.presence` tells the same
//! service the state of a person's agent machine.

use serde_json::{Value, json};

use super::agents::{Agent, public_agent_reachable};
use super::machines;
use super::messages::{Target, insert_message};
use super::paging::PAGE_CHARS;
use crate::dispatch::{Ctx, OpResult, bad_request, forbidden, not_found, str_param};
use crate::humans;
use crate::store::UserRow;

/// The actor kind of a service's sender row.
pub const SERVICE: &str = "service";

/// The caller's one sender row.
pub(crate) fn sender(ctx: &Ctx) -> OpResult<Agent> {
    let mut rows: Vec<Agent> = ctx
        .db
        .owner_agents(ctx.p.user_id)?
        .into_iter()
        .filter(|a| a.actor_kind.as_deref() == Some(SERVICE) && !a.retired)
        .collect();
    match rows.len() {
        1 => Ok(rows.remove(0)),
        0 => Err(forbidden(format!(
            "user '{}' is not a service: it has no service sender row (the admin's POST /v1/admin/services makes one)",
            ctx.p.user
        ))),
        _ => Err(forbidden(format!(
            "user '{}' has more than one service sender row",
            ctx.p.user
        ))),
    }
}

/// The person `to` names: a board user name, or the sign-in email of exactly one user.
fn person(ctx: &Ctx, to: &str) -> OpResult<UserRow> {
    let found = if to.contains('@') {
        let mut users = ctx.db.users_by_email(to)?;
        if users.len() > 1 {
            return Err(bad_request(format!(
                "more than one user has the email {to}; name the user instead"
            )));
        }
        users.pop()
    } else {
        ctx.db.user_by_name(&to.to_lowercase())?
    };
    found
        .filter(|u| !u.disabled)
        .ok_or_else(|| not_found(format!("no user {to} on this board")))
}

/// `service.presence {to}`: the person's machine as `{state, sign_in, principal_id}`
/// (`machine.status`'s state word, the reported sign-in flows and the VM identity, null until
/// known), nothing else; `not_found` without a machine. Reads the store only, never Azure.
pub fn presence(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    sender(ctx)?;
    let user = person(ctx, str_param(body, "to")?.trim())?;
    let m = ctx
        .db
        .machine_of_user(user.user_id)?
        .ok_or_else(|| not_found(format!("{} has no machine", user.name)))?;
    Ok(json!({
        "state": machines::state_of(ctx.db, &m, ctx.now_ms)?,
        "sign_in": machines::sign_in_json(&m),
        // always present: a reader takes a missing key for an older board, a null for none
        "principal_id": m.principal_id,
    }))
}

/// `service.notify {to, text, link}`.
pub fn notify(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let from = sender(ctx)?;
    let to = str_param(body, "to")?.trim();
    let text = str_param(body, "text")?.trim();
    let link = str_param(body, "link")?.trim();
    if text.is_empty() {
        return Err(bad_request("a notice needs its line"));
    }
    // a line separator would split the line in Slack as a newline does
    if text
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err(bad_request("a notice is one line"));
    }
    let chars = text.chars().count();
    if chars > PAGE_CHARS {
        return Err(bad_request(format!(
            "a notice is at most {PAGE_CHARS} characters; this one is {chars}"
        )));
    }
    if !conde_remote::web::welcome_url(link) {
        return Err(bad_request(
            "link must be an https URL of at most 512 characters without spaces",
        ));
    }
    let user = person(ctx, to)?;
    let line = format!("{text}\n{link}");
    let (rcpt, page, via) = match public_agent_reachable(ctx.db, user.user_id, ctx.now_ms)? {
        Some(agent) => (agent, None, "agent"),
        None => {
            let human = humans::of_owner(ctx.db, user.user_id)?
                .ok_or_else(|| not_found(format!("{} has no human name yet", user.name)))?;
            (human, Some("blocked"), "human")
        }
    };
    let inserted = insert_message(
        ctx,
        &from,
        Target::Agent(&rcpt),
        &line,
        None,
        Vec::new(),
        page,
    )?;
    Ok(json!({"message_id": inserted.id, "to": rcpt.canonical(), "via": via}))
}
