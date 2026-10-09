//! One-off history tools of the v2 switch (DESIGN-v2 section 10), removed after H4.
//!
//! H1 `conde history inventory`: classifies the rows of a board that involve one owner from the
//! read-only extraction (`history_extract.sql`, metadata and hashes only) and a kond snapshot,
//! and writes the frozen manifest:
//! - (a) imported: the message id is in the kond snapshot;
//! - (b) owner-internal: a DM between two of the owner's agents, or a post in a channel whose
//!   every sender ever and every current member is the owner's;
//! - (c) shared: everything else.
//!
//! H3 `conde history copy`: copies the manifest's class (b) rows by their frozen ids from the
//! central board (`history.copy`) into the local board the H2 import restored, in one SQLite
//! transaction, and validates them by hashes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use crate::import::{Snapshot, rows_digest};
use crate::protocol::{Request, Response};
use crate::store::sqlite::SqliteStore;
use crate::store::{Channel, ImportedAgent, Mode, NewMessage, Store, Tx};
use crate::util::{new_ulid, sha256_hex};

/// The extraction query (Postgres), run read-only in one snapshot with `conde.owner` set.
pub const EXTRACT_SQL: &str = include_str!("history_extract.sql");

// --- extraction ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct XAgent {
    pub owner: String,
    pub name: String,
    pub kind: Option<String>,
    pub host: Option<String>,
    pub pinned: bool,
}

impl XAgent {
    pub fn canonical(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Clone, Debug)]
pub struct XMessage {
    pub id: String,
    pub seq: i64,
    pub sender: String,
    pub recipient: Option<String>,
    pub channel: Option<String>,
    pub reply_to: Option<String>,
    pub created_at: String,
    pub sha256: String,
    pub bytes: i64,
}

#[derive(Clone, Debug)]
pub struct XAttachment {
    pub id: String,
    pub message_id: String,
    pub filename: String,
    pub size: i64,
    pub sha256: String,
}

/// The extraction's lines, checked against its own count and digest.
#[derive(Debug, Default)]
pub struct Extract {
    pub owner: String,
    /// The board's max commit_seq when the extraction ran (the freeze point).
    pub freeze_seq: i64,
    pub board_messages: i64,
    pub taken_at: String,
    pub lines: usize,
    pub digest: String,
    pub agents: BTreeMap<String, XAgent>,
    /// channel id -> (name, creator agent id)
    pub channels: BTreeMap<String, (String, String)>,
    /// channel id -> member agent id -> read_seq
    pub members: BTreeMap<String, BTreeMap<String, i64>>,
    pub messages: BTreeMap<String, XMessage>,
    pub attachments: Vec<XAttachment>,
    /// (message id, agent id, read_at)
    pub reads: Vec<(String, String, String)>,
    /// (message id, actor id, emoji, present)
    pub reactions: Vec<(String, String, String, bool)>,
    /// (host id, has an unrevoked key)
    pub hosts: Vec<(String, bool)>,
}

fn field<'a>(row: &'a [Value], i: usize, line: &str) -> anyhow::Result<&'a Value> {
    row.get(i).ok_or_else(|| anyhow!("short line: {line}"))
}

fn text(row: &[Value], i: usize, line: &str) -> anyhow::Result<String> {
    field(row, i, line)?.as_str().map(str::to_string).ok_or_else(|| anyhow!("field {i} is not text: {line}"))
}

fn opt_text(row: &[Value], i: usize, line: &str) -> anyhow::Result<Option<String>> {
    Ok(field(row, i, line)?.as_str().map(str::to_string))
}

fn int(row: &[Value], i: usize, line: &str) -> anyhow::Result<i64> {
    field(row, i, line)?.as_i64().ok_or_else(|| anyhow!("field {i} is not a number: {line}"))
}

/// Parse the extraction output (psql lines or the same lines read back from a log): every
/// `H1 [..]` line once, verified against the `end` line's count and digest.
pub fn parse_extract(output: &str) -> anyhow::Result<Extract> {
    let mut lines: BTreeSet<&str> = BTreeSet::new();
    let mut end: Option<Vec<Value>> = None;
    for raw in output.lines() {
        let Some(line) = raw.trim_end().strip_prefix("H1 ") else { continue };
        let row: Vec<Value> = serde_json::from_str(line).with_context(|| format!("not a JSON array: {line}"))?;
        if row.first().and_then(Value::as_str) == Some("end") {
            if end.as_ref().is_some_and(|e| *e != row) {
                bail!("two different end lines: is this one extraction?");
            }
            end = Some(row);
        } else {
            lines.insert(line);
        }
    }
    let end = end.ok_or_else(|| anyhow!("no end line: the extraction is incomplete"))?;
    let endl = Value::Array(end.clone()).to_string();
    let mut x = Extract {
        owner: text(&end, 1, &endl)?,
        freeze_seq: int(&end, 2, &endl)?,
        board_messages: int(&end, 3, &endl)?,
        taken_at: text(&end, 4, &endl)?,
        lines: int(&end, 5, &endl)? as usize,
        digest: text(&end, 6, &endl)?,
        ..Extract::default()
    };
    if lines.len() != x.lines {
        bail!("{} distinct lines, the extraction printed {}: lines are missing", lines.len(), x.lines);
    }
    let joined: Vec<&str> = lines.iter().copied().collect();
    let digest = sha256_hex(joined.join("\n").as_bytes());
    if digest != x.digest {
        bail!("the lines' sha256 is {digest}, the extraction's {}: a line was altered", x.digest);
    }
    for line in lines {
        let row: Vec<Value> = serde_json::from_str(line)?;
        let r = &row;
        match r[0].as_str().unwrap_or_default() {
            "agent" => {
                let agent = XAgent {
                    owner: text(r, 2, line)?,
                    name: text(r, 3, line)?,
                    kind: opt_text(r, 4, line)?,
                    host: opt_text(r, 5, line)?,
                    pinned: field(r, 6, line)?.as_bool().unwrap_or(false),
                };
                x.agents.insert(text(r, 1, line)?, agent);
            }
            "channel" => {
                x.channels.insert(text(r, 1, line)?, (text(r, 2, line)?, text(r, 3, line)?));
            }
            "member" => {
                x.members.entry(text(r, 1, line)?).or_default().insert(text(r, 2, line)?, int(r, 3, line)?);
            }
            "msg" => {
                let m = XMessage {
                    id: text(r, 1, line)?,
                    seq: int(r, 2, line)?,
                    sender: text(r, 3, line)?,
                    recipient: opt_text(r, 4, line)?,
                    channel: opt_text(r, 5, line)?,
                    reply_to: opt_text(r, 6, line)?,
                    created_at: text(r, 7, line)?,
                    sha256: text(r, 8, line)?,
                    bytes: int(r, 9, line)?,
                };
                x.messages.insert(m.id.clone(), m);
            }
            "att" => x.attachments.push(XAttachment {
                id: text(r, 1, line)?,
                message_id: text(r, 2, line)?,
                filename: text(r, 3, line)?,
                size: int(r, 5, line)?,
                sha256: text(r, 6, line)?,
            }),
            "read" => x.reads.push((text(r, 1, line)?, text(r, 2, line)?, text(r, 3, line)?)),
            "react" => x.reactions.push((
                text(r, 1, line)?,
                text(r, 2, line)?,
                text(r, 3, line)?,
                field(r, 4, line)?.as_bool().unwrap_or(false),
            )),
            "host" => x.hosts.push((text(r, 1, line)?, field(r, 2, line)?.as_bool().unwrap_or(false))),
            other => bail!("unknown line kind '{other}': {line}"),
        }
    }
    for m in x.messages.values() {
        for a in std::iter::once(&m.sender).chain(&m.recipient) {
            if !x.agents.contains_key(a) {
                bail!("message {} names agent {a}, which the extraction does not list", m.id);
            }
        }
        if let Some(c) = &m.channel
            && !x.channels.contains_key(c)
        {
            bail!("message {} is in channel {c}, which the extraction does not list", m.id);
        }
    }
    if let Some(c) = x.members.keys().find(|c| !x.channels.contains_key(*c)) {
        bail!("a membership of channel {c}, which the extraction does not list");
    }
    let unknown = x.reads.iter().map(|r| &r.0).chain(x.reactions.iter().map(|r| &r.0)).chain(x.attachments.iter().map(|a| &a.message_id));
    if let Some(id) = unknown.into_iter().find(|id| !x.messages.contains_key(*id)) {
        bail!("a row of message {id}, which the extraction does not list");
    }
    Ok(x)
}

