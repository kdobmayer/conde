//! Delivery into the harness sessions on this host: Claude Code over its
//! session socket (receipts come back to one long-lived reply socket), Codex
//! over the app-server (falling back to `codex queue`).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use conde_transport::{
    ClaudeSender, ClaudeSession, CodexOutcome, ReceiptStatus as Wire, codex_control_socket, codex_home,
    codex_notify_with_id, discover_claude, msg_id_for,
};

use crate::address::{Address, Kind};
use crate::notify::{Notifier, NotifyOutcome, Receipt, ReceiptStatus};

/// How many sent frames keep their msg_id -> attempt mapping for late receipts.
const REMEMBERED: usize = 65_536;
/// After a send, how long a pass waits for an immediate receipt (held,
/// refused) before the provisional acceptance stands; later receipts revise it.
pub const RECEIPT_WAIT: Duration = Duration::from_millis(300);

pub struct HarnessNotifier {
    claude_config: Option<PathBuf>,
    /// One reply identity per recipients' socket directory, for the process lifetime.
    senders: HashMap<PathBuf, ClaudeSender>,
    attempts: HashMap<String, String>,
    order: VecDeque<String>,
    receipt_wait: Duration,
    sent_since_poll: bool,
}

impl HarnessNotifier {
    /// `claude_config` overrides `$CLAUDE_CONFIG_DIR` / `~/.claude`.
    pub fn new(claude_config: Option<PathBuf>) -> HarnessNotifier {
        HarnessNotifier {
            claude_config,
            senders: HashMap::new(),
            attempts: HashMap::new(),
            order: VecDeque::new(),
            receipt_wait: RECEIPT_WAIT,
            sent_since_poll: false,
        }
    }

    pub fn claude_sessions(&self) -> Vec<ClaudeSession> {
        discover_claude(self.claude_config.as_deref())
    }

    /// Session ids of the live Claude sessions (the bridge's endpoint list).
    pub fn live_claude(&self) -> Vec<String> {
        self.claude_sessions().into_iter().map(|s| s.session_id).collect()
    }

    fn remember(&mut self, msg_id: String, attempt_id: &str) {
        if self.attempts.insert(msg_id.clone(), attempt_id.to_string()).is_none() {
            self.order.push_back(msg_id);
            if self.order.len() > REMEMBERED
                && let Some(old) = self.order.pop_front()
            {
                self.attempts.remove(&old);
            }
        }
    }

    fn claude(&mut self, session_id: &str, text: &str, attempt_id: &str) -> NotifyOutcome {
        // Registry targets are session ids; a session *named* like one must not match.
        let Some(session) = self.claude_sessions().into_iter().find(|s| s.session_id == session_id) else {
            return NotifyOutcome::Failed(format!("no live Claude session {session_id}"));
        };
        let dir = session.sock.parent().map(Path::to_path_buf).unwrap_or_default();
        if !self.senders.contains_key(&dir) {
            match ClaudeSender::new(&dir) {
                Ok(sender) => {
                    tracing::info!(reply = sender.address(), "claude reply socket bound");
                    self.senders.insert(dir.clone(), sender);
                }
                Err(e) => return NotifyOutcome::Failed(format!("cannot bind a reply socket in {}: {e}", dir.display())),
            }
        }
        let msg_id = msg_id_for(attempt_id);
        self.remember(msg_id.clone(), attempt_id);
        match self.senders[&dir].send(&session, text, &msg_id) {
            Ok(()) => {
                self.sent_since_poll = true;
                NotifyOutcome::Accepted
            }
            Err(e) => NotifyOutcome::Failed(format!("cannot deliver to {}: {e}", session.sock.display())),
        }
    }
}

fn status(r: &conde_transport::Receipt) -> Option<ReceiptStatus> {
    Some(match &r.status {
        Wire::Held => ReceiptStatus::Held,
        Wire::Delivered => ReceiptStatus::Delivered,
        Wire::Expired => ReceiptStatus::Expired,
        Wire::Denied => ReceiptStatus::Denied,
        Wire::Refused => ReceiptStatus::Refused,
        Wire::Dropped => ReceiptStatus::Dropped { reason: r.drop_reason.clone().unwrap_or_default() },
        Wire::Other(_) => return None,
    })
}

