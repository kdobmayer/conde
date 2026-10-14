//! The bridge loop: claim due notifications through the dispatcher, push them
//! through a `Notifier`, and report outcomes and late receipts back.

use std::sync::Arc;

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use crate::address::Address;
use crate::dispatch::{Dispatcher, Principal};
use crate::notify::Notifier;
use crate::protocol::Request;

pub struct Bridge<N: Notifier> {
    dispatcher: Arc<Dispatcher>,
    principal: Principal,
    notifier: N,
    seq: u64,
}

impl<N: Notifier> Bridge<N> {
    pub fn new(dispatcher: Arc<Dispatcher>, principal: Principal, notifier: N) -> Bridge<N> {
        assert!(principal.bridge_instance.is_some(), "a bridge needs a bridge principal");
        Bridge { dispatcher, principal, notifier, seq: 0 }
    }

    pub fn instance(&self) -> &str {
        self.principal.bridge_instance.as_deref().unwrap_or_default()
    }

    pub fn notifier(&mut self) -> &mut N {
        &mut self.notifier
    }

    fn call(&mut self, op: &str, body: Value) -> anyhow::Result<Value> {
        self.seq += 1;
        let req = Request::new(format!("bridge-{}", self.seq), op, body);
        let resp = self.dispatcher.dispatch(&self.principal, &req);
        match (resp.ok, resp.result, resp.error) {
            (true, Some(result), _) => Ok(result),
            (_, _, Some(e)) => Err(anyhow!("{op}: {}", e.message)),
            _ => Err(anyhow!("{op}: empty response")),
        }
    }

    /// Report one outcome; a refused report (not this bridge's attempt) is
    /// logged and skipped so it cannot stall the other outcomes of the pass.
    fn report(&mut self, attempt_id: &str, outcome: &str, reason: Option<&str>) -> anyhow::Result<()> {
        let body = json!({"attempt_id": attempt_id, "outcome": outcome, "reason": reason});
        let result = match self.call("bridge.report", body) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(attempt = attempt_id, outcome, "report refused: {e:#}");
                return Ok(());
            }
        };
        match result.get("ignored").and_then(Value::as_str) {
            Some(why) => tracing::info!(attempt = attempt_id, outcome, "receipt ignored: {why}"),
            None => tracing::info!(
                attempt = attempt_id,
                notification = result["notification_id"].as_str().unwrap_or_default(),
                outcome,
                reason = reason.unwrap_or_default(),
                "attempt settled"
            ),
        }
        Ok(())
    }

    fn settle_receipts(&mut self) -> anyhow::Result<()> {
        for receipt in self.notifier.poll_receipts() {
            let (outcome, reason) = receipt.status.report();
            self.report(&receipt.attempt_id, outcome, reason)?;
        }
        Ok(())
    }

    /// One pass: late receipts, liveness of open holds, then due notifications.
    pub fn pass(&mut self) -> anyhow::Result<()> {
        self.settle_receipts()?;
        let claim = self.call("bridge.claim", json!({"limit": 100}))?;
        for held in claim["held"].as_array().into_iter().flatten() {
            let (Some(attempt), Some(address)) = (held["attempt_id"].as_str(), held["address"].as_str()) else {
                continue;
            };
            let address = Address::parse(address, &self.principal.host).context("held attempt address")?;
            if !self.notifier.is_live(&address) {
                self.report(attempt, "abandoned", Some("recipient session gone"))?;
            }
        }
        for job in claim["jobs"].as_array().into_iter().flatten() {
            let attempt = job["attempt_id"].as_str().unwrap_or_default();
            let text = job["text"].as_str().unwrap_or_default();
            let address = Address::parse(job["address"].as_str().unwrap_or_default(), &self.principal.host)
                .context("job address")?;
            let result = self.notifier.notify(&address, text, attempt);
            let (outcome, reason) = result.report();
            self.report(attempt, outcome, reason)?;
        }
        self.settle_receipts()
    }
}