// --- classification -----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Imported,
    OwnerInternal,
    Shared,
}

impl Class {
    pub fn letter(self) -> &'static str {
        match self {
            Class::Imported => "a",
            Class::OwnerInternal => "b",
            Class::Shared => "c",
        }
    }

    pub fn parse(s: &str) -> Option<Class> {
        match s {
            "a" => Some(Class::Imported),
            "b" => Some(Class::OwnerInternal),
            "c" => Some(Class::Shared),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Class::Imported => "(a) imported from kond",
            Class::OwnerInternal => "(b) conde-native, owner-internal",
            Class::Shared => "(c) shared",
        }
    }
}

/// A cross-owner relationship of one of the owner's agents.
#[derive(Clone, Debug, Default)]
pub struct Evidence {
    pub imported: usize,
    pub later: usize,
    pub last: String,
}

impl Evidence {
    fn add(&mut self, class: Class, at: &str) {
        if class == Class::Imported {
            self.imported += 1;
        } else {
            self.later += 1;
        }
        if at > self.last.as_str() {
            self.last = at.to_string();
        }
    }
}

#[derive(Clone, Debug)]
pub struct NameRow {
    pub agent_id: String,
    pub name: String,
    pub imported: bool,
    /// `kind@host` when bound.
    pub binding: Option<String>,
    pub host: Option<String>,
    /// Other-owner agent -> DMs with it.
    pub dms: BTreeMap<String, Evidence>,
    /// Shared channel -> this agent's posts there.
    pub posts: BTreeMap<String, Evidence>,
    /// Shared channels this agent is a member of without posting.
    pub member_only: BTreeSet<String>,
}

impl NameRow {
    pub fn cross_owner(&self) -> bool {
        !self.dms.is_empty() || !self.posts.is_empty()
    }

    fn cross_in(&self, imported: bool) -> bool {
        self.dms.values().chain(self.posts.values()).any(|e| if imported { e.imported > 0 } else { e.later > 0 })
    }

