//! Each owner's one human name on a shared board (docs/DESIGN-human-paging.md): a protected
//! row bound to `human:<owner>@web`, the Slack sink of its owner. Humans never send on a shared
//! board; agents reach their own human only by paging.

use crate::address::{Address, Kind};
use crate::store::{Agent, StoreResult, Tx};
use crate::util::{iso_from_ms, new_ulid, now_ms};

/// The name a new human row takes; reserved for new registrations on a shared board.
pub const HUMAN_NAME: &str = "human";

/// The ops a human actor never runs on a shared board: each sends, or posts in a channel.
pub const SEND_OPS: &[&str] =
    &["message.send", "reaction.set", "channel.invite", "channel.join", "channel.create", "human.page"];

/// The refusal of a send by a human on a shared board.
pub fn no_sending(board: &str) -> String {
    format!(
        "humans do not send on {board}: your agents page you on Slack (conde page), and you read the board on its \
         web page; ask one of your agents to send"
    )
}

/// The binding every human row has: `human:<owner>@web`, a host no credential acts as.
pub fn web_address(owner: &str) -> Address {
    Address::new(Kind::Human, owner, conde_remote::web::BROWSER_HOST)
}

/// The refusal of an op that would change the human row's binding or membership.
pub fn protected(a: &Agent) -> String {
    format!("{} is its owner's human name: the board keeps it, and nothing binds, moves or renames it", a.canonical())
}

/// The owner's human row: the human-kind row with the highest binding version.
pub fn of_owner(tx: &dyn Tx, owner_id: i64) -> StoreResult<Option<Agent>> {
    Ok(tx.owner_agents(owner_id)?.into_iter().filter(Agent::is_human).max_by_key(|a| a.binding_version))
}

/// What [`ensure`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ensured {
    /// The row was already right.
    Kept(String),
    /// An existing human row was rebound to `web`.
    Rebound(String),
    /// A new `<owner>/human` row.
    Created(String),
    /// No human row, and the owner's `human` name is another agent's: nothing changed.
    Conflict(String),
}

/// Makes sure the user has exactly one human row, bound to `human:<owner>@web`, inside the
/// caller's write transaction (which serializes writers, so a racing ensure sees this one's row).
/// Extra human rows (several per owner) are unbound and retired, the one with the highest
/// binding version kept.
pub fn ensure(tx: &dyn Tx, user_id: i64) -> StoreResult<Option<Ensured>> {
    let Some(user) = tx.user_by_id(user_id)? else { return Ok(None) };
    let mut humans: Vec<Agent> = tx.owner_agents(user_id)?.into_iter().filter(Agent::is_human).collect();
    humans.sort_by_key(|a| std::cmp::Reverse(a.binding_version));
    let at = iso_from_ms(now_ms());
    for extra in humans.iter().skip(1) {
        tx.set_binding(&extra.id, None)?;
        tx.retire_agent(&extra.id, &at)?;
        tracing::warn!(owner = %user.name, name = %extra.canonical(), "human name retired: the owner has another");
    }
    let address = web_address(&user.name);
    if let Some(row) = humans.first() {
        let name = row.canonical();
        if row.pinned.is_some() {
            tx.set_pin(&row.id, None)?;
        }
        if row.address.as_ref() == Some(&address) {
            return Ok(Some(Ensured::Kept(name)));
        }
        tx.set_binding(&row.id, Some(&address))?;
        tracing::info!(owner = %user.name, %name, "human name rebound to the web host");
        return Ok(Some(Ensured::Rebound(name)));
    }
    if let Some(taken) = tx.agent_by_name(user_id, HUMAN_NAME)? {
        tracing::warn!(owner = %user.name, name = %taken.canonical(), "no human name made: the name is another agent's");
        return Ok(Some(Ensured::Conflict(taken.canonical())));
    }
    let id = new_ulid();
    tx.insert_agent(&id, user_id, HUMAN_NAME)?;
    tx.set_binding(&id, Some(&address))?;
    let name = format!("{}/{HUMAN_NAME}", user.name);
    tracing::info!(owner = %user.name, %name, "human name made");
    Ok(Some(Ensured::Created(name)))
}

/// [`ensure`] for every user (the server's startup pass).
pub fn ensure_all(tx: &dyn Tx) -> StoreResult<Vec<Ensured>> {
    let mut out = Vec::new();
    for u in tx.users_slack()? {
        out.extend(ensure(tx, u.user_id)?);
    }
    Ok(out)
}
