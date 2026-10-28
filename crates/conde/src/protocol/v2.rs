//! The v2 ops between a daemon and a shared board (DESIGN-v2.md §6, §9) and the board
//! labels of daemon responses (§3). Each op is a v1 request frame (`op`, `body`); the
//! mutations carry the local `board_ops.op_id` as `idempotency_key`, so a resumed op
//! replays the board's stored answer.

use conde_remote::Route;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Op names.
pub mod op {
    /// Query: [`BoardInfo`](super::BoardInfo).
    pub const BOARD_INFO: &str = "board.info";
    /// Mutation, idempotent on the op id: [`PublishBody`](super::PublishBody) -> [`Published`](super::Published).
    pub const MAILBOX_PUBLISH: &str = "mailbox.publish";
    /// Mutation, idempotent on the op id: [`UnpublishBody`](super::UnpublishBody) -> [`Unpublished`](super::Unpublished).
    pub const MAILBOX_UNPUBLISH: &str = "mailbox.unpublish";
    /// Query: [`Mine`](super::Mine).
    pub const MAILBOX_MINE: &str = "mailbox.mine";
    /// Query, read-only: [`MigratePreview`](super::MigratePreview).
    pub const HOST_MIGRATE_PREVIEW: &str = "host.migrate_preview";
    /// Mutation, idempotent on the op id: [`MigrateBody`](super::MigrateBody) -> [`Manifest`](super::Manifest).
    pub const HOST_MIGRATE_V2: &str = "host.migrate_v2";
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// `board.info` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardInfo {
    /// The board's advertised name (`CONDE_BOARD_NAME`), offered as the default alias.
    pub name: String,
    /// The calling host's account on the board.
    pub owner: String,
    /// The calling host.
    pub host: String,
    pub mailbox_only: bool,
    /// The workspace's welcome, defaults filled; None from a server before the welcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome: Option<conde_remote::web::Welcome>,
}

/// `mailbox.publish` body: bind `<caller>/name` on the calling host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishBody {
    pub name: String,
    /// Publish the caller's human name (address `human:<user>@<host>`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub human: bool,
    /// Take the name over from another host of the same owner.
    #[serde(default, skip_serializing_if = "is_false")]
    pub take: bool,
}

/// `mailbox.publish` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    pub name: String,
    pub mailbox_id: String,
    pub route_generation: u64,
    /// The host's route revision after the publish (see [`conde_remote::Routes::revision`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

/// `mailbox.unpublish` body: unbind only if bound to the calling host at this generation,
/// else `conflict`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnpublishBody {
    pub mailbox_id: String,
    pub expected_generation: u64,
}

/// `mailbox.unpublish` result: the generation after the unbind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unpublished {
    pub mailbox_id: String,
    pub route_generation: u64,
    /// The host's route revision after the unpublish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

/// `mailbox.mine` result: the mailboxes bound to the calling host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mine {
    pub mailboxes: Vec<MineMailbox>,
    /// The host's route revision this list was read at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MineMailbox {
    pub name: String,
    pub mailbox_id: String,
    pub route_generation: u64,
}

/// `host.migrate_preview` result: the calling host's bound rows with their evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigratePreview {
    pub rows: Vec<PreviewRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewRow {
    pub agent_id: String,
    /// The name, without the owner.
    pub name: String,
    /// The current harness (or human) address.
    pub address: String,
    pub pin: Option<String>,
    /// Has a cross-owner message relationship.
    pub candidate: bool,
    pub evidence: Vec<Evidence>,
}

/// One cross-owner relationship of a row, per counterpart and source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub via: Via,
    /// The other owner's agent (`owner/name`) for a DM, `#channel` for a channel.
    pub with: String,
    pub source: EvidenceSource,
    pub messages: u64,
    pub last_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Dm,
    Channel,
}

/// Whether the messages are rows imported from kond or later central traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceSource {
    Imported,
    Later,
}

/// `host.migrate_v2` body: the confirmed agent ids to publish.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateBody {
    pub publish: Vec<String>,
}

/// `host.migrate_v2` result: every former binding of the host, before conversion.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub bindings: Vec<ManifestRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRow {
    pub agent_id: String,
    /// The name, without the owner.
    pub name: String,
    /// The former harness (or human) address.
    pub address: String,
    pub pin: Option<String>,
    /// The mailbox and its generation when the row was published, else null.
    pub published: Option<Route>,
}

/// The local board's label; bare message ids belong to it.
pub const LOCAL_BOARD: &str = "local";

/// The field naming a row's board in a daemon response.
pub const BOARD_FIELD: &str = "board";

/// Labels one response row (a JSON object) with its board: `"board": "<alias>"`.
pub fn label(row: &mut Value, board: &str) {
    if let Some(obj) = row.as_object_mut() {
        obj.insert(BOARD_FIELD.into(), Value::String(board.into()));
    }
}

/// One board's part in an aggregate view: `"boards": [{"board":"dakai","available":false}]`.
/// A board that did not answer in time is listed unavailable and contributes no rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardPart {
    pub board: String,
    pub available: bool,
}