    pub fn relation(&self) -> &'static str {
        match (self.cross_in(true), self.cross_in(false)) {
            (false, false) => "none",
            (true, false) => "imported rows",
            (false, true) => "later rows",
            (true, true) => "imported and later rows",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChannelRow {
    pub name: String,
    pub imported: bool,
    pub creator: String,
    /// owner -> members
    pub members: BTreeMap<String, usize>,
    /// owner -> distinct senders ever
    pub senders: BTreeMap<String, usize>,
    pub posts: BTreeMap<Class, usize>,
    pub last_post: Option<String>,
    pub owner_only: bool,
}

pub struct Inventory {
    pub x: Extract,
    pub class: BTreeMap<String, Class>,
    pub names: Vec<NameRow>,
    pub channels: Vec<ChannelRow>,
    /// Snapshot messages absent from the board.
    pub snapshot_missing: Vec<String>,
    /// Class (a) rows whose body or attachments differ from the snapshot.
    pub snapshot_differs: Vec<String>,
    pub snapshot_messages: usize,
    pub snapshot_parse_errors: Vec<String>,
    /// The snapshot directory and its file count, for the restore commands.
    pub snapshot_dir: PathBuf,
    pub snapshot_files: usize,
}

pub fn classify(x: Extract, snap: &Snapshot) -> Inventory {
    classify_at(x, snap, Path::new("<snapshot>"), 0)
}

pub fn classify_at(x: Extract, snap: &Snapshot, snapshot_dir: &Path, snapshot_files: usize) -> Inventory {
    let owner = x.owner.clone();
    let owner_of = |agent: &str| x.agents.get(agent).map(|a| a.owner.as_str()).unwrap_or("");
    // A channel is owner-internal when every sender ever and every current member is the owner's.
    let mut internal: BTreeMap<&str, bool> = x.channels.keys().map(|c| (c.as_str(), true)).collect();
    let mut shared_by_sender: BTreeMap<&str, bool> = BTreeMap::new();
    for m in x.messages.values() {
        if let Some(c) = &m.channel
            && owner_of(&m.sender) != owner
        {
            internal.insert(c, false);
            shared_by_sender.insert(c, true);
        }
    }
    for (c, members) in &x.members {
        if members.keys().any(|a| owner_of(a) != owner) {
            internal.insert(c, false);
        }
    }
    let mut class = BTreeMap::new();
    let mut snapshot_differs = Vec::new();
    let atts: BTreeMap<&str, Vec<(String, String)>> = x.attachments.iter().fold(BTreeMap::new(), |mut acc, a| {
        acc.entry(a.message_id.as_str()).or_default().push((a.filename.clone(), a.sha256.clone()));
        acc
    });
    for m in x.messages.values() {
        let c = if let Some(s) = snap.messages.get(&m.id) {
            let mut have = atts.get(m.id.as_str()).cloned().unwrap_or_default();
            let mut want = s.attachments.clone();
            have.sort();
            want.sort();
            if s.body_sha256 != m.sha256 || have != want {
                snapshot_differs.push(m.id.clone());
            }
            Class::Imported
        } else {
            let own = match (&m.channel, &m.recipient) {
                (Some(c), _) => internal.get(c.as_str()).copied().unwrap_or(false),
                (None, Some(r)) => owner_of(&m.sender) == owner && owner_of(r) == owner,
                (None, None) => false,
            };
            if own { Class::OwnerInternal } else { Class::Shared }
        };
        class.insert(m.id.clone(), c);
    }
    let snapshot_missing: Vec<String> = snap.messages.keys().filter(|id| !x.messages.contains_key(*id)).cloned().collect();

    // Names: the owner's agents and their cross-owner relationships, from the messages.
    let mut names: BTreeMap<String, NameRow> = x
        .agents
        .iter()
        .filter(|(_, a)| a.owner == owner)
        .map(|(id, a)| {
            let row = NameRow {
                agent_id: id.clone(),
                name: a.name.clone(),
                imported: snap.agents.contains(&a.name),
                binding: a.kind.as_ref().map(|k| format!("{k}@{}", a.host.as_deref().unwrap_or("?"))),
                host: a.kind.as_ref().and(a.host.clone()),
                dms: BTreeMap::new(),
                posts: BTreeMap::new(),
                member_only: BTreeSet::new(),
            };
            (id.clone(), row)
        })
        .collect();
    for m in x.messages.values() {
        let c = class[&m.id];
        match (&m.channel, &m.recipient) {
            (None, Some(r)) => {
                for (me, other) in [(&m.sender, r), (r, &m.sender)] {
                    if owner_of(other) != owner
                        && let Some(row) = names.get_mut(me)
                    {
                        row.dms.entry(x.agents[other].canonical()).or_default().add(c, &m.created_at);
                    }
                }
            }
            (Some(ch), _) if !internal[ch.as_str()] => {
                if let Some(row) = names.get_mut(&m.sender) {
                    row.posts.entry(format!("#{}", x.channels[ch].0)).or_default().add(c, &m.created_at);
                }
            }
            _ => {}
        }
    }
    for (c, members) in &x.members {
        if internal[c.as_str()] {
            continue;
        }
        let label = format!("#{}", x.channels[c].0);
        for a in members.keys() {
            if let Some(row) = names.get_mut(a)
                && !row.posts.contains_key(&label)
            {
                row.member_only.insert(label.clone());
            }
        }
    }
    let mut names: Vec<NameRow> = names.into_values().collect();
    names.sort_by(|a, b| a.name.cmp(&b.name));

    let mut channels = Vec::new();
    for (id, (name, creator)) in &x.channels {
        let mut members: BTreeMap<String, usize> = BTreeMap::new();
        for a in x.members.get(id).map(|m| m.keys().collect::<Vec<_>>()).unwrap_or_default() {
            *members.entry(owner_of(a).to_string()).or_default() += 1;
        }
        let mut senders: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        let mut posts: BTreeMap<Class, usize> = BTreeMap::new();
        let mut last_post: Option<String> = None;
        for m in x.messages.values().filter(|m| m.channel.as_deref() == Some(id)) {
            senders.entry(owner_of(&m.sender).to_string()).or_default().insert(&m.sender);
            *posts.entry(class[&m.id]).or_default() += 1;
            if last_post.as_deref().is_none_or(|l| m.created_at.as_str() > l) {
                last_post = Some(m.created_at.clone());
            }
        }
        channels.push(ChannelRow {
            name: name.clone(),
            imported: snap.channels.contains(name),
            creator: x.agents.get(creator).map(XAgent::canonical).unwrap_or_else(|| creator.clone()),
            members,
            senders: senders.into_iter().map(|(o, s)| (o, s.len())).collect(),
            posts,
            last_post,
            owner_only: internal[id.as_str()],
        });
    }
    channels.sort_by(|a, b| (!a.imported, &a.name).cmp(&(!b.imported, &b.name)));
    Inventory {
        snapshot_messages: snap.messages.len(),
        snapshot_parse_errors: snap.parse_errors.clone(),
        snapshot_dir: snapshot_dir.to_path_buf(),
        snapshot_files,
        x,
        class,
        names,
        channels,
        snapshot_missing,
        snapshot_differs,
    }
}

// --- the manifest -------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn unesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

pub fn manifest_paths(stem: &Path) -> [PathBuf; 4] {
    let with = |suffix: &str| {
        let mut s = stem.as_os_str().to_owned();
        s.push(suffix);
        PathBuf::from(s)
    };
    [with(".md"), with("-messages.tsv"), with("-attachments.tsv"), with("-names.tsv")]
}

impl Inventory {
    fn party(&self, agent: &str) -> String {
        self.x.agents.get(agent).map(XAgent::canonical).unwrap_or_else(|| agent.to_string())
    }

    fn to(&self, m: &XMessage) -> String {
        match (&m.channel, &m.recipient) {
            (Some(c), _) => format!("#{}", self.x.channels[c].0),
            (None, Some(r)) => self.party(r),
            _ => String::new(),
        }
    }

    pub fn count(&self, c: Class) -> usize {
        self.class.values().filter(|k| **k == c).count()
    }

    pub fn ids(&self, c: Class) -> Vec<&XMessage> {
        let mut v: Vec<&XMessage> =
            self.x.messages.values().filter(|m| self.class[&m.id] == c).collect();
        v.sort_by_key(|m| m.seq);
        v
    }

    fn attachments_of(&self, id: &str) -> Vec<&XAttachment> {
        self.x.attachments.iter().filter(|a| a.message_id == id).collect()
    }

    pub fn class_digest(&self, c: Class) -> String {
        rows_digest(self.ids(c).into_iter().map(|m| (m.id.as_str(), m.sha256.as_str())))
    }

    fn header(&self) -> String {
        format!(
            "# owner: {}\n# freeze: commit_seq {} ({} messages on the board), extracted {}\n# extraction: {} lines, sha256 {}\n",
            self.x.owner, self.x.freeze_seq, self.x.board_messages, self.x.taken_at, self.x.lines, self.x.digest
        )
    }

    pub fn messages_tsv(&self) -> String {
        let mut out = self.header();
        out.push_str("class\tmessage_id\tseq\tcreated_at\tfrom\tto\treply_to\tbody_sha256\tbody_bytes\tattachments\n");
        let mut all: Vec<&XMessage> = self.x.messages.values().collect();
        all.sort_by_key(|m| m.seq);
        for m in all {
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                self.class[&m.id].letter(),
                m.id,
                m.seq,
                m.created_at,
                esc(&self.party(&m.sender)),
                esc(&self.to(m)),
                m.reply_to.as_deref().unwrap_or(""),
                m.sha256,
                m.bytes,
                self.attachments_of(&m.id).len()
            );
        }
        out
    }

    pub fn attachments_tsv(&self) -> String {
        let mut out = self.header();
        out.push_str("class\tmessage_id\tattachment_id\tfilename\tsize\tsha256\n");
        let mut all: Vec<&XAttachment> = self.x.attachments.iter().collect();
        all.sort_by(|a, b| (self.x.messages[&a.message_id].seq, &a.id).cmp(&(self.x.messages[&b.message_id].seq, &b.id)));
        for a in all {
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}",
                self.class[&a.message_id].letter(),
                a.message_id,
                a.id,
                esc(&a.filename),
                a.size,
                a.sha256
            );
        }
        out
    }

    pub fn names_tsv(&self) -> String {
        let mut out = self.header();
        out.push_str("name\tagent_id\timported\tbinding\tcross_owner\tcandidate\tevidence\n");
        for n in &self.names {
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                esc(&n.name),
                n.agent_id,
                if n.imported { "yes" } else { "no" },
                n.binding.as_deref().unwrap_or("unbound"),
                n.relation(),
                if n.cross_owner() && n.host.is_some() { "yes" } else { "no" },
                esc(&evidence(n))
            );
        }
        out
    }

    pub fn markdown(&self, source: &str) -> String {
        let o = &self.x.owner;
        let mut s = String::new();
        let _ = writeln!(s, "# History manifest, owner {o} ({})\n", self.x.taken_at.get(..10).unwrap_or(""));
        let _ = writeln!(
            s,
            "The central board's rows that involve {o}'s agents, classified by message relationships (H1, DESIGN-v2\n\
             section 10), frozen at commit_seq {freeze} when the board held {total} messages (extracted {at}). Of the\n\
             {scope} messages in scope, {a} are imported from kond (a), {b} owner-internal (b) and {c} shared (c). H3 copies\n\
             the (b) rows to the local board; H4, only on the owner's go for an exact list, retires (a) and the copied (b)\n\
             from central; (c) stays.\n\n\
             The files next to this one hold every row with its hashes: `-messages.tsv` (per message: class, id, seq,\n\
             date, parties, reply-to, body sha256 and size), `-attachments.tsv` (per attachment, with its sha256) and\n\
             `-names.tsv` (per {o} agent). No message text, attachment byte, session or thread id is in any of them.\n",
            freeze = self.x.freeze_seq,
            total = self.x.board_messages,
            at = self.x.taken_at,
            scope = self.x.messages.len(),
            a = self.count(Class::Imported),
            b = self.count(Class::OwnerInternal),
            c = self.count(Class::Shared),
        );
        let _ = writeln!(s, "Source: {source}. The extraction printed {} lines with sha256 `{}`; the lines read back hash to the same value.\n", self.x.lines, self.x.digest);
        s.push_str("## Rules applied\n\n");
        let _ = writeln!(
            s,
            "- Scope: every message sent or received by an agent of {o}, and every post in a channel where an agent of\n  \
             {o} is a member, has posted or is the creator.\n\
             - (a) imported: the message id is in the kond snapshot (the importer's own reading of it).\n\
             - (b) owner-internal: not (a), and a DM whose both parties are {o}'s, or a post in a channel whose every\n  \
             sender ever and every current member is {o}'s.\n\
             - (c) shared: everything else.\n\
             - A name has a cross-owner relationship when it has a DM with an agent of another owner, or a post in a\n  \
             channel where an agent of another owner has posted or is a member (the section 9 candidate rule).\n  \
             The publication candidates of a host are its bound names with such a relationship.\n"
        );
        s.push_str("## Classes\n\n| class | messages | attachments | bodies digest |\n|---|---:|---:|---|\n");
        for c in [Class::Imported, Class::OwnerInternal, Class::Shared] {
            let atts = self.x.attachments.iter().filter(|a| self.class[&a.message_id] == c).count();
            let _ = writeln!(s, "| {} | {} | {} | `{}` |", c.title(), self.count(c), atts, self.class_digest(c));
        }
        let _ = writeln!(
            s,
            "\nThe bodies digest is the sha256 of the sorted `<id> <body sha256>` lines of the class; `conde import`\n\
             prints the same digest for the bodies it restored (H2), so the restore is checked against this list.\n"
        );
        s.push_str("## Class (a) against the kond snapshot\n\n");
        let _ = writeln!(
            s,
            "- snapshot messages (as the importer reads them): {}; on the board: {}; missing from the board: {}\n\
             - class (a) rows whose body or attachments differ from the snapshot by sha256: {}\n\
             - snapshot parse errors: {}\n",
            self.snapshot_messages,
            self.snapshot_messages - self.snapshot_missing.len(),
            list_or_none(&self.snapshot_missing),
            list_or_none(&self.snapshot_differs),
            self.snapshot_parse_errors.len()
        );

        let dangling: Vec<String> = {
            let mut v: Vec<&XMessage> = self
                .x
                .messages
                .values()
                .filter(|m| m.reply_to.as_ref().is_some_and(|r| !self.x.messages.contains_key(r)))
                .collect();
            v.sort_by_key(|m| m.seq);
            v.iter()
                .map(|m| format!("`{}` ({}) replies to `{}`", m.id, self.class[&m.id].letter(), m.reply_to.as_deref().unwrap_or("")))
                .collect()
        };
        let _ = writeln!(
            s,
            "Reply-to links that resolve to no message on the board: {}.\n",
            if dangling.is_empty() { "none".to_string() } else { dangling.join("; ") }
        );

        s.push_str("## Names\n\n");
        let mine = &self.names;
        let imported: Vec<&NameRow> = mine.iter().filter(|n| n.imported).collect();
        let bound_imported = imported.iter().filter(|n| n.binding.is_some()).count();
        let crossing: Vec<&NameRow> = mine.iter().filter(|n| n.cross_owner()).collect();
        let imported_later: Vec<&NameRow> = imported.iter().copied().filter(|n| n.cross_in(false)).collect();
        let _ = writeln!(
            s,
            "| | names |\n|---|---:|\n| {o}'s agents on the board | {} |\n| of them in the kond snapshot (imported) | {} |\n\
             | imported and bound | {} |\n| imported, no cross-owner relationship | {} |\n\
             | imported, cross-owner relationship in imported rows | {} |\n| imported, cross-owner relationship in later rows | {} |\n\
             | not imported (conde-native) | {} |\n| any name with a cross-owner relationship | {} |\n",
            mine.len(),
            imported.len(),
            bound_imported,
            imported.iter().filter(|n| !n.cross_owner()).count(),
            imported.iter().filter(|n| n.cross_in(true)).count(),
            imported_later.len(),
            mine.len() - imported.len(),
            crossing.len()
        );
        let _ = writeln!(
            s,
            "Imported names that acquired a cross-owner relationship after the import: {}.\n",
            if imported_later.is_empty() {
                "none".to_string()
            } else {
                imported_later.iter().map(|n| format!("`{}`", n.name)).collect::<Vec<_>>().join(", ")
            }
        );
        s.push_str("Every name with a cross-owner relationship or not in the snapshot (all others: `-names.tsv`, relationship none):\n\n");
        s.push_str("| name | imported | binding | cross-owner | evidence |\n|---|---|---|---|---|\n");
        for n in mine.iter().filter(|n| n.cross_owner() || !n.imported) {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} |",
                n.name,
                if n.imported { "yes" } else { "no" },
                n.binding.as_deref().unwrap_or("unbound"),
                n.relation(),
                md(&evidence(n))
            );
        }

        s.push_str("\n## Channels\n\n| channel | imported | creator | members by owner | senders by owner | posts (a/b/c) | last post | owner-only |\n|---|---|---|---|---|---|---|---|\n");
        for c in &self.channels {
            let by = |m: &BTreeMap<String, usize>| m.iter().map(|(o, n)| format!("{o} {n}")).collect::<Vec<_>>().join(", ");
            let p = |k| c.posts.get(&k).copied().unwrap_or(0);
            let _ = writeln!(
                s,
                "| #{} | {} | {} | {} | {} | {}/{}/{} | {} | {} |",
                c.name,
                if c.imported { "yes" } else { "no" },
                c.creator,
                by(&c.members),
                if c.senders.is_empty() { "none".into() } else { by(&c.senders) },
                p(Class::Imported),
                p(Class::OwnerInternal),
                p(Class::Shared),
                c.last_post.as_deref().unwrap_or("none"),
                if c.owner_only { "yes" } else { "no" }
            );
        }

        s.push_str("\n## Publication candidates per host\n\n");
        let mut hosts: Vec<(String, bool)> = self.x.hosts.clone();
        hosts.sort();
        for (host, active) in &hosts {
            let cands: Vec<&NameRow> =
                mine.iter().filter(|n| n.cross_owner() && n.host.as_deref() == Some(host.as_str())).collect();
            let bound = mine.iter().filter(|n| n.host.as_deref() == Some(host.as_str())).count();
            let _ = writeln!(
                s,
                "- `{host}` ({}; {bound} bound names): {}",
                if *active { "host key active" } else { "no active host key" },
                if cands.is_empty() {
                    "no candidates".to_string()
                } else {
                    cands.iter().map(|n| format!("`{}`", n.name)).collect::<Vec<_>>().join(", ")
                }
            );
        }
        let unhosted: Vec<&NameRow> = crossing.iter().copied().filter(|n| n.host.is_none()).collect();
        if !unhosted.is_empty() {
            let _ = writeln!(
                s,
                "- unbound names with a cross-owner relationship (no host, so no candidate): {}",
                unhosted.iter().map(|n| format!("`{}`", n.name)).collect::<Vec<_>>().join(", ")
            );
        }

        let host = match self.x.hosts.iter().filter(|(_, active)| *active).collect::<Vec<_>>().as_slice() {
            [(h, _)] => h.clone(),
            _ => "<the kond machine's host id>".to_string(),
        };
        s.push_str("\n## Restore on the switch (H2 + H3) and the final freeze\n\n");
        let _ = writeln!(
            s,
            "This list is frozen at commit_seq {freeze}. Class (b) traffic needs one of {o}'s hosts, so the final freeze\n\
             is taken again after the switch stops the daemon on `{host}`: the same extraction, then `conde history\n\
             inventory`; rows above commit_seq {freeze} are the delta for review. The restore then runs into the new,\n\
             empty local board with the daemon stopped; on any failure the file is discarded and both steps rerun.\n\n\
             1. H2: `conde import --from {dir} --expect-files {files} --owner {o} --host {host}`. Its validation must\n   \
             end `result: OK`, with every body and attachment sha256-equal to the snapshot and `bodies digest:\n   \
             {a}` (class (a) above).\n\
             2. H3: `conde history copy --manifest <this file without .md>` (the board must serve `history.copy`).\n   \
             It must end `result: OK`, with every frozen id present, bodies, attachments, reply-to and read state\n   \
             equal, and `bodies digest: {b}` (class (b) above).\n",
            freeze = self.x.freeze_seq,
            dir = self.snapshot_dir.display(),
            files = self.snapshot_files,
            a = self.class_digest(Class::Imported),
            b = self.class_digest(Class::OwnerInternal),
        );

        for c in [Class::OwnerInternal, Class::Shared] {
            let rows = self.ids(c);
            let _ = writeln!(s, "\n## Class {} rows ({})\n", c.title(), rows.len());
            if c == Class::OwnerInternal {
                s.push_str("For the owner's review: H3 copies exactly these rows to the local board, and H4 retires them from\ncentral after the copy validates. Privacy is not inferred from membership alone.\n\n");
            } else {
                s.push_str("These stay on central (H4 does not touch them).\n\n");
            }
            s.push_str("| seq | id | date | from | to | bytes | attachments | reply to |\n|---:|---|---|---|---|---:|---|---|\n");
            for m in rows {
                let atts: Vec<String> = self.attachments_of(&m.id).iter().map(|a| md(&a.filename)).collect();
                let _ = writeln!(
                    s,
                    "| {} | `{}` | {} | {} | {} | {} | {} | {} |",
                    m.seq,
                    m.id,
                    m.created_at,
                    md(&self.party(&m.sender)),
                    md(&self.to(m)),
                    m.bytes,
                    if atts.is_empty() { "-".into() } else { atts.join(", ") },
                    m.reply_to.as_deref().map(|r| format!("`{r}`")).unwrap_or_else(|| "-".into())
                );
            }
        }
        s
    }
}

