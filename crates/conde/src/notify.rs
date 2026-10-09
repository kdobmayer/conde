//! The seam between the board and harness transports.
//!
//! The daemon's bridge loop owns the delivery state machine; a `Notifier`
//! only pushes one notification into one harness and reports what the
//! harness said about it, keyed by the attempt id it was given.

use crate::address::Address;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifyOutcome {
    /// Handed over; no receipt yet (provisional acceptance, revisable).
    Accepted,
    /// The recipient's harness parked it pending its user's decision.
    Held,
    /// Not delivered; retried after the backoff.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    Held,
    Delivered,
    Expired,
    Denied,
    Refused,
    Dropped { reason: String },
}

impl NotifyOutcome {
    /// The `bridge.report` outcome and reason for this result.
    pub fn report(&self) -> (&str, Option<&str>) {
        match self {
            NotifyOutcome::Accepted => ("accepted", None),
            NotifyOutcome::Held => ("held", None),
            NotifyOutcome::Failed(reason) => ("failed", Some(reason)),
        }
    }
}

impl ReceiptStatus {
    /// The `bridge.report` outcome and reason for this receipt.
    pub fn report(&self) -> (&str, Option<&str>) {
        match self {
            ReceiptStatus::Held => ("held", None),
            ReceiptStatus::Delivered => ("delivered", None),
            ReceiptStatus::Expired => ("expired", None),
            ReceiptStatus::Denied => ("denied", None),
            ReceiptStatus::Refused => ("refused", None),
            ReceiptStatus::Dropped { reason } => ("dropped", Some(reason)),
        }
    }
}

/// A late harness receipt for one earlier `notify` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub attempt_id: String,
    pub status: ReceiptStatus,
}

pub trait Notifier {
    fn notify(&mut self, address: &Address, text: &str, attempt_id: &str) -> NotifyOutcome;
    fn poll_receipts(&mut self) -> Vec<Receipt>;
    /// Whether the recipient incarnation still exists; a hold on a gone
    /// session is abandoned and re-attempted. Transports that cannot tell
    /// keep the default.
    fn is_live(&mut self, _address: &Address) -> bool {
        true
    }
}

/// Logs every notification and accepts it: local mode without transports.
#[derive(Debug, Default)]
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&mut self, address: &Address, text: &str, attempt_id: &str) -> NotifyOutcome {
        tracing::info!(%address, attempt = attempt_id, "notify: {text}");
        NotifyOutcome::Accepted
    }

    fn poll_receipts(&mut self) -> Vec<Receipt> {
        Vec::new()
    }
}
