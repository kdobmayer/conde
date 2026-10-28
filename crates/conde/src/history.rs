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
//! H3 `conde history copy`: copies the manifest's class (b) rows (and, on request, the owner's
//! later DMs) from the rows the central purge saved (one Postgres CSV per table) into the local
//! board the H2 import restored, in one SQLite transaction, and validates them by hashes.
//!
//! `conde history prune-candidates`: the owner's prune list of a kond snapshot.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde_json::Value;

use crate::import::{Snapshot, rows_digest};
use crate::store::sqlite::SqliteStore;
use crate::store::{ImportedAgent, Mode, NewMessage, Store, Tx};
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
    field(row, i, line)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("field {i} is not text: {line}"))
}

fn opt_text(row: &[Value], i: usize, line: &str) -> anyhow::Result<Option<String>> {
    Ok(field(row, i, line)?.as_str().map(str::to_string))
}

fn int(row: &[Value], i: usize, line: &str) -> anyhow::Result<i64> {
    field(row, i, line)?
        .as_i64()
        .ok_or_else(|| anyhow!("field {i} is not a number: {line}"))
}

/// Parse the extraction output (psql lines or the same lines read back from a log): every
/// `H1 [..]` line once, verified against the `end` line's count and digest.
pub fn parse_extract(output: &str) -> anyhow::Result<Extract> {
    let mut lines: BTreeSet<&str> = BTreeSet::new();
    let mut end: Option<Vec<Value>> = None;
    for raw in output.lines() {
        let Some(line) = raw.trim_end().strip_prefix("H1 ") else {
            continue;
        };
        let row: Vec<Value> =
            serde_json::from_str(line).with_context(|| format!("not a JSON array: {line}"))?;
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
        bail!(
            "{} distinct lines, the extraction printed {}: lines are missing",
            lines.len(),
            x.lines
        );
    }
    let joined: Vec<&str> = lines.iter().copied().collect();
    let digest = sha256_hex(joined.join("\n").as_bytes());
    if digest != x.digest {
        bail!(
            "the lines' sha256 is {digest}, the extraction's {}: a line was altered",
            x.digest
        );
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
                x.channels
                    .insert(text(r, 1, line)?, (text(r, 2, line)?, text(r, 3, line)?));
            }
            "member" => {
                x.members
                    .entry(text(r, 1, line)?)
                    .or_default()
                    .insert(text(r, 2, line)?, int(r, 3, line)?);
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
            "read" => x
                .reads
                .push((text(r, 1, line)?, text(r, 2, line)?, text(r, 3, line)?)),
            "react" => x.reactions.push((
                text(r, 1, line)?,
                text(r, 2, line)?,
                text(r, 3, line)?,
                field(r, 4, line)?.as_bool().unwrap_or(false),
            )),
            "host" => x.hosts.push((
                text(r, 1, line)?,
                field(r, 2, line)?.as_bool().unwrap_or(false),
            )),
            other => bail!("unknown line kind '{other}': {line}"),
        }
    }
    for m in x.messages.values() {
        for a in std::iter::once(&m.sender).chain(&m.recipient) {
            if !x.agents.contains_key(a) {
                bail!(
                    "message {} names agent {a}, which the extraction does not list",
                    m.id
                );
            }
        }
        if let Some(c) = &m.channel
            && !x.channels.contains_key(c)
        {
            bail!(
                "message {} is in channel {c}, which the extraction does not list",
                m.id
            );
        }
    }
    if let Some(c) = x.members.keys().find(|c| !x.channels.contains_key(*c)) {
        bail!("a membership of channel {c}, which the extraction does not list");
    }
    let unknown = x
        .reads
        .iter()
        .map(|r| &r.0)
        .chain(x.reactions.iter().map(|r| &r.0))
        .chain(x.attachments.iter().map(|a| &a.message_id));
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
        self.dms.values().chain(self.posts.values()).any(|e| {
            if imported {
                e.imported > 0
            } else {
                e.later > 0
            }
        })
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
}