fn list_or_none(v: &[String]) -> String {
    if v.is_empty() { "none".to_string() } else { v.join(" ") }
}

fn md(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn evidence(n: &NameRow) -> String {
    let fmt = |what: &str, e: &Evidence| {
        let mut parts = Vec::new();
        if e.imported > 0 {
            parts.push(format!("{} imported", e.imported));
        }
        if e.later > 0 {
            parts.push(format!("{} later", e.later));
        }
        format!("{what}: {}, last {}", parts.join(" + "), e.last)
    };
    let mut out: Vec<String> = n.dms.iter().map(|(who, e)| fmt(&format!("DM {who}"), e)).collect();
    out.extend(n.posts.iter().map(|(c, e)| fmt(&format!("posts {c}"), e)));
    if !n.member_only.is_empty() {
        out.push(format!("member of {}", n.member_only.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    if out.is_empty() { "-".to_string() } else { out.join("; ") }
}

/// Write the manifest files of `inv` at `out` (see [`manifest_paths`]).
pub fn write_manifest(inv: &Inventory, out: &Path, source: &str) -> anyhow::Result<[PathBuf; 4]> {
    let paths = manifest_paths(out);
    let [md_path, messages, attachments, names] = &paths;
    fs::write(md_path, inv.markdown(source))?;
    fs::write(messages, inv.messages_tsv())?;
    fs::write(attachments, inv.attachments_tsv())?;
    fs::write(names, inv.names_tsv())?;
    Ok(paths)
}

/// `conde history inventory`: classify an extraction against a kond snapshot and write the
/// manifest files at `<out>.md`, `<out>-messages.tsv`, `<out>-attachments.tsv`, `<out>-names.tsv`.
pub fn inventory_main(extract: &Path, snapshot: &Path, out: &Path, source: &str) -> anyhow::Result<i32> {
    let raw = fs::read_to_string(extract).with_context(|| format!("reading {}", extract.display()))?;
    let x = parse_extract(&raw).with_context(|| format!("checking {}", extract.display()))?;
    let snap = crate::import::snapshot(snapshot)?;
    let files = crate::import::count_files(snapshot)?;
    let inv = classify_at(x, &snap, snapshot, files);
    let paths = write_manifest(&inv, out, source)?;
    println!(
        "classified {} messages at commit_seq {}: (a) {}, (b) {}, (c) {}",
        inv.x.messages.len(),
        inv.x.freeze_seq,
        inv.count(Class::Imported),
        inv.count(Class::OwnerInternal),
        inv.count(Class::Shared)
    );
    println!(
        "snapshot: {} messages, {} missing from the board, {} differ by sha256",
        inv.snapshot_messages,
        inv.snapshot_missing.len(),
        inv.snapshot_differs.len()
    );
    for p in paths {
        println!("wrote {}", p.display());
    }
    Ok(i32::from(!inv.snapshot_missing.is_empty() || !inv.snapshot_differs.is_empty()))
}

// --- reading the manifest -----------------------------------------------------------------

/// One message line of the manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestRow {
    pub class: Class,
    pub id: String,
    pub seq: i64,
    pub created_at: String,
    pub from: String,
    pub to: String,
    pub reply_to: Option<String>,
    pub sha256: String,
    /// (filename, sha256), sorted.
    pub attachments: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct Manifest {
    pub owner: String,
    pub rows: Vec<ManifestRow>,
}

impl Manifest {
    pub fn class(&self, c: Class) -> Vec<&ManifestRow> {
        self.rows.iter().filter(|r| r.class == c).collect()
    }
}

fn data_lines(raw: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    raw.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).skip(1).map(|l| l.split('\t').map(unesc).collect())
}

pub fn read_manifest(stem: &Path) -> anyhow::Result<Manifest> {
    let [_, messages, attachments, _] = manifest_paths(stem);
    let raw = fs::read_to_string(&messages).with_context(|| format!("reading {}", messages.display()))?;
    let owner = raw
        .lines()
        .find_map(|l| l.strip_prefix("# owner: "))
        .ok_or_else(|| anyhow!("{} has no owner line", messages.display()))?
        .to_string();
    let mut atts: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let raw_atts = fs::read_to_string(&attachments).with_context(|| format!("reading {}", attachments.display()))?;
    for f in data_lines(&raw_atts) {
        if f.len() != 6 {
            bail!("{}: a line has {} fields, not 6", attachments.display(), f.len());
        }
        atts.entry(f[1].clone()).or_default().push((f[3].clone(), f[5].clone()));
    }
    let mut rows = Vec::new();
    for f in data_lines(&raw) {
        if f.len() != 10 {
            bail!("{}: a line has {} fields, not 10", messages.display(), f.len());
        }
        let class = Class::parse(&f[0]).ok_or_else(|| anyhow!("unknown class '{}'", f[0]))?;
        let mut a = atts.remove(&f[1]).unwrap_or_default();
        a.sort();
        if a.len().to_string() != f[9] {
            bail!("{}: message {} lists {} attachments, the attachments file {}", messages.display(), f[1], f[9], a.len());
        }
        rows.push(ManifestRow {
            class,
            id: f[1].clone(),
            seq: f[2].parse().with_context(|| format!("seq of {}", f[1]))?,
            created_at: f[3].clone(),
            from: f[4].clone(),
            to: f[5].clone(),
            reply_to: Some(f[6].clone()).filter(|r| !r.is_empty()),
            sha256: f[7].clone(),
            attachments: a,
        });
    }
    if let Some(id) = atts.keys().next() {
        bail!("{}: attachment of message {id}, which the messages file does not list", attachments.display());
    }
    Ok(Manifest { owner, rows })
}

// --- H3: the copy -------------------------------------------------------------------------

/// A row as `history.copy` returns it, checked against the manifest.
#[derive(Debug)]
struct Fetched {
    row: ManifestRow,
    text: String,
    attachments: Vec<(String, Vec<u8>)>,
    /// DM: the recipient's read_at.
    read_at: Option<String>,
    /// Channel post: member name -> read on central.
    members: Vec<(String, bool)>,
    /// (emoji, actor name)
    reactions: Vec<(String, String)>,
}

pub struct CopyOptions {
    pub manifest: PathBuf,
    pub db: PathBuf,
    pub server: String,
    pub key: String,
    pub dry_run: bool,
}

#[derive(Debug, Default)]
pub struct CopyReport {
    pub rows: usize,
    pub attachments: usize,
    pub agents_created: Vec<String>,
    pub channels_created: Vec<String>,
    pub problems: Vec<String>,
    pub lines: Vec<String>,
    pub written: bool,
}

impl CopyReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

impl std::fmt::Display for CopyReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "conde history copy: {} class (b) rows, {} attachments", self.rows, self.attachments)?;
        writeln!(f, "  local agents created (unbound): {}", list_or_none(&self.agents_created))?;
        writeln!(f, "  local channels created: {}", list_or_none(&self.channels_created))?;
        for l in &self.lines {
            writeln!(f, "  {l}")?;
        }
        for p in &self.problems {
            writeln!(f, "  PROBLEM {p}")?;
        }
        writeln!(f, "  written: {}", if self.written { "yes, one transaction" } else { "nothing" })?;
        writeln!(f, "result: {}", if self.ok() { "OK" } else { "ISSUES (see above)" })
    }
}

