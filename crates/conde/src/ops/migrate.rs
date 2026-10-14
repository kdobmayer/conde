//! The one-time conversion of a v1 host to v2 (DESIGN-v2.md §9): a read-only preview of the
//! host's bound names with their cross-owner evidence, and the conversion itself, whose stored
//! answer (the full pre-conversion manifest) replays under its idempotency key.

use std::collections::{BTreeMap, BTreeSet};

use conde_remote::Route;
use serde_json::Value;

use super::mailbox::{json, parse};
use crate::address::{Address, Kind};
use crate::dispatch::{Ctx, OpResult, bad_request, conflict};
use crate::protocol::v2::{
    Evidence, EvidenceSource, Manifest, ManifestRow, MigrateBody, MigratePreview, PreviewRow, Via,
};
use crate::store::{Agent, topics};

/// The caller's names bound on its host, by name.
fn bound_here(ctx: &Ctx) -> OpResult<Vec<Agent>> {
    Ok(ctx.db.agents_on_host(&ctx.p.host)?.into_iter().filter(|a| a.owner_id == ctx.p.user_id).collect())
}

/// `host.migrate_preview`: the host's bound names, each with its evidence (one entry per via,
/// counterpart and source) and the candidate flag. Read-only; answers this host only.
pub fn preview(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    ctx.shared_board()?;
    // Per agent: (via, with, source) -> (messages, last_at), in that order on the wire.
    type Counts = BTreeMap<(&'static str, String, &'static str), (u64, String)>;
    let mut found: BTreeMap<String, Counts> = BTreeMap::new();
    for e in ctx.db.host_evidence(ctx.p.user_id, &ctx.p.host)? {
        let via = if e.via == "dm" { "dm" } else { "channel" };
        let source = if e.imported { "imported" } else { "later" };
        let entry = found.entry(e.agent_id).or_default().entry((via, e.with, source)).or_default();
        entry.0 += 1;
        if e.created_at > entry.1 {
            entry.1 = e.created_at;
        }
    }
    let mut rows = Vec::new();
    for a in bound_here(ctx)? {
        let evidence: Vec<Evidence> = found
            .remove(&a.id)
            .unwrap_or_default()
            .into_iter()
            .map(|((via, with, source), (messages, last_at))| Evidence {
                via: if via == "dm" { Via::Dm } else { Via::Channel },
                with,
                source: if source == "imported" { EvidenceSource::Imported } else { EvidenceSource::Later },
                messages,
                last_at,
            })
            .collect();
        rows.push(PreviewRow {
            agent_id: a.id.clone(),
            name: a.name.clone(),
            address: a.address.as_ref().map(ToString::to_string).unwrap_or_default(),
            pin: a.pinned.clone(),
            candidate: !evidence.is_empty(),
            evidence,
        });
    }
    json(&MigratePreview { rows })
}

/// `host.migrate_v2 {publish}`: in one transaction, fences the host's bridge instances (their
/// connections close), converts the listed names to mailboxes (a human name keeps its address)
/// with a bumped generation, unbinds the host's other names, clears the pins, deletes the host's
/// endpoints and sets `mailbox_only`. Returns every former binding with its address and pin.
pub fn migrate(ctx: &Ctx, body: &Value) -> OpResult<Value> {
    ctx.shared_board()?;
    if ctx.mailbox_only {
        return Err(conflict(format!("host '{}' already migrated to v2", ctx.p.host)));
    }
    let b: MigrateBody = parse(body)?;
    let rows = bound_here(ctx)?;
    let publish: BTreeSet<&str> = b.publish.iter().map(String::as_str).collect();
    if let Some(stray) = publish.iter().find(|id| !rows.iter().any(|a| a.id == **id)) {
        return Err(bad_request(format!("agent {stray} is not one of your names bound on host '{}'", ctx.p.host)));
    }
    let now = ctx.now_iso();
    let fenced = ctx.db.fence_host(&ctx.p.host, &now)?;
    let mut bindings = Vec::with_capacity(rows.len());
    for a in rows {
        let Some(address) = a.address.clone() else { continue };
        let published = if publish.contains(a.id.as_str()) {
            let target = match address.kind {
                Kind::Human => address.clone(),
                _ => Address::new(Kind::Mailbox, &a.id, &ctx.p.host),
            };
            ctx.db.set_binding(&a.id, Some(&target))?;
            Some(Route { mailbox_id: a.id.clone(), route_generation: u64::try_from(a.binding_version + 1).unwrap_or(0) })
        } else {
            ctx.db.set_binding(&a.id, None)?;
            None
        };
        if a.pinned.is_some() {
            ctx.db.set_pin(&a.id, None)?;
        }
        bindings.push(ManifestRow { agent_id: a.id, name: a.name, address: address.to_string(), pin: a.pinned, published });
    }
    ctx.db.replace_endpoints(&ctx.p.host, "", &[], &now)?;
    ctx.db.set_host_mailbox_only(&ctx.p.host)?;
    ctx.db.announce(topics::FLEET);
    tracing::info!(host = %ctx.p.host, ?fenced, names = bindings.len(), published = publish.len(), "host migrated to v2");
    json(&Manifest { bindings })
}
