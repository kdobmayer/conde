//! Diagnostics.

use serde_json::{Value, json};

use crate::dispatch::{Ctx, OpResult};

pub fn doctor(ctx: &Ctx, _body: &Value) -> OpResult<Value> {
    let mut lines: Vec<Value> = Vec::new();
    let mut issue = |ok: bool, text: String| lines.push(json!({"ok": ok, "text": text}));
    let integrity = ctx.db.integrity_check()?;
    issue(integrity == "ok", format!("database: {integrity}"));
    let agents = ctx.db.count_bound_agents()?;
    issue(true, format!("registry: {agents} registered agent(s)"));
    if ctx.p.host == crate::util::FALLBACK_HOST {
        issue(false, format!(
            "host: the host id is '{}' because no hostname could be read; teammates' machines would collide \
             on it. Set CONDE_HOST to a unique name for this machine",
            ctx.p.host
        ));
    }
    for (name, n) in ctx.db.unbound_waiting()? {
        issue(false, format!(
            "unbound: {name} has {n} notification(s) waiting; the session that owns it should run: \
             conde register <name> (to change a name, use conde rename, which keeps its messages)"
        ));
    }
    let held = ctx.db.count_notifications(&["held"])?;
    issue(held == 0, format!("held: {held} notification(s) await their recipient user's approval"));
    let refused = ctx.db.count_notifications(&["denied", "refused"])?;
    issue(true, format!("refused: {refused} notification(s) denied or refused by recipients"));
    let issues = lines.iter().filter(|l| l["ok"] == false).count();
    Ok(json!({"checks": lines, "issues": issues}))
}