const BATCH: usize = 20;

fn rpc(server: &str, key: &str, ids: &[String]) -> anyhow::Result<Vec<Value>> {
    let url = crate::credentials::rpc_url(server)?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(crate::login::tls()?)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        let mut out = Vec::new();
        for chunk in ids.chunks(BATCH) {
            let req = Request::new(new_ulid(), "history.copy", json!({"ids": chunk}));
            let resp = client.post(&url).bearer_auth(key).json(&req).send().await.context("calling history.copy")?;
            let status = resp.status();
            let resp: Response = resp.json().await.with_context(|| format!("history.copy answered {status}"))?;
            if !resp.ok {
                let e = resp.error.map(|e| e.message).unwrap_or_else(|| status.to_string());
                bail!("history.copy refused: {e}");
            }
            let rows = resp.result.and_then(|r| r.get("messages").cloned()).and_then(|m| m.as_array().cloned());
            out.extend(rows.ok_or_else(|| anyhow!("history.copy answer has no messages"))?);
        }
        Ok(out)
    })
}

/// The name part of `owner/name`, when the owner is `owner`.
fn own_name<'a>(qualified: &'a str, owner: &str) -> Option<&'a str> {
    qualified.strip_prefix(owner).and_then(|r| r.strip_prefix('/'))
}

