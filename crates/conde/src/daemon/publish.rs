//! The daemon's own ops (DESIGN-v2.md §5, §9): publication, the board list, forgetting an
//! alias, and the one-time migration. Each remote mutation is written as a `board_ops` row
//! before its first call and resumed with the same key after a crash or a lost answer.

use std::sync::Arc;

use serde_json::{Value, json};

use super::board::{self, BoardConn, OpOutcome, Status};
use super::{Core, Refusal};
use crate::address::{Address, Kind, normalize_name};
use crate::boards;
use crate::protocol::v2::{LOCAL_BOARD, Manifest, MigrateBody, PublishBody, UnpublishBody, op};
use crate::protocol::{ErrorCode, Request};
use crate::store::Tx;
use crate::store::local::{BoardOp, BoardOpKind, BoardOpState, LocalTx, Publication, PublicationState};
use crate::util::{iso_from_ms, new_ulid, now_ms};

fn refuse(code: ErrorCode, message: impl Into<String>) -> Refusal {
    (code, message.into())
}

fn internal(e: anyhow::Error) -> Refusal {
    (ErrorCode::Internal, format!("{e:#}"))
}

/// The board a daemon op names: its body's `board`, else `--board`.
fn board_of(core: &Core, req: &Request) -> Result<Arc<BoardConn>, Refusal> {
    let alias = req
        .board
        .as_deref()
        .or(req.body.get("board").and_then(Value::as_str))
        .ok_or_else(|| refuse(ErrorCode::BadRequest, "name the shared board: --board <alias>"))?;
    if alias == LOCAL_BOARD {
        return Err(refuse(ErrorCode::BadRequest, "the local board needs no publication; name a shared board"));
    }
    core.board(alias).ok_or_else(|| refuse(ErrorCode::NotFound, crate::surface::unknown_board(alias, &core.enrolled())))
}

/// Publication and the board's configuration are the human's.
fn require_human(core: &Core, req: &Request, what: &str) -> Result<(), Refusal> {
    let human = req
        .actor
        .as_deref()
        .and_then(|a| Address::parse_v1(a, &core.principal.host).ok())
        .is_some_and(|a| a.kind == Kind::Human);
    if human {
        Ok(())
    } else {
        Err(refuse(ErrorCode::Forbidden, format!("{what} is set by your human")))
    }
}

fn usable(conn: &BoardConn) -> Result<(), Refusal> {
    match conn.status() {
        Status::Running => Ok(()),
        Status::NotMigrated => Err(refuse(ErrorCode::Conflict, format!("{}: not migrated (run conde setup)", conn.alias))),
        Status::Stopped(why) | Status::Broken(why) => Err(refuse(ErrorCode::Conflict, format!("{}: {why}", conn.alias))),
    }
}

fn agent_named(core: &Core, name: &str) -> Result<crate::store::Agent, Refusal> {
    // `owner/name` in the caller's own namespace, or the bare name
    let name = name.rsplit('/').next().unwrap_or(name);
    let normalized = normalize_name(name).ok_or_else(|| refuse(ErrorCode::BadRequest, format!("invalid name '{name}'")))?;
    core.local
        .with_conn(|tx| tx.agent_by_name(core.principal.user_id, &normalized))
        .map_err(|e| refuse(ErrorCode::Internal, e.to_string()))?
        .ok_or_else(|| refuse(ErrorCode::NotFound, format!("no agent '{normalized}' on the local board")))
}

fn pending_refusal(name: &str, alias: &str, op: &BoardOp) -> Refusal {
    refuse(
        ErrorCode::Conflict,
        format!("{name} has a pending {} on {alias} (op {}); it completes when {alias} is reachable", op.kind.as_str(), op.op_id),
    )
}