impl Notifier for HarnessNotifier {
    fn notify(&mut self, address: &Address, text: &str, attempt_id: &str) -> NotifyOutcome {
        tracing::info!(%address, attempt = attempt_id, "notify: {text}");
        let outcome = match address.kind {
            Kind::Claude => self.claude(&address.target, text, attempt_id),
            Kind::Codex => match codex_notify_with_id(&address.target, text, Some(attempt_id)) {
                Ok(CodexOutcome::Accepted { .. }) => NotifyOutcome::Accepted,
                // Queued for the thread's next turn: parked like a hold, settled by the read.
                Ok(CodexOutcome::Queued) => NotifyOutcome::Held,
                Err(e) => NotifyOutcome::Failed(e.to_string()),
            },
            Kind::Human => NotifyOutcome::Failed("no push transport for humans".into()),
            Kind::Mailbox => NotifyOutcome::Failed("a mailbox is not a harness".into()),
        };
        if let NotifyOutcome::Failed(why) = &outcome {
            tracing::warn!(%address, attempt = attempt_id, "notify failed: {why}");
        }
        outcome
    }

    fn poll_receipts(&mut self) -> Vec<Receipt> {
        let mut wait = if std::mem::take(&mut self.sent_since_poll) { self.receipt_wait } else { Duration::ZERO };
        let mut out = Vec::new();
        for sender in self.senders.values() {
            for r in sender.receipts(wait) {
                match (self.attempts.get(&r.msg_id), status(&r)) {
                    (Some(attempt), Some(status)) => out.push(Receipt { attempt_id: attempt.clone(), status }),
                    (None, _) => tracing::debug!(msg_id = r.msg_id, "receipt for an unknown frame"),
                    (_, None) => tracing::warn!(msg_id = r.msg_id, status = r.status.as_str(), "unknown receipt status"),
                }
            }
            wait = Duration::ZERO;
        }
        out
    }

    /// Claude: the session is published and its socket exists. Codex: a thread
    /// id stays resumable, so it is always live (a queued notification waits
    /// for the thread's next turn; abandoning it would only queue it again).
    fn is_live(&mut self, address: &Address) -> bool {
        match address.kind {
            Kind::Claude => self.claude_sessions().iter().any(|s| s.session_id == address.target),
            Kind::Codex | Kind::Human => true,
            Kind::Mailbox => false,
        }
    }
}

/// Where Claude publishes its sessions: `<config>/sessions`.
pub fn claude_sessions_dir(claude_config: Option<&Path>) -> Option<PathBuf> {
    let config = match claude_config {
        Some(c) => c.to_path_buf(),
        None => match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
            Some(c) => PathBuf::from(c),
            None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?).join(".claude"),
        },
    };
    Some(config.join("sessions"))
}

/// Live checks of this host's harness side, as the daemon sees it, for the
/// caller `actor` (`kind:target`): (ok, text) lines for `conde doctor`.
pub fn doctor_checks(notifier: &HarnessNotifier, actor: Option<&str>) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let caller = actor.and_then(|a| a.split_once(':'));
    let is = |kind: &str| caller.is_some_and(|(k, _)| k == kind);
    match claude_sessions_dir(notifier.claude_config.as_deref()) {
        None => out.push((!is("claude"), "claude: cannot locate the sessions dir (no HOME)".to_string())),
        Some(dir) => match std::fs::read_dir(&dir) {
            Ok(_) => {
                let live = notifier.claude_sessions();
                out.push((true, format!("claude: sessions dir {} readable, {} live session(s)", dir.display(), live.len())));
                if let Some((_, sid)) = caller.filter(|(k, _)| *k == "claude") {
                    let found = live.iter().any(|s| s.session_id == sid);
                    out.push((
                        found,
                        if found {
                            format!("claude: this session {sid} is discovered; notifications can reach it")
                        } else {
                            format!(
                                "claude: this session {sid} is not discovered in {} (no live socket?); notifications cannot reach it",
                                dir.display()
                            )
                        },
                    ));
                }
            }
            Err(e) => out.push((!is("claude"), format!("claude: sessions dir {} not readable: {e}", dir.display()))),
        },
    }
    match codex_home().map(|h| codex_control_socket(&h)) {
        Some(sock) if sock.exists() => {
            out.push((true, format!("codex: app-server control socket present at {}", sock.display())))
        }
        Some(sock) => out.push((
            !is("codex"),
            format!(
                "codex: no app-server control socket at {}; codex notifications fall back to `codex queue` (picked up at the next turn)",
                sock.display()
            ),
        )),
        None => out.push((!is("codex"), "codex: cannot locate the codex home (CODEX_HOME and HOME unset)".to_string())),
    }
    out
}
