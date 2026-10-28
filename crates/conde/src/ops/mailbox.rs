//! The shared board's v2 ops (DESIGN-v2.md §6): `board.info`, and a migrated host's mailboxes:
//! an agent row bound to `mailbox:<agent_id>@<host>` (`human:<user>@<host>` for a human), whose
//! `binding_version` is its route generation.

use conde_remote::Route;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::agents::{self, Agent};
use crate::address::{Address, Kind};
use crate::dispatch::{Ctx, OpResult, bad_request, conflict, forbidden, not_found};
use crate::protocol::v2::{
    BoardInfo, Mine, MineMailbox, PublishBody, Published, UnpublishBody, Unpublished,
};

pub(crate) fn parse<T: DeserializeOwned>(body: &Value) -> OpResult<T> {
    serde_json::from_value(body.clone()).map_err(|e| bad_request(format!("bad body: {e}")))
}

pub(crate) fn json<T: serde::Serialize>(value: &T) -> OpResult<Value> {
    serde_json::to_value(value).map_err(|e| bad_request(e.to_string()))
}

/// A row `host` routes: a mailbox or a published human name bound there.
fn routed(a: &Agent, host: &str) -> bool {
    a.address
        .as_ref()
        .is_some_and(|x| x.host == host && matches!(x.kind, Kind::Mailbox | Kind::Human))
}

fn generation(a: &Agent) -> u64 {
    u64::try_from(a.binding_version).unwrap_or_default()
}

/// The caller's host's routes: what `mailbox.mine` lists and the `routes` frame carries.
pub fn routes(ctx: &Ctx) -> OpResult<Vec<Route>> {
    Ok(ctx
        .db
        .agents_on_host(&ctx.p.host)?
        .iter()
        .filter(|a| routed(a, &ctx.p.host))
        .map(|a| Route {
            mailbox_id: a.id.clone(),
            route_generation: generation(a),
        })
        .collect())
}

/// `board.info`: the advertised name (the default alias) and the caller's account and host.
pub fn board_info(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let name = ctx.shared_board()?.to_string();
    json(&BoardInfo {
        name,
        owner: ctx.p.user.clone(),
        host: ctx.p.host.clone(),
        mailbox_only: ctx.mailbox_only,
    })
}

/// `mailbox.publish {name, take?}`: binds `<caller>/name` on the calling host and bumps its
/// generation. Publishing again as the same mailbox is a no-op; a name bound on another host is
/// a conflict unless `take`, which moves it here (the old host's routes are pushed). A human
/// name is never published (`human: true` is refused): each owner's is the board's own.
pub fn publish(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    ctx.migrated()?;
    let b: PublishBody = parse(body)?;
    let name = ctx.own_name(&b.name)?;
    if name == "everybody" {
        return Err(bad_request(
            "'everybody' is reserved (channel-wide mention)",
        ));
    }
    if b.human {
        return Err(forbidden(
            "a human is not published: each owner's human name is the board's own, and your agents page you there",
        ));
    }
    agents::may_bind(ctx, &name)?;
    let row = agents::ensure_row(ctx, &name)?;
    let target = Address::new(Kind::Mailbox, &row.id, &ctx.p.host);
    match &row.address {
        Some(a) if *a == target => return published(ctx, &row),
        Some(a) if a.host != ctx.p.host && !b.take => {
            return Err(conflict(format!(
                "{} is published on host '{}'; take it over with --take",
                row.canonical(),
                a.host
            )));
        }
        _ => {}
    }
    ctx.db.set_binding(&row.id, Some(&target))?;
    // A pin is a local-board feature: a mailbox row has none.
    if row.pinned.is_some() {
        ctx.db.set_pin(&row.id, None)?;
    }
    if row.binding_version == 0 {
        agents::join_lobby(ctx, &row)?;
    }
    let row = agents::by_id(ctx.db, &row.id)?.ok_or_else(|| not_found(name.clone()))?;
    published(ctx, &row)
}

/// The calling host's route revision, read in the op's own transaction.
pub fn revision(ctx: &Ctx) -> OpResult<u64> {
    Ok(ctx.db.route_revision(&ctx.p.host)?)
}

fn published(ctx: &Ctx, a: &Agent) -> OpResult<Value> {
    json(&Published {
        name: a.name.clone(),
        mailbox_id: a.id.clone(),
        route_generation: generation(a),
        revision: Some(revision(ctx)?),
    })
}

/// `mailbox.unpublish {mailbox_id, expected_generation}`: unbinds only a mailbox bound to the
/// calling host at that generation (else `conflict`: it moved); messages and memberships stay.
pub fn unpublish(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    ctx.migrated()?;
    let b: UnpublishBody = parse(body)?;
    let moved = || {
        conflict(format!(
            "mailbox {} is not routed to host '{}' at generation {}",
            b.mailbox_id, ctx.p.host, b.expected_generation
        ))
    };
    let row = agents::by_id(ctx.db, &b.mailbox_id)?
        .filter(|a| a.owner_id == ctx.p.user_id)
        .ok_or_else(moved)?;
    agents::unprotected(ctx, &row)?;
    if !routed(&row, &ctx.p.host) || generation(&row) != b.expected_generation {
        return Err(moved());
    }
    ctx.db.set_binding(&row.id, None)?;
    json(&Unpublished {
        mailbox_id: row.id,
        route_generation: b.expected_generation + 1,
        revision: Some(revision(ctx)?),
    })
}

/// `mailbox.mine`: the mailboxes routed to the calling host, with their generations.
pub fn mine(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    ctx.migrated()?;
    let mailboxes = ctx
        .db
        .agents_on_host(&ctx.p.host)?
        .iter()
        .filter(|a| routed(a, &ctx.p.host))
        .map(|a| MineMailbox {
            name: a.name.clone(),
            mailbox_id: a.id.clone(),
            route_generation: generation(a),
        })
        .collect();
    json(&Mine {
        mailboxes,
        revision: Some(revision(ctx)?),
    })
}
