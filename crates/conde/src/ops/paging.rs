//! Human paging on a shared board (docs/DESIGN-human-paging.md): an agent pages its own human
//! (`human.page`), switches its human's "work done" pages (`human.done_pages`), and reads its
//! human's state (`human.status`, the whoami line).

use serde_json::{Value, json};

use super::agents::Agent;
use super::messages::{Target, insert_message};
use crate::dispatch::{Ctx, OpResult, bad_request, forbidden, not_found, opt_str, str_param};
use crate::humans;
use crate::store::UserSlack;
use crate::util::iso_from_ms;

/// The longest page, in characters: the whole line fits the Slack DM.
pub const PAGE_CHARS: usize = 140;
/// The most pages one agent stores in a rolling hour.
pub const PAGES_PER_HOUR: i64 = 10;
const HOUR_MS: i64 = 3_600_000;

/// The caller (an agent, never a human) on a shared board, with its owner's human row and
/// paging fields.
fn caller(ctx: &Ctx) -> OpResult<(String, Agent, Agent, UserSlack)> {
    let board = ctx
        .board
        .ok_or_else(|| {
            forbidden("paging reaches your human on a shared board: name it with --board <alias>")
        })?
        .to_string();
    let me = ctx.me()?;
    if me.is_human() {
        return Err(forbidden(humans::no_sending(&board)));
    }
    let human = humans::of_owner(ctx.db, me.owner_id)?.ok_or_else(|| {
        not_found(format!(
            "{} has no human name on {board} yet; tell your human",
            me.owner
        ))
    })?;
    let user = ctx
        .db
        .user_slack(me.owner_id)?
        .ok_or_else(|| not_found(format!("no user '{}'", me.owner)))?;
    Ok((board, me, human, user))
}

/// Why a page is stored but not sent to Slack, with the fix; None when it is sent.
fn not_paged(ctx: &Ctx, board: &str, user: &UserSlack) -> Option<String> {
    if !ctx.slack {
        return Some(format!(
            "{board} sends no Slack DMs (no bot token is set); your human reads it on the web board"
        ));
    }
    if user.slack_member_id.is_some() {
        return None;
    }
    let fix = format!(
        "the board admin sets it: conde admin slack {} <member-id>",
        user.name
    );
    Some(match &user.email {
        Some(email) => format!(
            "no Slack member id for {} yet; the board looks it up by {email} and sends the page when found; if not, {fix}",
            user.name
        ),
        None => format!(
            "no Slack member id and no sign-in email for {}; {fix}",
            user.name
        ),
    })
}

/// `human.page {text, kind?}`: one line of at most [`PAGE_CHARS`] characters, at most
/// [`PAGES_PER_HOUR`] an hour per agent, from the calling agent to its own human, stored with `messages.page = kind` (`blocked`, the default, or
/// `done`), which makes the human's `dm` notification the Slack worker sends. A done page while
/// the human's done pages are off is not stored.
pub fn page(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let (board, me, human, user) = caller(ctx)?;
    let text = str_param(body, "text")?.trim();
    if text.is_empty() {
        return Err(bad_request(
            "a page needs a line: what you need from your human",
        ));
    }
    // a line separator would split the line in Slack as a newline does
    if text
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err(bad_request(
            "a page is one line: say what you need in one line",
        ));
    }
    let chars = text.chars().count();
    if chars > PAGE_CHARS {
        return Err(bad_request(format!(
            "a page is at most {PAGE_CHARS} characters; this one is {chars}"
        )));
    }
    let kind = match opt_str(body, "kind")?.unwrap_or("blocked") {
        k @ ("blocked" | "done") => k,
        other => {
            return Err(bad_request(format!(
                "kind '{other}' is not blocked or done"
            )));
        }
    };
    let to = human.canonical();
    if kind == "done" && !user.done_pages {
        return Ok(json!({"to": to, "status": "not paged", "reason": "done pages are off"}));
    }
    if ctx
        .db
        .pages_since(&me.id, &iso_from_ms(ctx.now_ms - HOUR_MS))?
        >= PAGES_PER_HOUR
    {
        return Err(forbidden(format!(
            "not paged: you paged your human {PAGES_PER_HOUR} times in the last hour, the limit; say what you need in \
             one page when the oldest is an hour old"
        )));
    }
    let inserted = insert_message(
        ctx,
        &me,
        Target::Agent(&human),
        text,
        None,
        Vec::new(),
        Some(kind),
    )?;
    let mut out = json!({"message_id": inserted.id, "seq": inserted.seq, "to": to, "kind": kind});
    match not_paged(ctx, &board, &user) {
        None => out["status"] = json!("paged"),
        Some(why) => {
            out["status"] = json!("stored, not paged");
            out["reason"] = json!(why);
        }
    }
    Ok(out)
}

/// `human.done_pages {on}`: whether the caller's human takes "work done" pages; any of the
/// owner's agents switches it when the human asks.
pub fn done_pages(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    let (_, me, human, _) = caller(ctx)?;
    let on = body
        .get("on")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad_request("missing boolean field 'on'"))?;
    ctx.db.set_done_pages(me.owner_id, on)?;
    Ok(json!({"human": human.canonical(), "done_pages": on}))
}

/// The caller's human as `whoami` shows it: name, board, whether the board has a Slack member id
/// to send to (`slack`: a bot token and an id; not that a DM arrives), and
/// whether done pages are on.
pub fn status_of(ctx: &Ctx) -> OpResult<Value> {
    let (board, _, human, user) = caller(ctx)?;
    Ok(json!({
        "human": human.canonical(),
        "board": board,
        "slack": ctx.slack && user.slack_member_id.is_some(),
        "done_pages": user.done_pages,
    }))
}

/// `human.status`.
pub fn status(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    status_of(ctx)
}