pub fn classify(x: Extract, snap: &Snapshot) -> Inventory {
    let owner = x.owner.clone();
    let owner_of = |agent: &str| x.agents.get(agent).map(|a| a.owner.as_str()).unwrap_or("");
    // A channel is owner-internal when every sender ever and every current member is the owner's.
    let mut internal: BTreeMap<&str, bool> =
        x.channels.keys().map(|c| (c.as_str(), true)).collect();
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
    let atts: BTreeMap<&str, Vec<(String, String)>> =
        x.attachments.iter().fold(BTreeMap::new(), |mut acc, a| {
            acc.entry(a.message_id.as_str())
                .or_default()
                .push((a.filename.clone(), a.sha256.clone()));
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
            if own {
                Class::OwnerInternal
            } else {
                Class::Shared
            }
        };
        class.insert(m.id.clone(), c);
    }
    let snapshot_missing: Vec<String> = snap
        .messages
        .keys()
        .filter(|id| !x.messages.contains_key(*id))
        .cloned()
        .collect();

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
                binding: a
                    .kind
                    .as_ref()
                    .map(|k| format!("{k}@{}", a.host.as_deref().unwrap_or("?"))),
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
                        row.dms
                            .entry(x.agents[other].canonical())
                            .or_default()
                            .add(c, &m.created_at);
                    }
                }
            }
            (Some(ch), _) if !internal[ch.as_str()] => {
                if let Some(row) = names.get_mut(&m.sender) {
                    row.posts
                        .entry(format!("#{}", x.channels[ch].0))
                        .or_default()
                        .add(c, &m.created_at);
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
        for a in x
            .members
            .get(id)
            .map(|m| m.keys().collect::<Vec<_>>())
            .unwrap_or_default()
        {
            *members.entry(owner_of(a).to_string()).or_default() += 1;
        }
        let mut senders: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        let mut posts: BTreeMap<Class, usize> = BTreeMap::new();
        let mut last_post: Option<String> = None;
        for m in x
            .messages
            .values()
            .filter(|m| m.channel.as_deref() == Some(id))
        {
            senders
                .entry(owner_of(&m.sender).to_string())
                .or_default()
                .insert(&m.sender);
            *posts.entry(class[&m.id]).or_default() += 1;
            if last_post
                .as_deref()
                .is_none_or(|l| m.created_at.as_str() > l)
            {
                last_post = Some(m.created_at.clone());
            }
        }
        channels.push(ChannelRow {
            name: name.clone(),
            imported: snap.channels.contains(name),
            creator: x
                .agents
                .get(creator)
                .map(XAgent::canonical)
                .unwrap_or_else(|| creator.clone()),
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
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
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
    [
        with(".md"),
        with("-messages.tsv"),
        with("-attachments.tsv"),
        with("-names.tsv"),
    ]
}

impl Inventory {
    fn party(&self, agent: &str) -> String {
        self.x
            .agents
            .get(agent)
            .map(XAgent::canonical)
            .unwrap_or_else(|| agent.to_string())
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
        let mut v: Vec<&XMessage> = self
            .x
            .messages
            .values()
            .filter(|m| self.class[&m.id] == c)
            .collect();
        v.sort_by_key(|m| m.seq);
        v
    }

    fn attachments_of(&self, id: &str) -> Vec<&XAttachment> {
        self.x
            .attachments
            .iter()
            .filter(|a| a.message_id == id)
            .collect()
    }

    pub fn class_digest(&self, c: Class) -> String {
        rows_digest(
            self.ids(c)
                .into_iter()
                .map(|m| (m.id.as_str(), m.sha256.as_str())),
        )
    }

    fn header(&self) -> String {
        format!(
            "# owner: {}\n# freeze: commit_seq {} ({} messages on the board), extracted {}\n# extraction: {} lines, sha256 {}\n",
            self.x.owner,
            self.x.freeze_seq,
            self.x.board_messages,
            self.x.taken_at,
            self.x.lines,
            self.x.digest
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
        all.sort_by(|a, b| {
            (self.x.messages[&a.message_id].seq, &a.id)
                .cmp(&(self.x.messages[&b.message_id].seq, &b.id))
        });
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
                if n.cross_owner() && n.host.is_some() {
                    "yes"
                } else {
                    "no"
                },
                esc(&evidence(n))
            );
        }
        out
    }

    pub fn markdown(&self, source: &str) -> String {
        let o = &self.x.owner;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "# History manifest, owner {o} ({})\n",
            self.x.taken_at.get(..10).unwrap_or("")
        );
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
        let _ = writeln!(
            s,
            "Source: {source}. The extraction printed {} lines with sha256 `{}`; the lines read back hash to the same value.\n",
            self.x.lines, self.x.digest
        );
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
            let atts = self
                .x
                .attachments
                .iter()
                .filter(|a| self.class[&a.message_id] == c)
                .count();
            let _ = writeln!(
                s,
                "| {} | {} | {} | `{}` |",
                c.title(),
                self.count(c),
                atts,
                self.class_digest(c)
            );
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
             - the snapshot read is complete: no parse error (any would have refused this manifest)\n",
            self.snapshot_messages,
            self.snapshot_messages - self.snapshot_missing.len(),
            list_or_none(&self.snapshot_missing),
            list_or_none(&self.snapshot_differs),
        );

        let dangling: Vec<String> = {
            let mut v: Vec<&XMessage> = self
                .x
                .messages
                .values()
                .filter(|m| {
                    m.reply_to
                        .as_ref()
                        .is_some_and(|r| !self.x.messages.contains_key(r))
                })
                .collect();
            v.sort_by_key(|m| m.seq);
            v.iter()
                .map(|m| {
                    format!(
                        "`{}` ({}) replies to `{}`",
                        m.id,
                        self.class[&m.id].letter(),
                        m.reply_to.as_deref().unwrap_or("")
                    )
                })
                .collect()
        };
        let _ = writeln!(
            s,
            "Reply-to links that resolve to no message on the board: {}.\n",
            if dangling.is_empty() {
                "none".to_string()
            } else {
                dangling.join("; ")
            }
        );

        s.push_str("## Names\n\n");
        let mine = &self.names;
        let imported: Vec<&NameRow> = mine.iter().filter(|n| n.imported).collect();
        let bound_imported = imported.iter().filter(|n| n.binding.is_some()).count();
        let crossing: Vec<&NameRow> = mine.iter().filter(|n| n.cross_owner()).collect();
        let imported_later: Vec<&NameRow> = imported
            .iter()
            .copied()
            .filter(|n| n.cross_in(false))
            .collect();
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
                imported_later
                    .iter()
                    .map(|n| format!("`{}`", n.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        s.push_str("Every name with a cross-owner relationship or not in the snapshot (all others: `-names.tsv`, relationship none):\n\n");
        s.push_str(
            "| name | imported | binding | cross-owner | evidence |\n|---|---|---|---|---|\n",
        );
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
            let by = |m: &BTreeMap<String, usize>| {
                m.iter()
                    .map(|(o, n)| format!("{o} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let p = |k| c.posts.get(&k).copied().unwrap_or(0);
            let _ = writeln!(
                s,
                "| #{} | {} | {} | {} | {} | {}/{}/{} | {} | {} |",
                c.name,
                if c.imported { "yes" } else { "no" },
                c.creator,
                by(&c.members),
                if c.senders.is_empty() {
                    "none".into()
                } else {
                    by(&c.senders)
                },
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
            let cands: Vec<&NameRow> = mine
                .iter()
                .filter(|n| n.cross_owner() && n.host.as_deref() == Some(host.as_str()))
                .collect();
            let bound = mine
                .iter()
                .filter(|n| n.host.as_deref() == Some(host.as_str()))
                .count();
            let _ = writeln!(
                s,
                "- `{host}` ({}; {bound} bound names): {}",
                if *active {
                    "host key active"
                } else {
                    "no active host key"
                },
                if cands.is_empty() {
                    "no candidates".to_string()
                } else {
                    cands
                        .iter()
                        .map(|n| format!("`{}`", n.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
        }
        let unhosted: Vec<&NameRow> = crossing
            .iter()
            .copied()
            .filter(|n| n.host.is_none())
            .collect();
        if !unhosted.is_empty() {
            let _ = writeln!(
                s,
                "- unbound names with a cross-owner relationship (no host, so no candidate): {}",
                unhosted
                    .iter()
                    .map(|n| format!("`{}`", n.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        s.push_str("\n## Restore on the switch (H2 + H3)\n\n");
        let _ = writeln!(
            s,
            "The recipe is docs/SWITCH-v2-history.md. H2 imports a fresh snapshot of the kond directory taken at the\n\
             switch, minus the owner's pruned names; its class (a) part is the {a_n} rows above, so with no name\n\
             pruned its bodies digest is `{a}`. H3 restores the class (b) rows that the purge of the shared board\n\
             saved (checked against the hashes above), reads back the class (b) rows kept there between kept\n\
             names, and restores the owner's saved DMs newer than commit_seq {freeze}; any other row stops it.\n",
            a_n = self.count(Class::Imported),
            a = self.class_digest(Class::Imported),
            freeze = self.x.freeze_seq,
        );

        for c in [Class::OwnerInternal, Class::Shared] {
            let rows = self.ids(c);
            let _ = writeln!(s, "\n## Class {} rows ({})\n", c.title(), rows.len());
            if c == Class::OwnerInternal {
                s.push_str("For the owner's review: H3 restores the ones the purge saved and verifies the rest on the shared\nboard; H4 retires them from central after the copy validates. Privacy is not inferred from\nmembership alone.\n\n");
            } else {
                s.push_str("These stay on central (H4 does not touch them).\n\n");
            }
            s.push_str("| seq | id | date | from | to | bytes | attachments | reply to |\n|---:|---|---|---|---|---:|---|---|\n");
            for m in rows {
                let atts: Vec<String> = self
                    .attachments_of(&m.id)
                    .iter()
                    .map(|a| md(&a.filename))
                    .collect();
                let _ = writeln!(
                    s,
                    "| {} | `{}` | {} | {} | {} | {} | {} | {} |",
                    m.seq,
                    m.id,
                    m.created_at,
                    md(&self.party(&m.sender)),
                    md(&self.to(m)),
                    m.bytes,
                    if atts.is_empty() {
                        "-".into()
                    } else {
                        atts.join(", ")
                    },
                    m.reply_to
                        .as_deref()
                        .map(|r| format!("`{r}`"))
                        .unwrap_or_else(|| "-".into())
                );
            }
        }
        s
    }
}

fn list_or_none(v: &[String]) -> String {
    if v.is_empty() {
        "none".to_string()
    } else {
        v.join(" ")
    }
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
    let mut out: Vec<String> = n
        .dms
        .iter()
        .map(|(who, e)| fmt(&format!("DM {who}"), e))
        .collect();
    out.extend(n.posts.iter().map(|(c, e)| fmt(&format!("posts {c}"), e)));
    if !n.member_only.is_empty() {
        out.push(format!(
            "member of {}",
            n.member_only.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if out.is_empty() {
        "-".to_string()
    } else {
        out.join("; ")
    }
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
pub fn inventory_main(
    extract: &Path,
    snapshot: &Path,
    out: &Path,
    source: &str,
) -> anyhow::Result<i32> {
    let snap = crate::import::snapshot(snapshot)?;
    let raw =
        fs::read_to_string(extract).with_context(|| format!("reading {}", extract.display()))?;
    let x = parse_extract(&raw).with_context(|| format!("checking {}", extract.display()))?;
    let inv = classify(x, &snap);
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
    Ok(i32::from(
        !inv.snapshot_missing.is_empty() || !inv.snapshot_differs.is_empty(),
    ))
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
    /// The board's max commit_seq at the freeze.
    pub freeze_seq: i64,
    pub rows: Vec<ManifestRow>,
    /// Agent id on the frozen board -> name, for the owner's agents.
    pub agent_names: HashMap<String, String>,
}

impl Manifest {
    pub fn class(&self, c: Class) -> Vec<&ManifestRow> {
        self.rows.iter().filter(|r| r.class == c).collect()
    }
}

fn data_lines(raw: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    raw.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .skip(1)
        .map(|l| l.split('\t').map(unesc).collect())
}

pub fn read_manifest(stem: &Path) -> anyhow::Result<Manifest> {
    let [_, messages, attachments, _] = manifest_paths(stem);
    let raw =
        fs::read_to_string(&messages).with_context(|| format!("reading {}", messages.display()))?;
    let owner = raw
        .lines()
        .find_map(|l| l.strip_prefix("# owner: "))
        .ok_or_else(|| anyhow!("{} has no owner line", messages.display()))?
        .to_string();
    let freeze_seq = raw
        .lines()
        .find_map(|l| l.strip_prefix("# freeze: commit_seq "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow!("{} has no freeze line", messages.display()))?;
    let mut atts: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let raw_atts = fs::read_to_string(&attachments)
        .with_context(|| format!("reading {}", attachments.display()))?;
    for f in data_lines(&raw_atts) {
        if f.len() != 6 {
            bail!(
                "{}: a line has {} fields, not 6",
                attachments.display(),
                f.len()
            );
        }
        atts.entry(f[1].clone())
            .or_default()
            .push((f[3].clone(), f[5].clone()));
    }
    let mut rows = Vec::new();
    for f in data_lines(&raw) {
        if f.len() != 10 {
            bail!(
                "{}: a line has {} fields, not 10",
                messages.display(),
                f.len()
            );
        }
        let class = Class::parse(&f[0]).ok_or_else(|| anyhow!("unknown class '{}'", f[0]))?;
        let mut a = atts.remove(&f[1]).unwrap_or_default();
        a.sort();
        if a.len().to_string() != f[9] {
            bail!(
                "{}: message {} lists {} attachments, the attachments file {}",
                messages.display(),
                f[1],
                f[9],
                a.len()
            );
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
        bail!(
            "{}: attachment of message {id}, which the messages file does not list",
            attachments.display()
        );
    }
    let [_, _, _, names] = manifest_paths(stem);
    let mut agent_names = HashMap::new();
    let raw_names =
        fs::read_to_string(&names).with_context(|| format!("reading {}", names.display()))?;
    for f in data_lines(&raw_names) {
        if f.len() < 2 {
            bail!("{}: a line has {} fields", names.display(), f.len());
        }
        agent_names.insert(f[1].clone(), f[0].clone());
    }
    Ok(Manifest {
        owner,
        freeze_seq,
        rows,
        agent_names,
    })
}

// --- H3: the copy from the purge's saved rows ---------------------------------------------

/// The records of a Postgres `COPY ... (FORMAT csv, HEADER)` file: an unquoted empty field is
/// NULL (None), a quoted one an empty string.
pub fn read_csv(raw: &str) -> anyhow::Result<Vec<Vec<Option<String>>>> {
    csv_records(raw)
}

fn csv_records(raw: &str) -> anyhow::Result<Vec<Vec<Option<String>>>> {
    let mut out = Vec::new();
    let mut rec: Vec<Option<String>> = Vec::new();
    let mut field = String::new();
    let (mut quoted, mut in_quotes, mut started) = (false, false, false);
    let mut chars = raw.chars().peekable();
    let push_field = |rec: &mut Vec<Option<String>>, field: &mut String, quoted: &mut bool| {
        rec.push(if field.is_empty() && !*quoted {
            None
        } else {
            Some(std::mem::take(field))
        });
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        started = true;
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                quoted = true;
            }
            ',' => push_field(&mut rec, &mut field, &mut quoted),
            '\n' => {
                push_field(&mut rec, &mut field, &mut quoted);
                out.push(std::mem::take(&mut rec));
                started = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            _ => field.push(c),
        }
    }
    if in_quotes {
        bail!("a quoted field is not closed: the file is truncated");
    }
    if !raw.is_empty() && !raw.ends_with('\n') {
        bail!("the last record is not ended by a line break: the file is truncated");
    }
    if started {
        push_field(&mut rec, &mut field, &mut quoted);
        out.push(rec);
    }
    Ok(out)
}

/// The rows of one table file, as column name -> value maps.
fn csv_table(dir: &Path, table: &str) -> anyhow::Result<Vec<HashMap<String, Option<String>>>> {
    let path = dir.join(format!("{table}.csv"));
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut records = csv_records(&raw)
        .with_context(|| format!("parsing {}", path.display()))?
        .into_iter();
    let header: Vec<String> = records
        .next()
        .ok_or_else(|| anyhow!("{} has no header", path.display()))?
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect();
    records
        .map(|r| {
            if r.len() != header.len() {
                bail!(
                    "{}: a record has {} fields, the header {}",
                    path.display(),
                    r.len(),
                    header.len()
                );
            }
            Ok(header.iter().cloned().zip(r).collect())
        })
        .collect()
}

fn col(row: &HashMap<String, Option<String>>, name: &str) -> Option<String> {
    row.get(name).cloned().flatten()
}

/// A bytea in COPY text form (`\x` + hex).
fn bytea(hex: &str) -> anyhow::Result<Vec<u8>> {
    let h = hex
        .strip_prefix("\\x")
        .ok_or_else(|| anyhow!("bytea without \\x"))?;
    if h.len() % 2 != 0 {
        bail!("odd bytea length");
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).map_err(|e| anyhow!("bytea: {e}")))
        .collect()
}

/// A message of the saved rows, with the parties resolved to the owner's names.
#[derive(Debug)]
struct Saved {
    id: String,
    seq: i64,
    from: String,
    to: String,
    text: String,
    reply_to: Option<String>,
    created_at: String,
    attachments: Vec<(String, Vec<u8>)>,
    read_at: Option<String>,
    /// (emoji, actor name)
    reactions: Vec<(String, String)>,
    /// In the manifest's class (b), so checked against its frozen hashes.
    frozen: bool,
}

/// Where the copy verifies the class (b) rows retained on the shared board: a host credential
/// and an actor that is a party to every retained DM (`message.get` reads without marking).
pub struct Central {
    pub server: String,
    pub key: String,
    pub actor: String,
}

pub struct CopyOptions {
    pub manifest: PathBuf,
    /// The directory of the purge's saved rows (one CSV per table, counts.txt, preview.txt).
    pub from_csv: PathBuf,
    pub db: PathBuf,
    /// Names whose rows stay out (the importer's prune list).
    pub skip_agents: BTreeSet<String>,
    /// Where the retained class (b) rows are verified; None refuses when any is expected.
    pub central: Option<Central>,
    pub dry_run: bool,
}

/// Every saved message and every manifest class (b) row lands in exactly one of these.
#[derive(Debug, Default)]
pub struct CopyReport {
    pub saved: usize,
    /// Saved rows of the manifest's class (a): on the local board by the H2 import.
    pub saved_imported: usize,
    /// Of those, rows of a pruned name: absent from the local board.
    pub skipped_imported: usize,
    pub saved_frozen: usize,
    pub saved_newer: usize,
    /// Restored by this copy: frozen class (b) and newer rows.
    pub restored_frozen: usize,
    pub restored_newer: Vec<String>,
    /// Saved rows with a pruned party (any class).
    pub skipped: Vec<String>,
    /// Manifest class (b) rows between kept names, each read back from the shared board.
    pub retained_verified: Vec<String>,
    pub kept_names: Vec<String>,
    pub attachments: usize,
    pub agents_created: Vec<String>,
    pub notes: Vec<String>,
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
        writeln!(f, "conde history copy")?;
        writeln!(
            f,
            "  saved messages: {} = class (a) {} (on the local board by the import {}, skipped by the prune list {}) + frozen class (b) {} + newer than the freeze {}",
            self.saved,
            self.saved_imported,
            self.saved_imported - self.skipped_imported,
            self.skipped_imported,
            self.saved_frozen,
            self.saved_newer
        )?;
        writeln!(
            f,
            "  restored here: {} frozen + {} newer, {} attachments; skipped by the prune list: {}",
            self.restored_frozen,
            self.restored_newer.len(),
            self.attachments,
            self.skipped.len()
        )?;
        writeln!(
            f,
            "  class (b) retained on the shared board (between {}): {} verified there",
            list_or_none(&self.kept_names),
            self.retained_verified.len()
        )?;
        writeln!(
            f,
            "  newer than the freeze: {}",
            list_or_none(&self.restored_newer)
        )?;
        writeln!(f, "  skipped: {}", list_or_none(&self.skipped))?;
        writeln!(
            f,
            "  local agents created (unbound): {}",
            list_or_none(&self.agents_created)
        )?;
        for n in &self.notes {
            writeln!(f, "  note: {n}")?;
        }
        for l in &self.lines {
            writeln!(f, "  {l}")?;
        }
        for p in &self.problems {
            writeln!(f, "  PROBLEM {p}")?;
        }
        writeln!(
            f,
            "  written: {}",
            if self.written {
                "yes, one transaction"
            } else {
                "nothing"
            }
        )?;
        writeln!(
            f,
            "result: {}",
            if self.ok() {
                "OK"
            } else {
                "ISSUES (see above)"
            }
        )
    }
}

/// The name part of `owner/name`, when the owner is `owner`.
fn own_name<'a>(qualified: &'a str, owner: &str) -> Option<&'a str> {
    qualified
        .strip_prefix(owner)
        .and_then(|r| r.strip_prefix('/'))
}

/// The tables the copy reads; each needs its stated count.
const SAVED_TABLES: [&str; 5] = ["agents", "messages", "attachments", "reactions", "dm_read"];

/// Row counts the purge stated in counts.txt: each `COPY ( SELECT ... FROM <table> ...)` line
/// followed by its `COPY <n>` line.
fn stated_counts(dir: &Path) -> anyhow::Result<BTreeMap<String, usize>> {
    let path = dir.join("counts.txt");
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = BTreeMap::new();
    let mut table: Option<String> = None;
    for line in raw.lines() {
        if let Some(n) = line
            .strip_prefix("COPY ")
            .and_then(|n| n.trim().parse::<usize>().ok())
        {
            if let Some(t) = table.take() {
                out.insert(t, n);
            }
        } else if line.starts_with("COPY ") {
            table = line
                .split_whitespace()
                .skip_while(|w| *w != "FROM")
                .nth(1)
                .map(str::to_string);
        }
    }
    Ok(out)
}

/// The saved rows are the purge's whole output: every table file holds the count counts.txt
/// states, and agents.csv names exactly the agents preview.txt lists as gone.
/// Whether every record could be read; false stops the copy before anything else.
fn check_source(dir: &Path, problems: &mut Vec<String>) -> anyhow::Result<bool> {
    let stated = stated_counts(dir)?;
    let before = problems.len();
    for t in stated
        .keys()
        .filter(|t| dir.join(format!("{t}.csv")).exists())
    {
        match csv_table(dir, t) {
            Ok(rows) => check_records(t, &rows, problems),
            Err(e) => problems.push(format!("{t}.csv: {e:#}")),
        }
    }
    if problems.len() > before {
        return Ok(false);
    }
    for t in SAVED_TABLES {
        if !stated.contains_key(t) {
            problems.push(format!("counts.txt states no count for {t}"));
        }
    }
    for (t, n) in &stated {
        let path = dir.join(format!("{t}.csv"));
        if !path.exists() {
            if *n > 0 || SAVED_TABLES.contains(&t.as_str()) {
                problems.push(format!("{t}.csv is missing; counts.txt states {n} rows"));
            }
            continue;
        }
        let have = csv_table(dir, t)?.len();
        if have != *n {
            problems.push(format!("{t}.csv holds {have} rows, counts.txt states {n}"));
        }
    }
    let path = dir.join("preview.txt");
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let listed: BTreeSet<String> = raw
        .lines()
        .filter_map(|l| l.strip_prefix("gone agent | "))
        .map(|n| n.trim().to_string())
        .collect();
    let saved: BTreeSet<String> = csv_table(dir, "agents")?
        .iter()
        .filter_map(|a| col(a, "name_lower"))
        .collect();
    if listed != saved {
        let only = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
            a.difference(b).cloned().collect::<Vec<_>>().join(" ")
        };
        problems.push(format!(
            "agents.csv and preview.txt differ: only in agents.csv: [{}]; only in preview.txt: [{}]",
            only(&saved, &listed),
            only(&listed, &saved)
        ));
    }
    Ok(true)
}

/// A conde timestamp: `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn is_stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 24
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'.',
            23 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// Every field of every record typed: ids are ULIDs, times are conde timestamps, numbers parse,
/// bytea decodes to its stated size. A malformed record refuses the whole source.
fn check_records(
    table: &str,
    rows: &[HashMap<String, Option<String>>],
    problems: &mut Vec<String>,
) {
    let ulid = |v: &Option<String>| v.as_deref().is_some_and(crate::util::is_ulid);
    let opt_ulid = |v: &Option<String>| v.is_none() || ulid(v);
    let stamp = |v: &Option<String>| v.as_deref().is_some_and(is_stamp);
    let int = |v: &Option<String>| v.as_deref().is_some_and(|s| s.parse::<i64>().is_ok());
    let text = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.is_empty());
    let get = |r: &HashMap<String, Option<String>>, k: &str| r.get(k).cloned().flatten();
    for (i, r) in rows.iter().enumerate() {
        let mut bad: Vec<&str> = Vec::new();
        let mut need = |ok: bool, field: &'static str| {
            if !ok {
                bad.push(field);
            }
        };
        match table {
            "agents" => {
                need(ulid(&get(r, "agent_id")), "agent_id");
                need(int(&get(r, "owner_id")), "owner_id");
                need(text(&get(r, "name_lower")), "name_lower");
            }
            "messages" => {
                need(int(&get(r, "commit_seq")), "commit_seq");
                need(ulid(&get(r, "message_id")), "message_id");
                need(ulid(&get(r, "sender_agent_id")), "sender_agent_id");
                let (rcpt, chan) = (get(r, "recipient_agent_id"), get(r, "channel_id"));
                need(
                    opt_ulid(&rcpt) && opt_ulid(&chan) && (rcpt.is_none() != chan.is_none()),
                    "recipient_agent_id/channel_id",
                );
                need(r.get("text").is_some_and(Option::is_some), "text");
                need(
                    get(r, "reply_to").is_none() || text(&get(r, "reply_to")),
                    "reply_to",
                );
                need(stamp(&get(r, "created_at")), "created_at");
            }
            "attachments" => {
                need(ulid(&get(r, "attachment_id")), "attachment_id");
                need(ulid(&get(r, "message_id")), "message_id");
                need(text(&get(r, "filename")), "filename");
                let size = get(r, "size").and_then(|s| s.parse::<usize>().ok());
                let len = get(r, "bytes")
                    .and_then(|b| bytea(&b).ok())
                    .map(|b| b.len());
                need(size.is_some() && size == len, "bytes/size");
            }
            "reactions" => {
                need(ulid(&get(r, "message_id")), "message_id");
                need(ulid(&get(r, "actor_id")), "actor_id");
                need(text(&get(r, "emoji")), "emoji");
                need(
                    matches!(get(r, "present").as_deref(), Some("t" | "f")),
                    "present",
                );
            }
            "dm_read" => {
                need(ulid(&get(r, "message_id")), "message_id");
                need(ulid(&get(r, "agent_id")), "agent_id");
                need(stamp(&get(r, "read_at")), "read_at");
            }
            _ => {}
        }
        if !bad.is_empty() {
            let id = ["message_id", "agent_id", "attachment_id"]
                .iter()
                .find_map(|k| get(r, k))
                .unwrap_or_default();
            problems.push(format!(
                "{table}.csv record {} ({id}): malformed {}",
                i + 1,
                bad.join(", ")
            ));
        }
    }
}

/// Every saved message with its parties as the owner's names; attachments, read state and
/// reactions only for `wanted` ids. A party that is not the owner's, or a channel post, is a
/// problem (the purge saved one owner's DMs).
fn read_saved(
    m: &Manifest,
    dir: &Path,
    report: &mut CopyReport,
) -> anyhow::Result<(Vec<Saved>, BTreeSet<String>)> {
    let owner = &m.owner;
    let mut names: HashMap<String, String> = m.agent_names.clone();
    let mut gone = BTreeSet::new();
    let mut owner_ids = BTreeSet::new();
    for a in csv_table(dir, "agents")? {
        let (Some(id), Some(name)) = (col(&a, "agent_id"), col(&a, "name_lower")) else {
            bail!("agents.csv: a row without id or name")
        };
        owner_ids.insert(col(&a, "owner_id").unwrap_or_default());
        gone.insert(name.clone());
        names.insert(id, name);
    }
    if owner_ids.len() > 1 {
        report.problems.push(format!(
            "agents.csv holds agents of {} owners; the saved rows are one owner's",
            owner_ids.len()
        ));
    }
    let mut out = Vec::new();
    for r in csv_table(dir, "messages")? {
        let id = col(&r, "message_id").ok_or_else(|| anyhow!("messages.csv: a row without id"))?;
        if col(&r, "channel_id").is_some() {
            report.problems.push(format!(
                "{id}: a channel post among the saved rows; the copy restores DMs only"
            ));
            continue;
        }
        let party = |k: &str| col(&r, k).and_then(|a| names.get(&a).cloned());
        let (Some(from), Some(to)) = (party("sender_agent_id"), party("recipient_agent_id")) else {
            report
                .problems
                .push(format!("{id}: a party is not one of {owner}'s agents"));
            continue;
        };
        out.push(Saved {
            id,
            seq: col(&r, "commit_seq")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            from: format!("{owner}/{from}"),
            to: format!("{owner}/{to}"),
            text: col(&r, "text").unwrap_or_default(),
            reply_to: col(&r, "reply_to"),
            created_at: col(&r, "created_at").unwrap_or_default(),
            attachments: Vec::new(),
            read_at: None,
            reactions: Vec::new(),
            frozen: false,
        });
    }
    out.sort_by_key(|s| s.seq);
    Ok((out, gone))
}

/// Attachments, read state and reactions of the rows to restore.
fn read_dependents(
    m: &Manifest,
    dir: &Path,
    rows: &mut [Saved],
    report: &mut CopyReport,
) -> anyhow::Result<()> {
    let owner = &m.owner;
    let mut names: HashMap<String, String> = m.agent_names.clone();
    for a in csv_table(dir, "agents")? {
        if let (Some(id), Some(name)) = (col(&a, "agent_id"), col(&a, "name_lower")) {
            names.insert(id, name);
        }
    }
    let index: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    for a in csv_table(dir, "attachments")? {
        let Some(i) = col(&a, "message_id").and_then(|m| index.get(&m).copied()) else {
            continue;
        };
        let bytes = bytea(&col(&a, "bytes").unwrap_or_default())?;
        rows[i]
            .attachments
            .push((col(&a, "filename").unwrap_or_default(), bytes));
    }
    for d in csv_table(dir, "dm_read")? {
        let Some(i) = col(&d, "message_id").and_then(|m| index.get(&m).copied()) else {
            continue;
        };
        let reader = col(&d, "agent_id")
            .and_then(|a| names.get(&a).cloned())
            .map(|n| format!("{owner}/{n}"));
        if reader.as_deref() == Some(rows[i].to.as_str()) {
            rows[i].read_at = col(&d, "read_at");
        }
    }
    for x in csv_table(dir, "reactions")? {
        let Some(i) = col(&x, "message_id").and_then(|m| index.get(&m).copied()) else {
            continue;
        };
        if col(&x, "present").as_deref() != Some("t") {
            continue;
        }
        match col(&x, "actor_id").and_then(|a| names.get(&a).cloned()) {
            Some(actor) => rows[i].reactions.push((
                col(&x, "emoji").unwrap_or_default(),
                format!("{owner}/{actor}"),
            )),
            None => report.problems.push(format!(
                "{}: a reaction by an agent that is not {owner}'s",
                rows[i].id
            )),
        }
    }
    report.attachments = rows.iter().map(|s| s.attachments.len()).sum();
    Ok(())
}

/// Read each retained row back from the shared board as `central.actor`; its body must hash
/// to the manifest's.
fn verify_retained(
    central: &Central,
    rows: &[&ManifestRow],
    report: &mut CopyReport,
) -> anyhow::Result<()> {
    let url = crate::credentials::rpc_url(&central.server)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for r in rows {
        let req =
            serde_json::json!({"op": "message.get", "actor": central.actor, "body": {"id": r.id}});
        let answer = rt.block_on(conde_remote::rpc(
            &url,
            &central.key,
            req,
            std::time::Duration::from_secs(30),
        ));
        let text = match &answer {
            Ok(v) if v["ok"].as_bool() == Some(true) => {
                v["result"]["message"]["text"].as_str().map(str::to_string)
            }
            _ => None,
        };
        match text {
            Some(t) if sha256_hex(t.as_bytes()) == r.sha256 => {
                report.retained_verified.push(r.id.clone())
            }
            Some(_) => report.problems.push(format!(
                "{}: on the shared board with a body that differs from the manifest",
                r.id
            )),
            None => {
                let why = match answer {
                    Ok(v) => v["error"]["message"]
                        .as_str()
                        .unwrap_or("no message")
                        .to_string(),
                    Err(e) => format!("{e:?}"),
                };
                report.problems.push(format!(
                    "{}: expected on the shared board and not readable there ({why})",
                    r.id
                ));
            }
        }
    }
    Ok(())
}
fn check_frozen(s: &Saved, want: &ManifestRow, problems: &mut Vec<String>) {
    let id = &s.id;
    let sha = sha256_hex(s.text.as_bytes());
    if sha != want.sha256 {
        problems.push(format!(
            "{id}: body sha256 {sha}, the manifest {}",
            want.sha256
        ));
    }
    for (what, got, exp) in [
        ("sender", s.from.clone(), want.from.clone()),
        ("recipient", s.to.clone(), want.to.clone()),
        ("date", s.created_at.clone(), want.created_at.clone()),
        (
            "reply-to",
            s.reply_to.clone().unwrap_or_default(),
            want.reply_to.clone().unwrap_or_default(),
        ),
    ] {
        if got != exp {
            problems.push(format!(
                "{id}: {what} is {got} in the saved rows, {exp} in the manifest"
            ));
        }
    }
    let mut hashes: Vec<(String, String)> = s
        .attachments
        .iter()
        .map(|(n, b)| (n.clone(), sha256_hex(b)))
        .collect();
    hashes.sort();
    if hashes != want.attachments {
        problems.push(format!(
            "{id}: attachments {hashes:?} differ from the manifest's {:?}",
            want.attachments
        ));
    }
}

/// The newest local DM between two of the owner's names, as (created_at, id).
fn newest_local(
    tx: &dyn Tx,
    user_id: i64,
    a: &str,
    b: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let (Some(a), Some(b)) = (tx.agent_by_name(user_id, a)?, tx.agent_by_name(user_id, b)?) else {
        return Ok(None);
    };
    let mut newest: Option<(String, String)> = None;
    for (from, to) in [(&a, &b), (&b, &a)] {
        let mut after = 0;
        loop {
            let page = tx.sent_by(&from.id, after, 500)?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            for m in page
                .into_iter()
                .filter(|m| m.recipient_id.as_deref() == Some(to.id.as_str()))
            {
                if newest.as_ref().is_none_or(|(at, _)| m.created_at > *at) {
                    newest = Some((m.created_at, m.id));
                }
            }
        }
    }
    Ok(newest)
}

pub fn copy(opts: &CopyOptions) -> anyhow::Result<CopyReport> {
    let manifest = read_manifest(&opts.manifest)?;
    let owner = manifest.owner.clone();
    let mut report = CopyReport::default();
    if !opts.db.exists() {
        bail!(
            "{} does not exist: the H2 import restores the local board first",
            opts.db.display()
        );
    }
    // 1. The source is the purge's whole output, every record well formed.
    if !check_source(&opts.from_csv, &mut report.problems)? {
        return Ok(report);
    }
    let (saved, gone) = read_saved(&manifest, &opts.from_csv, &mut report)?;
    report.saved = saved.len();

    // 2. Every saved row and every class (b) row lands in exactly one category.
    let by_id: HashMap<&str, &ManifestRow> =
        manifest.rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let saved_ids: BTreeSet<String> = saved.iter().map(|s| s.id.clone()).collect();
    let pruned = |s: &str| own_name(s, &owner).is_some_and(|n| opts.skip_agents.contains(n));
    let mut imported_rows = Vec::new();
    let mut restore = Vec::new();
    for mut s in saved {
        match by_id.get(s.id.as_str()).map(|r| r.class) {
            Some(Class::Imported) => {
                report.saved_imported += 1;
                if by_id[s.id.as_str()].sha256 != sha256_hex(s.text.as_bytes()) {
                    report.problems.push(format!(
                        "{}: a saved class (a) row whose body differs from the manifest",
                        s.id
                    ));
                }
                let skipped = pruned(&s.from) || pruned(&s.to);
                report.skipped_imported += usize::from(skipped);
                imported_rows.push((s.id.clone(), skipped));
                continue;
            }
            Some(Class::Shared) => {
                report.problems.push(format!(
                    "{}: a saved row the manifest classes as shared",
                    s.id
                ));
                continue;
            }
            Some(Class::OwnerInternal) => {
                report.saved_frozen += 1;
                s.frozen = true;
            }
            None if s.seq > manifest.freeze_seq => report.saved_newer += 1,
            None => {
                report.problems.push(format!(
                    "{}: a saved row at or before the freeze that the manifest does not list",
                    s.id
                ));
                continue;
            }
        }
        if pruned(&s.from) || pruned(&s.to) {
            report.skipped.push(s.id);
        } else {
            restore.push(s);
        }
    }
    let names: BTreeSet<&String> = manifest.agent_names.values().collect();
    report.kept_names = names
        .iter()
        .filter(|n| !gone.contains(**n))
        .map(|n| n.to_string())
        .collect();
    let kept =
        |q: &str| own_name(q, &owner).is_some_and(|n| report.kept_names.iter().any(|k| k == n));
    let mut retained = Vec::new();
    for r in manifest.class(Class::OwnerInternal) {
        let between_kept = kept(&r.from) && kept(&r.to);
        match (saved_ids.contains(&r.id), between_kept) {
            (true, false) => {}
            (true, true) => report.problems.push(format!("{}: saved by the purge, though both its parties were kept", r.id)),
            (false, true) => retained.push(r),
            (false, false) => report.problems.push(format!(
                "{}: in the manifest's class (b), not among the saved rows, and a party ({} or {}) was purged: missing",
                r.id, r.from, r.to
            )),
        }
    }
    match (&opts.central, retained.is_empty()) {
        (_, true) => {}
        (Some(c), false) => verify_retained(c, &retained, &mut report)?,
        (None, false) => report.problems.push(format!(
            "{} class (b) rows are expected on the shared board; give the credential and actor to verify them",
            retained.len()
        )),
    }
    report.restored_frozen = restore.iter().filter(|s| s.frozen).count();
    report.restored_newer = restore
        .iter()
        .filter(|s| !s.frozen)
        .map(|s| s.id.clone())
        .collect();
    read_dependents(&manifest, &opts.from_csv, &mut restore, &mut report)?;
    let frozen: HashMap<&str, &ManifestRow> = manifest
        .class(Class::OwnerInternal)
        .into_iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    for s in restore.iter().filter(|s| s.frozen) {
        check_frozen(s, frozen[s.id.as_str()], &mut report.problems);
    }
    let b_total = manifest.class(Class::OwnerInternal).len();
    let b_skipped = report
        .skipped
        .iter()
        .filter(|i| frozen.contains_key(i.as_str()))
        .count();
    report.lines.push(format!(
        "class (b) in the manifest: {b_total} = restored {} + skipped {b_skipped} + retained on the shared board {}",
        report.restored_frozen,
        retained.len()
    ));
    if report.restored_frozen + b_skipped + retained.len() != b_total && report.problems.is_empty()
    {
        report
            .problems
            .push("the class (b) rows do not add up".to_string());
    }

    // 3. Against the local board as the H2 import left it.
    let store = SqliteStore::open(&opts.db)?;
    let user_id = {
        let tx = store.begin(Mode::Read)?;
        let user_id = tx.user_id(&owner)?.ok_or_else(|| {
            anyhow!(
                "{} has no user {owner}: run the H2 import (conde import --owner {owner}) first",
                opts.db.display()
            )
        })?;
        for (id, skipped) in &imported_rows {
            let local = tx.message_by_id(id)?;
            match (local, skipped) {
                (Some(m), false) if sha256_hex(m.text.as_bytes()) == by_id[id.as_str()].sha256 => {}
                (None, true) => {}
                (Some(_), false) => report.problems.push(format!(
                    "{id}: class (a) on the local board with another body"
                )),
                (None, false) => report.problems.push(format!(
                    "{id}: class (a), saved by the purge, not on the local board"
                )),
                (Some(_), true) => report.problems.push(format!(
                    "{id}: a pruned party's class (a) row is on the local board"
                )),
            }
        }
        let copied: BTreeSet<&str> = restore.iter().map(|s| s.id.as_str()).collect();
        let mut newest: BTreeMap<(String, String), Option<(String, String)>> = BTreeMap::new();
        for s in &restore {
            if tx.message_by_id(&s.id)?.is_some() {
                report.problems.push(format!(
                    "{} is already on the local board (a copy already ran)",
                    s.id
                ));
            }
            if let Some(reply) = &s.reply_to
                && !copied.contains(reply.as_str())
                && tx.message_by_id(reply)?.is_none()
            {
                report.problems.push(format!(
                    "{}: replies to {reply}, which is neither local nor copied",
                    s.id
                ));
            }
            let (a, b) = (
                own_name(&s.from, &owner).unwrap_or_default(),
                own_name(&s.to, &owner).unwrap_or_default(),
            );
            let key = if a <= b {
                (a.to_string(), b.to_string())
            } else {
                (b.to_string(), a.to_string())
            };
            if !newest.contains_key(&key) {
                let n = newest_local(&*tx, user_id, &key.0, &key.1)?;
                newest.insert(key.clone(), n);
            }
            // A DM's read state is its own; an older copied DM is listed after newer local ones.
            if let Some((at, kond)) = &newest[&key]
                && s.created_at < *at
            {
                report.notes.push(format!(
                    "{} ({}) is older than {kond} ({at}) between {} and {}: listed after it",
                    s.id, s.created_at, key.0, key.1
                ));
            }
        }
        user_id
    };
    if !report.problems.is_empty() || opts.dry_run {
        if opts.dry_run {
            report
                .lines
                .push("dry run: read and checked, nothing written".to_string());
        }
        return Ok(report);
    }
    apply(&store, user_id, &owner, &restore, &mut report)?;
    report.written = true;
    validate(&store, &manifest, &restore, &mut report)?;
    Ok(report)
}

fn apply(
    store: &SqliteStore,
    user_id: i64,
    owner: &str,
    rows: &[Saved],
    report: &mut CopyReport,
) -> anyhow::Result<()> {
    let boxed = store.begin(Mode::Write)?;
    let tx = &*boxed;
    let mut agents: HashMap<String, String> = HashMap::new();
    let mut agent = |qualified: &str, report: &mut CopyReport| -> anyhow::Result<String> {
        if let Some(id) = agents.get(qualified) {
            return Ok(id.clone());
        }
        let name =
            own_name(qualified, owner).ok_or_else(|| anyhow!("{qualified} is not {owner}'s"))?;
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
    for s in rows {
        let sender = agent(&s.from, report)?;
        let recipient = agent(&s.to, report)?;
        let inserted = tx.insert_message_if_absent(&NewMessage {
            id: &s.id,
            sender_id: &sender,
            recipient_id: Some(&recipient),
            channel_id: None,
            text: &s.text,
            reply_to: s.reply_to.as_deref(),
            created_at: &s.created_at,
        })?;
        if !inserted {
            bail!("{} appeared on the local board during the copy", s.id);
        }
        for (name, bytes) in &s.attachments {
            tx.insert_attachment(&new_ulid(), &s.id, name, bytes)?;
        }
        if let Some(at) = &s.read_at {
            tx.mark_dm_read(&s.id, &recipient, at)?;
        }
        for (emoji, by) in &s.reactions {
            let actor = agent(by, report)?;
            tx.insert_reaction_if_absent(&s.id, &actor, emoji, true)?;
        }
    }
    boxed.commit()?;
    Ok(())
}

fn validate(
    store: &SqliteStore,
    manifest: &Manifest,
    rows: &[Saved],
    report: &mut CopyReport,
) -> anyhow::Result<()> {
    let frozen: HashMap<&str, &ManifestRow> = manifest
        .class(Class::OwnerInternal)
        .into_iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    let tx = store.begin(Mode::Read)?;
    let (mut present, mut bodies, mut atts, mut atts_ok, mut links, mut reads, mut reacts) =
        (0, 0, 0, 0, 0, 0, 0);
    for s in rows {
        let Some(m) = tx.message_by_id(&s.id)? else {
            report
                .problems
                .push(format!("{}: not on the local board after the copy", s.id));
            continue;
        };
        present += 1;
        // Frozen rows against the manifest, newer rows against the saved rows.
        let want_body = frozen
            .get(s.id.as_str())
            .map_or_else(|| sha256_hex(s.text.as_bytes()), |r| r.sha256.clone());
        let mut want_atts: Vec<(String, String)> = match frozen.get(s.id.as_str()) {
            Some(r) => r.attachments.clone(),
            None => s
                .attachments
                .iter()
                .map(|(n, b)| (n.clone(), sha256_hex(b)))
                .collect(),
        };
        want_atts.sort();
        if sha256_hex(m.text.as_bytes()) == want_body {
            bodies += 1;
        } else {
            report
                .problems
                .push(format!("{}: local body sha256 differs", s.id));
        }
        let mut local = Vec::new();
        for meta in tx.attachment_list(&s.id)? {
            if let Some(a) = tx.attachment(&meta.id)? {
                local.push((a.filename, sha256_hex(&a.bytes)));
            }
        }
        local.sort();
        atts += want_atts.len();
        if local == want_atts {
            atts_ok += want_atts.len();
        } else {
            report.problems.push(format!(
                "{}: local attachments {local:?} differ from {want_atts:?}",
                s.id
            ));
        }
        match &s.reply_to {
            Some(reply) if tx.message_by_id(reply)?.is_none() => {
                report.problems.push(format!(
                    "{}: reply-to {reply} does not resolve locally",
                    s.id
                ));
            }
            _ => links += 1,
        }
        let rcpt = m.recipient_id.clone().unwrap_or_default();
        if tx.dm_read_at(&s.id, &rcpt)? == s.read_at {
            reads += 1;
        } else {
            report.problems.push(format!(
                "{}: local read state differs from the saved rows",
                s.id
            ));
        }
        let mut local_reactions = tx.reactions(&s.id)?;
        let mut want = s.reactions.clone();
        local_reactions.sort();
        want.sort();
        if local_reactions == want {
            reacts += 1;
        } else {
            report.problems.push(format!(
                "{}: local reactions {local_reactions:?}, saved {want:?}",
                s.id
            ));
        }
    }
    let n = rows.len();
    let f = rows.iter().filter(|s| s.frozen).count();
    report.lines.push(format!(
        "ids: {present} of {n} on the local board ({f} frozen, {} newer)",
        n - f
    ));
    report.lines.push(format!("bodies: {bodies} of {n} sha256-equal (frozen rows to the manifest, newer rows to the saved rows)"));
    report
        .lines
        .push(format!("attachments: {atts_ok} of {atts} sha256-equal"));
    report
        .lines
        .push(format!("reply-to: {links} of {n} resolve locally"));
    report.lines.push(format!(
        "read state: {reads} of {n} equal to the saved rows"
    ));
    report.lines.push(format!(
        "reactions: {reacts} of {n} equal to the saved rows"
    ));
    report.lines.push(format!(
        "frozen bodies digest: {}",
        rows_digest(
            rows.iter()
                .filter(|s| s.frozen)
                .map(|s| (s.id.as_str(), frozen[s.id.as_str()].sha256.as_str()))
        )
    ));
    Ok(())
}

/// `conde history copy`: the H3 copy from the purge's saved rows into the local board. The
/// retained class (b) rows are verified on the board of `credentials` (default: the enrolled
/// one) as `verify_as` (default: this session's address).
pub fn copy_main(
    manifest: &Path,
    from_csv: &Path,
    db: Option<PathBuf>,
    skip_file: Option<&Path>,
    credentials: Option<PathBuf>,
    verify_as: Option<String>,
    dry_run: bool,
) -> anyhow::Result<i32> {
    let creds_path = match credentials {
        Some(p) => Some(p),
        None => {
            let dakai = crate::boards::path("dakai")?;
            let legacy = crate::credentials::path()?;
            [dakai, legacy].into_iter().find(|p| p.exists())
        }
    };
    let actor = verify_as.or_else(|| {
        crate::identity::derive_address(
            &crate::identity::Markers::from_env(),
            crate::identity::harness_ancestor,
        )
        .ok()
    });
    let central = match (creds_path, actor) {
        (Some(p), Some(actor)) => {
            let c = crate::credentials::load(&p)?;
            Some(Central {
                server: c.server,
                key: c.key,
                actor,
            })
        }
        _ => None,
    };
    let opts = CopyOptions {
        manifest: manifest.to_path_buf(),
        from_csv: from_csv.to_path_buf(),
        db: db.unwrap_or_else(|| crate::paths::data_dir().join("conde.db")),
        skip_agents: match skip_file {
            Some(p) => crate::import::read_name_list(p)?,
            None => BTreeSet::new(),
        },
        central,
        dry_run,
    };
    let report = copy(&opts)?;
    print!("{report}");
    Ok(i32::from(!report.ok()))
}

// --- the prune list -----------------------------------------------------------------------

/// The prune list as TSV: every kond name with its activity, oldest first.
pub fn prune_tsv(rows: &[crate::import::AgentActivity]) -> String {
    let mut out =
        String::from("name\tconde_name\tlast_activity\tsource\tmessages\tregistered\tpinned\n");
    for a in rows {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            esc(&a.name),
            esc(&a.conde_name),
            a.last_activity.as_deref().unwrap_or("-"),
            if a.messages > 0 {
                "message"
            } else if a.from_registration {
                "registration"
            } else {
                "none"
            },
            a.messages,
            a.registered.as_deref().unwrap_or("no"),
            if a.pinned { "yes" } else { "no" }
        );
    }
    out
}

/// `conde history prune-candidates`: the TSV to `out` (or stdout), a short table on stderr.
pub fn prune_main(snapshot: &Path, out: Option<&Path>) -> anyhow::Result<i32> {
    let rows = crate::import::agent_activity(snapshot)?;
    let tsv = prune_tsv(&rows);
    match out {
        Some(p) => fs::write(p, &tsv).with_context(|| format!("writing {}", p.display()))?,
        None => print!("{tsv}"),
    }
    let unregistered = rows.iter().filter(|a| a.registered.is_none()).count();
    let silent = rows.iter().filter(|a| a.messages == 0).count();
    eprintln!(
        "{} names: {} not in the kond registry, {} without any message",
        rows.len(),
        unregistered,
        silent
    );
    eprintln!("oldest last activity first:");
    eprintln!(
        "  {:<40} {:<11} {:>8}  registered",
        "name", "last", "messages"
    );
    for a in &rows {
        let day = a
            .last_activity
            .as_deref()
            .and_then(|d| d.get(..10))
            .unwrap_or("-");
        eprintln!(
            "  {:<40} {:<11} {:>8}  {}",
            a.name,
            day,
            a.messages,
            a.registered.as_deref().unwrap_or("no")
        );
    }
    Ok(0)
}