/// `daemon.publish {agent, board, as?, take?}`: human only.
pub async fn publish(core: &Arc<Core>, req: &Request) -> Result<Value, Refusal> {
    require_human(core, req, "publication")?;
    let conn = board_of(core, req)?;
    usable(&conn)?;
    let alias = conn.alias.clone();
    let name = req.body.get("agent").and_then(Value::as_str).ok_or_else(|| refuse(ErrorCode::BadRequest, "missing string field 'agent'"))?;
    let agent = agent_named(core, name)?;
    let shared = match req.body.get("name").or(req.body.get("as")).and_then(Value::as_str) {
        Some(s) => normalize_name(s).ok_or_else(|| refuse(ErrorCode::BadRequest, format!("invalid name '{s}'")))?,
        None => agent.name.clone(),
    };
    let take = req.body.get("take").and_then(Value::as_bool).unwrap_or(false);
    let human = agent.address.as_ref().is_some_and(|a| a.kind == Kind::Human);
    let now = iso_from_ms(now_ms());
    let op = BoardOp {
        op_id: new_ulid(),
        board: alias.clone(),
        kind: BoardOpKind::Publish,
        agent_id: Some(agent.id.clone()),
        request: serde_json::to_value(PublishBody { name: shared.clone(), human, take }).unwrap_or_default(),
        state: BoardOpState::Pending,
        result: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let existing = core
        .local_tx(|tx| {
            if let Some(pending) = tx.pending_op_for(&alias, &agent.id)? {
                return Ok(Err(pending_refusal(&agent.name, &alias, &pending)));
            }
            match tx.publication(&alias, &agent.id)? {
                Some(p) if p.state == PublicationState::Published && p.shared_name == shared => return Ok(Ok(Some(p))),
                Some(p) => {
                    return Ok(Err(refuse(
                        ErrorCode::Conflict,
                        format!("{} is published on {alias} as {}; unpublish first", agent.name, p.shared_name),
                    )));
                }
                None => {}
            }
            if let Some(other) = tx.publication_by_name(&alias, &shared)? {
                let holder = tx.agent_by_id(&other.agent_id)?.map(|a| a.name).unwrap_or(other.agent_id);
                return Ok(Err(refuse(ErrorCode::Conflict, format!("{shared} on {alias} is already published for {holder}"))));
            }
            tx.insert_op(&op)?;
            tx.insert_publication(&Publication {
                board: alias.clone(),
                agent_id: agent.id.clone(),
                shared_name: shared.clone(),
                mailbox_id: None,
                route_generation: None,
                state: PublicationState::Publishing,
                updated_at: now.clone(),
            })?;
            Ok(Ok(None))
        })
        .map_err(internal)??;
    if let Some(p) = existing {
        return Ok(json!({"board": alias, "agent": agent.name, "name": p.shared_name, "state": "published",
                         "mailbox_id": p.mailbox_id, "route_generation": p.route_generation, "unchanged": true}));
    }
    let outcome = board::send_op(&conn, &op, wait(core)).await;
    let state = board::finish_op(core, &conn, &op, outcome)?;
    let p = board::publication_of(core, &alias, &agent.id);
    Ok(json!({"board": alias, "agent": agent.name, "name": shared, "state": state, "op_id": op.op_id,
              "owner": conn.owner(),
              "mailbox_id": p.as_ref().and_then(|p| p.mailbox_id.clone()),
              "route_generation": p.as_ref().and_then(|p| p.route_generation)}))
}

fn wait(core: &Core) -> std::time::Duration {
    core.mux.tuning.call_timeout.min(std::time::Duration::from_secs(10))
}

/// `daemon.unpublish {agent, board}`: the human, or the agent itself.
pub async fn unpublish(core: &Arc<Core>, req: &Request) -> Result<Value, Refusal> {
    let conn = board_of(core, req)?;
    usable(&conn)?;
    let alias = conn.alias.clone();
    let name = req.body.get("agent").and_then(Value::as_str).ok_or_else(|| refuse(ErrorCode::BadRequest, "missing string field 'agent'"))?;
    let agent = agent_named(core, name)?;
    if require_human(core, req, "unpublish").is_err() {
        // Shrinking exposure is always allowed to the agent itself.
        let me = core.local_identity(req)?;
        if me.id != agent.id {
            return Err(refuse(ErrorCode::Forbidden, format!("{} may be unpublished by your human or by itself", agent.name)));
        }
    }
    let now = iso_from_ms(now_ms());
    let op = core
        .local_tx(|tx| {
            if let Some(pending) = tx.pending_op_for(&alias, &agent.id)? {
                return Ok(Err(pending_refusal(&agent.name, &alias, &pending)));
            }
            let Some(p) = tx.publication(&alias, &agent.id)? else {
                return Ok(Err(refuse(ErrorCode::NotFound, format!("{} is not published on {alias}", agent.name))));
            };
            let (Some(mailbox_id), Some(generation)) = (p.mailbox_id.clone(), p.route_generation) else {
                return Ok(Err(refuse(ErrorCode::Conflict, format!("{} is still publishing on {alias}", agent.name))));
            };
            let op = BoardOp {
                op_id: new_ulid(),
                board: alias.clone(),
                kind: BoardOpKind::Unpublish,
                agent_id: Some(agent.id.clone()),
                request: serde_json::to_value(UnpublishBody { mailbox_id: mailbox_id.clone(), expected_generation: generation })
                    .unwrap_or_default(),
                state: BoardOpState::Pending,
                result: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            tx.insert_op(&op)?;
            tx.update_publication(&alias, &agent.id, Some((&mailbox_id, generation)), PublicationState::Unpublishing, &now)?;
            Ok(Ok(op))
        })
        .map_err(internal)??;
    let outcome = board::send_op(&conn, &op, wait(core)).await;
    let state = board::finish_op(core, &conn, &op, outcome)?;
    Ok(json!({"board": alias, "agent": agent.name, "state": state, "op_id": op.op_id}))
}

/// `board.list {}`: the local board and every enrolled board with its live state.
pub fn list(core: &Core) -> Result<Value, Refusal> {
    let default = core.default_board();
    let pubs = core.local_tx(|tx| Ok(tx.publications(None)?)).map_err(internal)?;
    let ops = core.local_tx(|tx| Ok(tx.pending_ops(None)?)).map_err(internal)?;
    let agents = core.local.with_conn(|tx| tx.count_bound_agents()).unwrap_or_default();
    let mut rows = vec![json!({"board": LOCAL_BOARD, "path": core.db_path.display().to_string(), "agents": agents,
                               "default": default == LOCAL_BOARD, "state": "local"})];
    for conn in core.boards() {
        let published: Vec<Value> = pubs
            .iter()
            .filter(|p| p.board == conn.alias)
            .map(|p| {
                let agent = core.local.with_conn(|tx| tx.agent_by_id(&p.agent_id)).ok().flatten().map(|a| a.name).unwrap_or_default();
                json!({"agent": agent, "name": format!("{}/{}", conn.owner(), p.shared_name), "state": p.state.as_str()})
            })
            .collect();
        rows.push(json!({
            "board": conn.alias,
            "server": conn.config.as_ref().map(|c| c.server.clone()),
            "host": conn.host(),
            "owner": conn.owner(),
            "state": core.board_state(&conn),
            "default": default == conn.alias,
            "published": published,
            "pending_ops": ops.iter().filter(|o| o.board == conn.alias).map(|o| json!({"op_id": o.op_id, "kind": o.kind.as_str()})).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({"boards": rows}))
}

/// `board.forget` (alias in the envelope): human only; never touches the server.
pub fn forget(core: &Arc<Core>, req: &Request) -> Result<Value, Refusal> {
    require_human(core, req, "a board's configuration")?;
    let conn = board_of(core, req)?;
    let alias = conn.alias.clone();
    // From here no late answer applies to this board and nothing starts it again.
    conn.forgotten.store(true, std::sync::atomic::Ordering::SeqCst);
    // A default that names this board would point bare names nowhere: it goes back to the
    // local board first, and the answer says so.
    let default_reset = core.default_board() == alias;
    if default_reset {
        let config = &core.config_dir;
        boards::write_default(&config.join("default-board"), &config.join("boards"), LOCAL_BOARD).map_err(internal)?;
    }
    conn.shutdown();
    core.mux.remove_board(&alias);
    core.remove_board(&alias);
    let (publications, ops) = core.local_tx(|tx| Ok(tx.forget_board(&alias)?)).map_err(internal)?;
    let file = boards::forget_in(&core.config_dir, &alias).map_err(internal)?;
    Ok(json!({"board": alias, "file_removed": file, "publications_removed": publications, "ops_removed": ops,
              "default_reset": default_reset, "default": core.default_board()}))
}

/// `host.migrate_preview` (alias in the envelope): the board's read-only preview, over HTTPS.
pub async fn migrate_preview(core: &Arc<Core>, req: &Request) -> Result<Value, Refusal> {
    require_human(core, req, "the migration")?;
    let conn = board_of(core, req)?;
    let resp = board::rpc(&conn, json!({"op": op::HOST_MIGRATE_PREVIEW, "body": {}}), core.mux.tuning.call_timeout)
        .await
        .map_err(|why| refuse(ErrorCode::Internal, why))?;
    match (resp.result, resp.error) {
        (Some(r), None) => Ok(r),
        (_, Some(e)) => Err(refuse(e.code, format!("{}: {}", conn.alias, e.message))),
        _ => Err(refuse(ErrorCode::Internal, "empty response")),
    }
}

/// `host.migrate_v2 {publish: [agent_id]}` (alias in the envelope): human only. Writes the op,
/// applies it on the board over HTTPS, applies the manifest locally, then connects. Answers the
/// manifest with the local conflicts; an op the board did not answer stays pending (an error).
pub async fn migrate(core: &Arc<Core>, req: &Request) -> Result<Value, Refusal> {
    require_human(core, req, "the migration")?;
    let conn = board_of(core, req)?;
    let alias = conn.alias.clone();
    let publish: Vec<String> = req
        .body
        .get("publish")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let now = iso_from_ms(now_ms());
    let op = core
        .local_tx(|tx| {
            if let Some(existing) = tx.migrate_op(&alias)? {
                return Ok(existing);
            }
            let op = BoardOp {
                op_id: new_ulid(),
                board: alias.clone(),
                kind: BoardOpKind::Migrate,
                agent_id: None,
                request: serde_json::to_value(MigrateBody { publish }).unwrap_or_default(),
                state: BoardOpState::Pending,
                result: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            tx.insert_op(&op)?;
            Ok(op)
        })
        .map_err(internal)?;
    if op.state == BoardOpState::Done {
        return migrated(core, &conn, &op.op_id);
    }
    let outcome = board::send_op(&conn, &op, core.mux.tuning.call_timeout).await;
    if let OpOutcome::Pending(why) = &outcome {
        return Err(refuse(
            ErrorCode::Internal,
            format!("{alias}: the switch is pending (op {}): {why}; the daemon resumes it when {alias} answers", op.op_id),
        ));
    }
    board::finish_op(core, &conn, &op, outcome)?;
    migrated(core, &conn, &op.op_id)
}

/// A done migration's answer: the board's manifest, the local conflicts, and the connection
/// started.
fn migrated(core: &Arc<Core>, conn: &Arc<BoardConn>, op_id: &str) -> Result<Value, Refusal> {
    if conn.status() == Status::NotMigrated {
        board::start(core, conn);
    }
    let stored = core.local_tx(|tx| Ok(tx.op(op_id)?)).map_err(internal)?.and_then(|o| o.result).unwrap_or_default();
    let mut out = stored["manifest"].clone();
    if !out.is_object() {
        out = json!({"bindings": []});
    }
    out["conflicts"] = stored["conflicts"].clone();
    out["board"] = json!(conn.alias);
    out["op_id"] = json!(op_id);
    Ok(out)
}

/// `board.info` (alias in the envelope): the board's answer over HTTPS, before or after the
/// migration. A board that reports this host `mailbox_only` counts as migrated here too (a
/// re-enrollment after `conde boards --forget`), and its connection starts.
pub async fn board_info(core: &Arc<Core>, req: &Request) -> Result<Value, Refusal> {
    let conn = board_of(core, req)?;
    let info = board::fetch_info(&conn, core.mux.tuning.call_timeout).await.map_err(|why| refuse(ErrorCode::Internal, why))?;
    if info.mailbox_only {
        board::adopt_migrated(core, &conn).map_err(internal)?;
    }
    serde_json::to_value(&info).map_err(|e| refuse(ErrorCode::Internal, e.to_string()))
}

/// The local apply of a migration manifest, in one transaction (idempotent): every former
/// binding is registered locally at its address with its pin, unless the local name is bound
/// to another live session (a conflict, kept and listed); published rows become publications.
pub fn apply_manifest(core: &Core, conn: &BoardConn, op: &BoardOp, result: &Value) -> Result<String, Refusal> {
    let manifest: Manifest = serde_json::from_value(result.clone())
        .map_err(|e| refuse(ErrorCode::Internal, format!("{}: the migration answer is not a manifest: {e}", conn.alias)))?;
    let alias = &conn.alias;
    let host = core.principal.host.clone();
    let owner = core.principal.user_id;
    let now = iso_from_ms(now_ms());
    let live = |a: &Address| core.live(a);
    let applied = core.local_tx(|tx| {
        // Only for the board's own pending op: a forgotten board's late answer applies nothing.
        if conn.forgotten.load(std::sync::atomic::Ordering::SeqCst)
            || tx.op(&op.op_id)?.is_none_or(|o| o.state != BoardOpState::Pending)
        {
            return Ok(false);
        }
        let mut conflicts = Vec::new();
        for row in &manifest.bindings {
            let Some(name) = normalize_name(&row.name) else { continue };
            let former = Address::parse_v1(&row.address, &host).ok().map(|a| Address::new(a.kind, &a.target, &host));
            let agent = match tx.agent_by_name(owner, &name)? {
                Some(a) => a,
                None => {
                    tx.insert_agent(&new_ulid(), owner, &name)?;
                    tx.agent_by_name(owner, &name)?.ok_or_else(|| anyhow::anyhow!("agent {name} not created"))?
                }
            };
            match (&agent.address, &former) {
                (Some(current), Some(f)) if current != f && live(current) => {
                    conflicts.push(json!({"name": name, "address": f.to_string(), "local": current.to_string()}));
                }
                (current, Some(f)) if current.as_ref() != Some(f) => tx.set_binding(&agent.id, Some(f))?,
                _ => {}
            }
            if agent.pinned.is_none()
                && let Some(pin) = &row.pin
            {
                tx.set_pin(&agent.id, Some(pin))?;
            }
            if let Some(route) = &row.published {
                let publication = Publication {
                    board: alias.clone(),
                    agent_id: agent.id.clone(),
                    shared_name: name.clone(),
                    mailbox_id: Some(route.mailbox_id.clone()),
                    route_generation: Some(route.route_generation),
                    state: PublicationState::Published,
                    updated_at: now.clone(),
                };
                if tx.publication(alias, &agent.id)?.is_some() {
                    tx.update_publication(alias, &agent.id, Some((&route.mailbox_id, route.route_generation)), PublicationState::Published, &now)?;
                } else {
                    tx.insert_publication(&publication)?;
                }
            }
        }
        let stored = json!({"manifest": result, "conflicts": conflicts});
        tx.finish_op(&op.op_id, BoardOpState::Done, Some(&stored), &now)?;
        Ok(true)
    })
    .map_err(internal)?;
    Ok(if applied { "migrated" } else { "forgotten" }.into())
}
