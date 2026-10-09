#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;

use conde::address::Address;
use conde::bridge::Bridge;
use conde::dispatch::{Dispatcher, Principal};
use conde::notify::{Notifier, NotifyOutcome, Receipt, ReceiptStatus};
use conde::protocol::{ErrorCode, Request};
use conde::util::new_ulid;
use serde_json::{Value, json};

#[path = "../pg/mod.rs"]
mod pg;

pub struct Board {
    pub d: Arc<Dispatcher>,
    pub p: Principal,
}

pub type Reply = Result<Value, (ErrorCode, String)>;

impl Board {
    /// A board on SQLite whatever CONDE_TEST_PG says (SQLite fault-injection tests).
    pub fn sqlite() -> Board {
        let d = Arc::new(Dispatcher::open_in_memory().unwrap());
        let p = d.principal("krisz", "box", true).unwrap();
        Board { d, p }
    }

    pub fn new() -> Board {
        let d = Arc::new(match pg::fresh_database() {
            Some(url) => Dispatcher::connect(&url, conde::store::postgres::PgOpen::COMMAND).unwrap(),
            None => Dispatcher::open_in_memory().unwrap(),
        });
        let p = d.principal("krisz", "box", true).unwrap();
        Board { d, p }
    }

    /// Another owner on another host, same board.
    pub fn other(&self, user: &str, host: &str) -> Principal {
        self.d.principal(user, host, false).unwrap()
    }

    pub fn call_as(&self, p: &Principal, actor: &str, op: &str, body: Value) -> Reply {
        self.request(p, actor, None, op, body, Some(&new_ulid()))
    }

    pub fn request(&self, p: &Principal, actor: &str, from: Option<&str>, op: &str, body: Value, key: Option<&str>) -> Reply {
        let mut req = Request::new(new_ulid(), op, body);
        req.actor = Some(actor.to_string());
        req.from = from.map(Into::into);
        req.idempotency_key = key.map(Into::into);
        let resp = self.d.dispatch(p, &req);
        match resp.error {
            None => Ok(resp.result.unwrap()),
            Some(e) => Err((e.code, e.message)),
        }
    }

    pub fn call(&self, actor: &str, op: &str, body: Value) -> Reply {
        self.call_as(&self.p, actor, op, body)
    }

    pub fn ok(&self, actor: &str, op: &str, body: Value) -> Value {
        self.call(actor, op, body).unwrap_or_else(|e| panic!("{op} as {actor}: {e:?}"))
    }

    pub fn register(&self, actor: &str, name: &str) {
        self.ok(actor, "agent.register", json!({"name": name}));
    }

    pub fn send(&self, actor: &str, to: &str, text: &str) -> String {
        let target = if to.starts_with('#') { json!({"channel": to}) } else { json!({"agent": to}) };
        let r = self.ok(actor, "message.send", json!({"to": target, "text": text}));
        r["message_id"].as_str().unwrap().to_string()
    }

    pub fn bridge(&self, notifier: FakeNotifier) -> Bridge<FakeNotifier> {
        Bridge::new(self.d.clone(), self.d.bridge_principal(&self.p), notifier)
    }

    /// (status, kind) of every notification, keyed by `<message_id>.<recipient name>`.
    pub fn notifications(&self) -> Vec<(String, String, String)> {
        let mut rows: Vec<(String, String, String)> = self
            .d
            .with_conn(|tx| tx.notifications())
            .unwrap()
            .into_iter()
            .map(|n| {
                let name = n.recipient_name.split_once('/').map_or(n.recipient_name.clone(), |(_, n)| n.to_string());
                (format!("{}.{name}", n.message_id), n.kind, n.status)
            })
            .collect();
        rows.sort();
        rows
    }

    /// The id of the board owner's agent `name`.
    pub fn agent_id(&self, name: &str) -> String {
        self.d.with_conn(|tx| tx.agent_by_name(self.p.user_id, name)).unwrap().expect(name).id
    }

    pub fn message_count(&self) -> i64 {
        self.d.with_conn(|tx| tx.message_count()).unwrap()
    }

    pub fn status_of(&self, message_id: &str, name: &str) -> String {
        let key = format!("{message_id}.{name}");
        self.notifications()
            .into_iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, _, s)| s)
            .unwrap_or_else(|| "none".into())
    }
}

pub const ALICE: &str = "codex:t-alice";
pub const BOB: &str = "codex:t-bob";
pub const CAROL: &str = "claude:s-carol";
pub const HUMAN: &str = "human:krisz";

/// Records every push; answers with scripted outcomes (default Accepted) and
/// hands out queued receipts.
#[derive(Default)]
pub struct FakeNotifier {
    pub sent: Vec<(String, String, String)>,
    pub outcomes: VecDeque<NotifyOutcome>,
    pub receipts: Vec<Receipt>,
    pub dead: Vec<String>,
}

impl FakeNotifier {
    pub fn receipt(&mut self, attempt_id: &str, status: ReceiptStatus) {
        self.receipts.push(Receipt { attempt_id: attempt_id.to_string(), status });
    }

    pub fn last_attempt(&self) -> String {
        self.sent.last().expect("nothing sent").2.clone()
    }

    pub fn sent_to(&self, address_prefix: &str) -> Vec<&(String, String, String)> {
        self.sent.iter().filter(|(a, _, _)| a.starts_with(address_prefix)).collect()
    }
}

impl Notifier for FakeNotifier {
    fn notify(&mut self, address: &Address, text: &str, attempt_id: &str) -> NotifyOutcome {
        self.sent.push((address.to_string(), text.to_string(), attempt_id.to_string()));
        self.outcomes.pop_front().unwrap_or(NotifyOutcome::Accepted)
    }

    fn poll_receipts(&mut self) -> Vec<Receipt> {
        std::mem::take(&mut self.receipts)
    }

    fn is_live(&mut self, address: &Address) -> bool {
        !self.dead.contains(&address.to_string())
    }
}
