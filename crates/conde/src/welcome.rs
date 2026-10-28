//! The workspace's welcome (DEPLOY.md, "The welcome"): what the admin set (stored as the
//! `welcome` setting), the lines `conde setup` and `conde login` print from `board.info`, and the
//! two messages the server writes on a user's first host enrollment: the greeting, in the admin
//! agent's name to the newcomer's human name, and the arrival notice, from the newcomer's human
//! name to the admin agent. Both are server-originated (no session sends them); the audit line
//! names their origin `system`.

use anyhow::Context;
use conde_remote::web::{Enrollment, SetWelcome, Welcome};

use crate::address::{Kind, normalize_channel, split_qualified};
use crate::store::{Agent, NewMessage, NewNotification, StoreResult, Tx, topics};
use crate::util::{iso_from_ms, new_ulid};

const KEY: &str = "welcome";

/// The welcome as the admin stored it (unset fields None).
pub fn stored(tx: &dyn Tx) -> StoreResult<Welcome> {
    Ok(tx
        .setting(KEY)?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

/// The published agent `owner/name` of an admin user; Err says why it is not one.
fn admin_agent(tx: &dyn Tx, name: &str) -> anyhow::Result<Result<Agent, String>> {
    let Some((Some(owner), agent)) = split_qualified(name) else {
        return Ok(Err(format!(
            "{name} is not an agent's full name, owner/name"
        )));
    };
    let Some(user) = tx.user_by_name(&owner)? else {
        return Ok(Err(format!("no user {owner} on this board")));
    };
    if !user.admin || user.disabled {
        return Ok(Err(format!(
            "{owner} is not an admin of this workspace (conde admin user promote {owner})"
        )));
    }
    match tx.agent_by_name(user.user_id, &agent)? {
        Some(a) if !a.retired && a.address.as_ref().is_some_and(|x| x.kind != Kind::Human) => {
            Ok(Ok(a))
        }
        _ => Ok(Err(format!(
            "{owner}/{agent} is not a published agent on this board (its human publishes it: conde publish {agent} --board <alias>)"
        ))),
    }
}

/// Stores `w` once its admin agent is a published agent of an admin user and its lobby a
/// channel on the board.
pub fn set(tx: &dyn Tx, w: &Welcome) -> anyhow::Result<SetWelcome> {
    if let Some(name) = &w.admin_agent
        && let Err(why) = admin_agent(tx, name)?
    {
        return Ok(SetWelcome::Refused(why));
    }
    if let Some(lobby) = &w.lobby {
        let found = normalize_channel(lobby)
            .map(|c| tx.channel_by_name(&c))
            .transpose()?
            .flatten();
        if found.is_none() {
            return Ok(SetWelcome::Refused(format!(
                "no channel {lobby} on this board"
            )));
        }
    }
    tx.set_setting(KEY, &serde_json::to_string(w)?)?;
    Ok(SetWelcome::Stored(w.clone()))
}

/// The lines `conde setup` and `conde login` print for board `alias` from its welcome (None:
/// a server from before the welcome).
pub fn lines(alias: &str, welcome: Option<&Welcome>) -> String {
    let w = welcome.cloned().unwrap_or_default().filled();
    let mut out = format!("Setup guide: {}\n", w.guide.as_deref().unwrap_or_default());
    if let Some(agent) = &w.admin_agent {
        out.push_str(&format!(
            "Questions: conde send {agent} \"<how the setup went, or your question>\" --board {alias}\n  \
             (from a name published there: conde publish <name> --board {alias}; keep it registered, {agent} replies to it)\n"
        ));
    }
    if let Some(lobby) = &w.lobby {
        out.push_str(&format!(
            "Team channel: {lobby} (conde read '{lobby}' --board {alias})\n"
        ));
    }
    out
}

/// The greeting the newcomer's human name gets from the admin agent.
pub fn greeting(owner: &str, board: &str, admin_agent: &str, w: &Welcome) -> String {
    let w = w.clone().filled();
    let mut text = format!(
        "{}\n\nYou are in, {owner}: your host is enrolled on {board}. Questions go to {admin_agent}: \
         conde send {admin_agent} \"<your question>\" --board {board}. Your agents reach the team once \
         published here: conde publish <name> --board {board}.",
        w.text.as_deref().unwrap_or_default()
    );
    if let Some(lobby) = &w.lobby {
        text.push_str(&format!(" The team channel is {lobby}."));
    }
    text.push_str(&format!(
        " Setup guide: {}",
        w.guide.as_deref().unwrap_or_default()
    ));
    text
}

/// The arrival notice the admin agent gets.
pub fn notice(owner: &str, host: &str, how: Enrollment, at: &str) -> String {
    format!(
        "{owner} enrolled host {host} ({}) at {at}; say hello",
        how.as_str()
    )
}

/// A server-originated DM from `from` to `to`; `notify`: a notification for `to` (an agent; a
/// human on a shared board gets only pages).
fn system_dm(
    tx: &dyn Tx,
    from: &Agent,
    to: &Agent,
    text: &str,
    now_ms: i64,
    notify: bool,
) -> anyhow::Result<String> {
    let id = new_ulid();
    tx.insert_message(&NewMessage {
        id: &id,
        sender_id: &from.id,
        recipient_id: Some(&to.id),
        channel_id: None,
        text,
        reply_to: None,
        created_at: &iso_from_ms(now_ms),
    })?;
    if notify {
        tx.insert_notification(&NewNotification {
            id: &format!("{id}.{}", to.id),
            message_id: &id,
            recipient_id: &to.id,
            kind: "dm",
            binding_version: to.binding_version,
            next_attempt_at: now_ms,
        })?;
    }
    Ok(id)
}

/// A key was minted for `host`, new to the board: on the user's first enrollment (recorded
/// once per user, whether or not a welcome is set), the greeting and the arrival notice when the
/// welcome names an admin agent that is still a published agent of an admin user, of another
/// user. `board`: the shared board's name.
pub fn enrolled(
    tx: &dyn Tx,
    board: &str,
    user_id: i64,
    host: &str,
    how: Enrollment,
    now_ms: i64,
) -> anyhow::Result<()> {
    let at = iso_from_ms(now_ms);
    if !tx.mark_user_arrived(user_id, &at)? {
        return Ok(());
    }
    let user = tx.user_by_id(user_id)?.context("the user vanished")?;
    let w = stored(tx)?;
    let Some(name) = &w.admin_agent else {
        return Ok(());
    };
    let admin = match admin_agent(tx, name)? {
        Ok(a) if a.owner_id != user_id => a,
        Ok(_) => return Ok(()),
        Err(why) => {
            tracing::warn!(user = %user.name, %why, "no welcome sent: the welcome's admin agent is not usable");
            return Ok(());
        }
    };
    crate::humans::ensure(tx, user_id)?;
    let Some(human) = crate::humans::of_owner(tx, user_id)? else {
        tracing::warn!(user = %user.name, "no welcome sent: the newcomer has no human name");
        return Ok(());
    };
    let greeting = greeting(&user.name, board, &admin.canonical(), &w);
    let greeting_id = system_dm(tx, &admin, &human, &greeting, now_ms, false)?;
    let notice = notice(&user.name, host, how, &at);
    let notice_id = system_dm(tx, &human, &admin, &notice, now_ms, true)?;
    tx.announce(topics::MESSAGES | topics::DELIVERY);
    tracing::info!(
        origin = "system",
        user = %user.name,
        %host,
        admin_agent = %admin.canonical(),
        greeting = %greeting_id,
        notice = %notice_id,
        "welcome sent: greeting to the newcomer, arrival notice to the admin agent"
    );
    Ok(())
}