fn check_fetched(v: &Value, want: &ManifestRow, owner: &str, problems: &mut Vec<String>) -> Fetched {
    let id = &want.id;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let text = s("text").unwrap_or_default();
    let to = match (s("channel"), s("to")) {
        (Some(c), _) => format!("#{c}"),
        (None, Some(r)) => r,
        _ => String::new(),
    };
    let sha = sha256_hex(text.as_bytes());
    if sha != want.sha256 || s("sha256").as_deref() != Some(sha.as_str()) {
        problems.push(format!("{id}: body sha256 {sha}, the manifest {}", want.sha256));
    }
    for (what, got, exp) in [
        ("sender", s("from").unwrap_or_default(), want.from.clone()),
        ("recipient", to, want.to.clone()),
        ("date", s("created_at").unwrap_or_default(), want.created_at.clone()),
        ("reply-to", s("reply_to").unwrap_or_default(), want.reply_to.clone().unwrap_or_default()),
    ] {
        if got != exp {
            problems.push(format!("{id}: {what} is {got} on the board, {exp} in the manifest"));
        }
    }
    let mut attachments = Vec::new();
    let mut hashes = Vec::new();
    for a in v.get("attachments").and_then(Value::as_array).cloned().unwrap_or_default() {
        let name = a["filename"].as_str().unwrap_or_default().to_string();
        let Ok(bytes) = B64.decode(a["data"].as_str().unwrap_or_default()) else {
            problems.push(format!("{id}: attachment {name} is not base64"));
            continue;
        };
        hashes.push((name.clone(), sha256_hex(&bytes)));
        attachments.push((name, bytes));
    }
    hashes.sort();
    if hashes != want.attachments {
        problems.push(format!("{id}: attachments {hashes:?} differ from the manifest's {:?}", want.attachments));
    }
    let mut members = Vec::new();
    for m in v["read"]["members"].as_array().cloned().unwrap_or_default() {
        members.push((m["name"].as_str().unwrap_or_default().to_string(), m["read"].as_bool().unwrap_or(false)));
    }
    let reactions: Vec<(String, String)> = v["reactions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| (r["emoji"].as_str().unwrap_or_default().to_string(), r["by"].as_str().unwrap_or_default().to_string()))
        .collect();
    let parties = std::iter::once(want.from.as_str())
        .chain((!want.to.starts_with('#')).then_some(want.to.as_str()))
        .chain(members.iter().map(|(n, _)| n.as_str()))
        .chain(reactions.iter().map(|(_, by)| by.as_str()));
    for p in parties {
        if own_name(p, owner).is_none() {
            problems.push(format!("{id}: {p} is not {owner}'s; a class (b) row has the owner's parties only"));
        }
    }
    let read_at = v["read"]["read_at"].as_str().map(str::to_string);
    Fetched { row: want.clone(), text, attachments, read_at, members, reactions }
}

/// DM conversation key (unordered pair) or channel.
fn conversation(r: &ManifestRow) -> (String, String) {
    if r.to.starts_with('#') {
        (r.to.clone(), String::new())
    } else if r.from <= r.to {
        (r.from.clone(), r.to.clone())
    } else {
        (r.to.clone(), r.from.clone())
    }
}

/// The newest local message of a conversation (local names), as (created_at, id).
fn newest_local(tx: &dyn Tx, user_id: i64, conv: &(String, String), owner: &str) -> anyhow::Result<Option<(String, String)>> {
    let mut newest: Option<(String, String)> = None;
    let mut see = |m: crate::store::Message| {
        if newest.as_ref().is_none_or(|(at, _)| m.created_at > *at) {
            newest = Some((m.created_at, m.id));
        }
    };
    if let Some(channel) = conv.0.strip_prefix('#') {
        let Some(cid) = tx.channel_id_by_name(channel)? else { return Ok(None) };
        for id in tx.channel_message_ids(&cid)? {
            if let Some(m) = tx.message_by_id(&id)? {
                see(m);
            }
        }
        return Ok(newest);
    }
    let local = |q: &str| own_name(q, owner).map(str::to_string);
    let (Some(a), Some(b)) = (local(&conv.0), local(&conv.1)) else { return Ok(None) };
    let (Some(a), Some(b)) = (tx.agent_by_name(user_id, &a)?, tx.agent_by_name(user_id, &b)?) else { return Ok(None) };
    for (from, to) in [(&a, &b), (&b, &a)] {
        let mut after = 0;
        loop {
            let page = tx.sent_by(&from.id, after, 500)?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            for m in page.into_iter().filter(|m| m.recipient_id.as_deref() == Some(to.id.as_str())) {
                see(m);
            }
        }
    }
    Ok(newest)
}

pub fn copy(opts: &CopyOptions) -> anyhow::Result<CopyReport> {
    let manifest = read_manifest(&opts.manifest)?;
    let owner = manifest.owner.clone();
    let mut wanted: Vec<ManifestRow> = manifest.class(Class::OwnerInternal).into_iter().cloned().collect();
    wanted.sort_by_key(|r| r.seq);
    let mut report = CopyReport { rows: wanted.len(), ..CopyReport::default() };
    report.attachments = wanted.iter().map(|r| r.attachments.len()).sum();
    if !opts.db.exists() {
        bail!("{} does not exist: the H2 import restores the local board first", opts.db.display());
    }
    let store = SqliteStore::open(&opts.db)?;

    // Before any fetch: the local board is the H2 restore, and none of the ids is on it yet.
    let user_id = {
        let tx = store.begin(Mode::Read)?;
        let user_id = tx.user_id(&owner)?.ok_or_else(|| {
            anyhow!("{} has no user {owner}: run the H2 import (conde import --owner {owner}) first", opts.db.display())
        })?;
        for r in &wanted {
            if tx.message_by_id(&r.id)?.is_some() {
                report.problems.push(format!("{} is already on the local board (a kond id, or a copy already ran)", r.id));
            }
        }
        user_id
    };
    if !report.problems.is_empty() {
        return Ok(report);
    }

    let ids: Vec<String> = wanted.iter().map(|r| r.id.clone()).collect();
    let fetched_raw = if ids.is_empty() { Vec::new() } else { rpc(&opts.server, &opts.key, &ids)? };
    let mut by_id: HashMap<String, Value> = HashMap::new();
    for v in fetched_raw {
        let id = v["message_id"].as_str().unwrap_or_default().to_string();
        by_id.insert(id, v);
    }
    let mut fetched = Vec::new();
    for r in &wanted {
        match by_id.get(&r.id) {
            Some(v) => fetched.push(check_fetched(v, r, &owner, &mut report.problems)),
            None => report.problems.push(format!("{}: not in the board's answer", r.id)),
        }
    }

    // Order and links, against the local board as the H2 import left it.
    let copied: BTreeSet<&str> = wanted.iter().map(|r| r.id.as_str()).collect();
    {
        let tx = store.begin(Mode::Read)?;
        let mut newest: BTreeMap<(String, String), Option<(String, String)>> = BTreeMap::new();
        for r in &wanted {
            let conv = conversation(r);
            if !newest.contains_key(&conv) {
                let n = newest_local(&*tx, user_id, &conv, &owner)?;
                newest.insert(conv.clone(), n);
            }
            if let Some((at, kond)) = &newest[&conv]
                && r.created_at < *at
            {
                report.problems.push(format!(
                    "{} ({}) is older than {kond} ({at}), the newest local message between {} and {}: for decision",
                    r.id, r.created_at, conv.0, if conv.1.is_empty() { "its members" } else { conv.1.as_str() }
                ));
            }
            if let Some(reply) = &r.reply_to
                && !copied.contains(reply.as_str())
                && tx.message_by_id(reply)?.is_none()
            {
                report.problems.push(format!("{}: replies to {reply}, which is neither local nor copied", r.id));
            }
        }
    }
    if !report.problems.is_empty() || opts.dry_run {
        if opts.dry_run {
            report.lines.push("dry run: fetched and checked, nothing written".to_string());
        }
        return Ok(report);
    }

    apply(&store, user_id, &owner, &fetched, &mut report)?;
    report.written = true;
    validate(&store, user_id, &owner, &fetched, &mut report)?;
    Ok(report)
}

fn apply(store: &SqliteStore, user_id: i64, owner: &str, rows: &[Fetched], report: &mut CopyReport) -> anyhow::Result<()> {
    let boxed = store.begin(Mode::Write)?;
    let tx = &*boxed;
    let mut agents: HashMap<String, String> = HashMap::new();
    let mut agent = |tx: &dyn Tx, qualified: &str, report: &mut CopyReport| -> anyhow::Result<String> {
        if let Some(id) = agents.get(qualified) {
            return Ok(id.clone());
        }
        let name = own_name(qualified, owner).ok_or_else(|| anyhow!("{qualified} is not {owner}'s"))?;
        let id = match tx.agent_by_name(user_id, name)? {
            Some(a) => a.id,
            None => {
                let id = new_ulid();
                tx.insert_imported_agent(&ImportedAgent {
                    id: &id,
                    owner_id: user_id,
                    name,
                    display: None,
                    address: None,
                    binding_version: 0,
                    pinned: None,
                })?;
                report.agents_created.push(name.to_string());
                id
            }
        };
        agents.insert(qualified.to_string(), id.clone());
        Ok(id)
    };
    for f in rows {
        let r = &f.row;
        let sender = agent(tx, &r.from, report)?;
        let (recipient, channel) = match r.to.strip_prefix('#') {
            Some(c) => {
                let id = match tx.channel_id_by_name(c)? {
                    Some(id) => id,
                    None => {
                        let id = new_ulid();
                        tx.insert_channel(&Channel { id: id.clone(), name: c.to_string(), created_by: sender.clone(), topic: None })?;
                        report.channels_created.push(format!("#{c}"));
                        id
                    }
                };
                (None, Some(id))
            }
            None => (Some(agent(tx, &r.to, report)?), None),
        };
        let inserted = tx.insert_message_if_absent(&NewMessage {
            id: &r.id,
            sender_id: &sender,
            recipient_id: recipient.as_deref(),
            channel_id: channel.as_deref(),
            text: &f.text,
            reply_to: r.reply_to.as_deref(),
            created_at: &r.created_at,
        })?;
        if !inserted {
            bail!("{} appeared on the local board during the copy", r.id);
        }
        for (name, bytes) in &f.attachments {
            tx.insert_attachment(&new_ulid(), &r.id, name, bytes)?;
        }
        if let (Some(rcpt), Some(at)) = (&recipient, &f.read_at) {
            tx.mark_dm_read(&r.id, rcpt, at)?;
        }
        if let Some(cid) = &channel {
            let seq = tx.message_by_id(&r.id)?.map(|m| m.seq).unwrap_or(0);
            for (member, read) in &f.members {
                let aid = agent(tx, member, report)?;
                match tx.read_seq(cid, &aid)? {
                    Some(_) if *read => tx.advance_cursor(cid, &aid, seq)?,
                    Some(_) => {}
                    None => tx.insert_membership(cid, &aid, if *read { seq } else { seq - 1 })?,
                }
            }
        }
        for (emoji, by) in &f.reactions {
            let aid = agent(tx, by, report)?;
            tx.insert_reaction_if_absent(&r.id, &aid, emoji, true)?;
        }
    }
    boxed.commit()?;
    Ok(())
}

fn validate(store: &SqliteStore, user_id: i64, owner: &str, rows: &[Fetched], report: &mut CopyReport) -> anyhow::Result<()> {
    let tx = store.begin(Mode::Read)?;
    let (mut present, mut bodies, mut atts, mut atts_ok, mut links, mut reads) = (0, 0, 0, 0, 0, 0);
    for f in rows {
        let r = &f.row;
        let Some(m) = tx.message_by_id(&r.id)? else {
            report.problems.push(format!("{}: not on the local board after the copy", r.id));
            continue;
        };
        present += 1;
        if sha256_hex(m.text.as_bytes()) == r.sha256 {
            bodies += 1;
        } else {
            report.problems.push(format!("{}: local body sha256 differs from the manifest", r.id));
        }
        let mut local = Vec::new();
        for meta in tx.attachment_list(&r.id)? {
            if let Some(a) = tx.attachment(&meta.id)? {
                local.push((a.filename, sha256_hex(&a.bytes)));
            }
        }
        local.sort();
        atts += r.attachments.len();
        if local == r.attachments {
            atts_ok += r.attachments.len();
        } else {
            report.problems.push(format!("{}: local attachments {local:?} differ from the manifest", r.id));
        }
        match &r.reply_to {
            Some(reply) if tx.message_by_id(reply)?.is_none() => {
                report.problems.push(format!("{}: reply-to {reply} does not resolve locally", r.id));
            }
            _ => links += 1,
        }
        let read_ok = match (&m.channel_id, &m.recipient_id) {
            (Some(c), _) => {
                let mut ok = true;
                for (member, read) in &f.members {
                    let name = own_name(member, owner).unwrap_or_default();
                    let seq = match tx.agent_by_name(user_id, name)? {
                        Some(a) => tx.read_seq(c, &a.id)?,
                        None => None,
                    };
                    ok &= seq.is_some_and(|s| (s >= m.seq) == *read);
                }
                ok
            }
            (None, Some(rcpt)) => tx.dm_read_at(&r.id, rcpt)? == f.read_at,
            _ => false,
        };
        if read_ok {
            reads += 1;
        } else {
            report.problems.push(format!("{}: local read state differs from the board's", r.id));
        }
        let mut local_reactions: Vec<(String, String)> = tx.reactions(&r.id)?;
        let mut want = f.reactions.clone();
        local_reactions.sort();
        want.sort();
        if local_reactions != want {
            report.problems.push(format!("{}: local reactions {local_reactions:?}, the board's {want:?}", r.id));
        }
    }
    let n = rows.len();
    report.lines.push(format!("ids: {present} of {n} frozen class (b) ids on the local board"));
    report.lines.push(format!("bodies: {bodies} of {n} sha256-equal to the manifest"));
    report.lines.push(format!("attachments: {atts_ok} of {atts} sha256-equal to the manifest"));
    report.lines.push(format!("reply-to: {links} of {n} resolve locally"));
    report.lines.push(format!("read state: {reads} of {n} equal to the board's"));
    report.lines.push(format!(
        "bodies digest: {}",
        rows_digest(rows.iter().map(|f| (f.row.id.as_str(), f.row.sha256.as_str())))
    ));
    Ok(())
}

/// `conde history copy`: the H3 copy with the host credential of this machine.
pub fn copy_main(manifest: &Path, db: Option<PathBuf>, credentials: Option<PathBuf>, dry_run: bool) -> anyhow::Result<i32> {
    let path = match credentials {
        Some(p) => p,
        None => crate::credentials::path()?,
    };
    let creds = crate::credentials::load(&path)?;
    let opts = CopyOptions {
        manifest: manifest.to_path_buf(),
        db: db.unwrap_or_else(|| crate::paths::data_dir().join("conde.db")),
        server: creds.server,
        key: creds.key,
        dry_run,
    };
    let report = copy(&opts)?;
    print!("{report}");
    Ok(i32::from(!report.ok()))
}
