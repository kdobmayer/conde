//! `conde import`: load a kond board (registry, maildir boxes, channel logs,
//! attachments, pins, profiles) into the board's store (the SQLite file, or
//! Postgres when a database URL is given), idempotently.
//!
//! kond keeps every delivered message twice (the sender's outbox/sent and the
//! recipient's inbox or the channel log); copies are folded by message id.
//! kond reactions are messages with a `Reaction` / `Reaction-Remove` header;
//! they become reaction rows (last event wins), not messages. Bounced mail,
//! notification markers and holds are runtime state and are not imported, and
//! no notification is created for an imported message.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use ulid::Ulid;

use crate::address::{Address, Kind, split_qualified};
use crate::maildir::MailMessage;
use crate::protocol::{MAX_ATTACHMENT, MAX_MESSAGE_ATTACHMENTS, MAX_TEXT};
use crate::store::postgres::{PgOpen, PgStore};
use crate::store::sqlite::SqliteStore;
use crate::store::{Channel, ImportState, ImportedAgent, Mode, NewMessage, Store, StoreResult, Tx};
use crate::util::{iso_from_ms, is_ulid, new_ulid, now_ms, sha256_hex};

const KNOWN_HEADERS: [&str; 7] = ["From", "To", "Date", "In-Reply-To", "Attachment", "Reaction", "Reaction-Remove"];

pub struct Options {
    /// The kond data root (`registry/`, `board/`, `state/`, `profiles/`).
    pub from: PathBuf,
    /// The conde SQLite file; a dry run never creates or writes it.
    pub db: PathBuf,
    /// A Postgres board instead of `db` (cloud mode); a dry run neither migrates nor writes it.
    pub database_url: Option<String>,
    pub owner: String,
    pub host: String,
    pub dry_run: bool,
    /// Session ids of live Claude sessions; None leaves liveness unchecked.
    pub live_claude: Option<HashSet<String>>,
    /// Progress lines on stdout (scan and write, every 1000 messages).
    pub progress: bool,
    /// The snapshot's file count: a directory with another count is refused before anything.
    pub expect_files: Option<usize>,
    /// kond names left out entirely, with every message they sent or received, their channel
    /// memberships and their reactions (the owner's prune list).
    pub skip_agents: BTreeSet<String>,
}

const PROGRESS_EVERY: usize = 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub agents: usize,
    pub channels: usize,
    pub messages: usize,
    pub attachments: usize,
    pub memberships: usize,
    pub dm_read: usize,
    pub reactions: usize,
}

impl Counts {
    pub fn rows(&self) -> [(&'static str, usize); 7] {
        [
            ("agents", self.agents),
            ("channels", self.channels),
            ("messages", self.messages),
            ("attachments", self.attachments),
            ("memberships", self.memberships),
            ("dm_read", self.dm_read),
            ("reactions", self.reactions),
        ]
    }
}

#[derive(Debug, Default)]
pub struct Validation {
    pub problems: Vec<String>,
    pub lines: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub from: PathBuf,
    pub db: PathBuf,
    /// Where the import goes, as printed (never a URL: it may carry a password).
    pub target: String,
    pub owner: String,
    pub host: String,
    pub dry_run: bool,
    /// Rows the source yields per table.
    pub source: Counts,
    /// Of those, rows already in conde before this run.
    pub present: Counts,
    /// Rows this run added (dry run: would add).
    pub added: Counts,
    pub notes: Vec<String>,
    pub pins: Vec<String>,
    /// Bindings to Claude sessions that are not live; None when unchecked.
    pub stale: Option<Vec<String>>,
    pub codex_bindings: usize,
    pub missing_attachments: Vec<String>,
    pub conflicts: Vec<String>,
    /// kond name -> conde name, for names invalid under conde's rule.
    pub renames: Vec<(String, String)>,
    pub parse_errors: Vec<String>,
    /// Channels that block the import (nothing is written while any is listed).
    pub refused: Vec<String>,
    /// What `skip_agents` left out.
    pub skipped: Option<Skipped>,
    pub validation: Option<Validation>,
}

/// Rows of the source left out by `Options::skip_agents`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Skipped {
    pub agents: Vec<String>,
    pub messages: usize,
    pub attachments: usize,
    pub memberships: usize,
    pub reactions: usize,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.parse_errors.is_empty()
            && self.refused.is_empty()
            && self.validation.as_ref().is_none_or(|v| v.problems.is_empty())
    }
}

// --- source model -------------------------------------------------------------

#[derive(Default)]
struct SrcAgent {
    registry: Option<String>,
    /// When the registry entry was written (the kond registration time).
    registered: Option<std::time::SystemTime>,
    pin: Option<String>,
    display: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum To {
    Agent(String),
    Channel(String),
}

struct SrcAttachment {
    filename: String,
    header: String,
    /// Path relative to the snapshot root, and size; or why it is not imported.
    file: Result<(PathBuf, u64), String>,
}

struct SrcMsg {
    path: PathBuf,
    digest: String,
    from: String,
    to: To,
    date: String,
    reply_to: Option<String>,
    body: String,
    attachments: Vec<SrcAttachment>,
    pending: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Loc {
    InboxNew,
    InboxCur,
    Sent,
    Pending,
    Channel,
}

#[derive(Default)]
struct Source {
    agents: BTreeMap<String, SrcAgent>,
    /// channel -> member -> cursor (last-read message id, "" = none).
    channels: BTreeMap<String, BTreeMap<String, String>>,
    messages: BTreeMap<String, SrcMsg>,
    /// (target id, actor, emoji) -> (event id, present).
    reactions: BTreeMap<(String, String, String), (String, bool)>,
    reaction_events: BTreeSet<String>,
    inbox_cur: HashSet<String>,
    stamps: HashMap<String, String>,
    bounced: Vec<String>,
    staging_leftovers: usize,
    unknown_headers: BTreeMap<String, usize>,
    divergent: Vec<String>,
    dates_repaired: usize,
    profile_extra: usize,
    state_counts: Vec<(String, usize)>,
    parse_errors: Vec<String>,
}

fn is_iso(s: &str) -> bool {
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

/// kond's `.read` stamp (`...:SSZ`) in the store's millisecond format.
fn normalize_stamp(s: &str) -> String {
    match s.strip_suffix('Z') {
        Some(head) if head.len() == 19 && is_iso(&format!("{head}.000Z")) => format!("{head}.000Z"),
        _ => s.to_string(),
    }
}

fn ulid_iso(id: &str) -> String {
    Ulid::from_string(id).map(|u| iso_from_ms(u.timestamp_ms() as i64)).unwrap_or_default()
}

struct Scanner<'a> {
    root: &'a Path,
    progress: bool,
    /// Message files read so far (progress lines).
    files: usize,
    src: Source,
    /// Symlinks already reported (one walk can meet the same one twice).
    symlinks: HashSet<PathBuf>,
}

impl Scanner<'_> {
    fn error(&mut self, path: &Path, what: impl fmt::Display) {
        self.src.parse_errors.push(format!("{}: {what}", path.display()));
    }

    fn symlink(&mut self, path: PathBuf) {
        if self.symlinks.insert(path.clone()) {
            self.error(&path, "symlink, not followed");
        }
    }

    /// The first symlink on `path` below the snapshot root, checked from the
    /// root down without following any link.
    fn symlink_on(&self, path: &Path) -> Option<PathBuf> {
        let mut at = self.root.to_path_buf();
        for part in path.strip_prefix(self.root).ok()?.components() {
            at.push(part);
            match fs::symlink_metadata(&at) {
                Ok(m) if m.file_type().is_symlink() => return Some(at),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    }

    /// Sorted (name, path) entries of a directory below the root; a missing
    /// directory is empty; no symlink is followed, on the way or among children.
    fn entries(&mut self, dir: &Path) -> Vec<(String, PathBuf)> {
        if let Some(link) = self.symlink_on(dir) {
            self.symlink(link);
            return Vec::new();
        }
        let rd = match fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                self.error(dir, e);
                return Vec::new();
            }
        };
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_symlink()) {
                self.symlink(e.path());
                continue;
            }
            out.push((e.file_name().to_string_lossy().into_owned(), e.path()));
        }
        out.sort();
        out
    }

    fn read_text(&mut self, path: &Path) -> Option<String> {
        match fs::read(path).map(String::from_utf8) {
            Ok(Ok(s)) => Some(s),
            Ok(Err(_)) => {
                self.error(path, "not UTF-8");
                None
            }
            Err(e) => {
                self.error(path, e);
                None
            }
        }
    }

    /// Files under `dir` as dotted names (`a/b/c` -> `a.b.c`), lowercased.
    fn dotted_files(&mut self, dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
        for (name, path) in self.entries(dir) {
            let full = if prefix.is_empty() { name } else { format!("{prefix}.{name}") };
            if path.is_dir() {
                self.dotted_files(&path, &full, out);
            } else {
                out.push((full.to_lowercase(), path));
            }
        }
    }

    fn scan(&mut self) {
        let mut files = Vec::new();
        self.dotted_files(&self.root.join("registry"), "", &mut files);
        for (name, path) in files {
            if let Some(text) = self.read_text(&path) {
                let agent = self.src.agents.entry(name).or_default();
                agent.registry = Some(text.trim().to_string());
                agent.registered = fs::symlink_metadata(&path).and_then(|m| m.modified()).ok();
            }
        }
        for (name, path) in self.entries(&self.root.join("state/registry-pin")) {
            if let Some(text) = self.read_text(&path) {
                self.src.agents.entry(name.to_lowercase()).or_default().pin = Some(text.trim().to_string());
            }
        }
        let mut files = Vec::new();
        self.dotted_files(&self.root.join("profiles"), "", &mut files);
        for (name, path) in files {
            let Some(text) = self.read_text(&path) else { continue };
            for line in text.lines() {
                let Some((key, value)) = line.split_once(':') else { continue };
                if key.trim() == "Display" && !value.trim().is_empty() {
                    self.src.agents.entry(name.clone()).or_default().display = Some(value.trim().to_string());
                } else {
                    self.src.profile_extra += 1;
                }
            }
        }
        for (name, path) in self.entries(&self.root.join("state")) {
            if name != "registry-pin" {
                let n = if path.is_dir() { self.entries(&path).len() } else { 1 };
                self.src.state_counts.push((format!("state/{name}"), n));
            }
        }

        for (agent, home) in self.entries(&self.root.join("board/agents")) {
            let agent = agent.to_lowercase();
            for (sub, loc) in [
                ("inbox/new", Loc::InboxNew),
                ("inbox/cur", Loc::InboxCur),
                ("outbox/sent", Loc::Sent),
                ("outbox/new", Loc::Pending),
            ] {
                for (file, path) in self.entries(&home.join(sub)) {
                    if loc == Loc::Sent
                        && let Some(id) = file.strip_suffix(".read")
                    {
                        if let Some(stamp) = self.read_text(&path) {
                            self.src.stamps.insert(id.to_string(), stamp.trim().to_string());
                        }
                        continue;
                    }
                    self.message(&file, &path, loc);
                }
            }
            for (file, _) in self.entries(&home.join("outbox/failed")) {
                if !file.ends_with(".reason") {
                    self.src.bounced.push(format!("{file} (from {agent})"));
                }
            }
            self.src.staging_leftovers += self.entries(&home.join("tmp")).len();
        }

        for (channel, dir) in self.entries(&self.root.join("board/channels")) {
            let channel = channel.to_lowercase();
            if !dir.join("messages").is_dir() {
                self.error(&dir, "channel without a messages directory");
                continue;
            }
            let mut members = BTreeMap::new();
            for (member, path) in self.entries(&dir.join("cursors")) {
                if let Some(cursor) = self.read_text(&path) {
                    members.insert(member.to_lowercase(), cursor.trim().to_string());
                }
            }
            self.src.channels.insert(channel, members);
            for (file, path) in self.entries(&dir.join("messages")) {
                self.message(&file, &path, Loc::Channel);
            }
            self.src.staging_leftovers += self.entries(&dir.join("tmp")).len();
        }
    }

    fn message(&mut self, id: &str, path: &Path, loc: Loc) {
        self.files += 1;
        if self.progress && self.files.is_multiple_of(PROGRESS_EVERY) {
            println!("import: scanned {} message files", self.files);
        }
        if !is_ulid(id) {
            self.error(path, "file name is not a message id");
            return;
        }
        let Some(raw) = self.read_text(path) else { return };
        let mail = MailMessage::parse(&raw);
        let digest = sha256_hex(raw.as_bytes());
        let field = |k: &str| mail.header(k).map(str::trim).filter(|v| !v.is_empty());
        let (Some(from), Some(to)) = (field("From"), field("To")) else {
            self.error(path, "missing From or To header");
            return;
        };
        let from = from.to_lowercase();
        for (key, _) in &mail.headers {
            if !KNOWN_HEADERS.contains(&key.as_str()) {
                *self.src.unknown_headers.entry(key.clone()).or_default() += 1;
            }
        }
        let reaction = field("Reaction").map(|e| (e, true)).or_else(|| field("Reaction-Remove").map(|e| (e, false)));
        if let Some((emoji, present)) = reaction {
            let Some(target) = field("In-Reply-To") else {
                self.error(path, "reaction without In-Reply-To");
                return;
            };
            self.src.reaction_events.insert(id.to_string());
            let key = (target.to_string(), from, emoji.to_string());
            let event = self.src.reactions.entry(key).or_insert_with(|| (id.to_string(), present));
            if id > event.0.as_str() {
                *event = (id.to_string(), present);
            }
            return;
        }
        if loc == Loc::InboxCur {
            self.src.inbox_cur.insert(id.to_string());
        }
        if let Some(prev) = self.src.messages.get(id) {
            if prev.digest != digest {
                self.src.divergent.push(id.to_string());
            }
            return;
        }
        let to = match to.strip_prefix('#') {
            Some(c) => To::Channel(c.to_lowercase()),
            None => To::Agent(to.to_lowercase()),
        };
        let date = match field("Date") {
            Some(d) if is_iso(d) => d.to_string(),
            _ => {
                self.src.dates_repaired += 1;
                ulid_iso(id)
            }
        };
        let attachments = mail.headers_named("Attachment").map(|h| self.attachment(h)).collect();
        self.src.messages.insert(
            id.to_string(),
            SrcMsg {
                path: path.to_path_buf(),
                digest,
                from,
                to,
                date,
                reply_to: field("In-Reply-To").map(str::to_string),
                body: mail.body,
                attachments,
                pending: loc == Loc::Pending,
            },
        );
    }

    /// The header holds the absolute path at write time; the file is looked up
    /// under this board's `board/attachments/` only.
    fn attachment(&self, header: &str) -> SrcAttachment {
        const MARK: &str = "board/attachments/";
        let filename = header.rsplit('/').next().unwrap_or_default().to_string();
        let file = match header.find(MARK).filter(|i| *i == 0 || header[..*i].ends_with('/')) {
            None => Err("not under board/attachments".to_string()),
            Some(i) => {
                let rel = Path::new(&header[i + MARK.len()..]);
                if filename.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
                    Err("unusable path".to_string())
                } else {
                    let rel = Path::new("board/attachments").join(rel);
                    attachment_size(self.root, &rel).map(|size| (rel, size))
                }
            }
        };
        SrcAttachment { filename, header: header.to_string(), file }
    }
}

/// Size of the regular file `root/rel`, reached without following a symlink
/// at any component below `root` (a snapshot must not import outside bytes).
fn attachment_size(root: &Path, rel: &Path) -> Result<u64, String> {
    let mut path = root.to_path_buf();
    let mut meta = None;
    for part in rel.components() {
        path.push(part);
        let m = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if m.file_type().is_symlink() {
            return Err(format!("rejected: {} is a symlink, not followed", path.display()));
        }
        meta = Some(m);
    }
    match meta {
        Some(m) if m.is_file() => Ok(m.len()),
        _ => Err("not a file".to_string()),
    }
}

/// The attachment's bytes, re-checked at read time: no symlink on the way and
/// the canonical path still inside the snapshot's attachments directory.
fn read_attachment(root: &Path, rel: &Path) -> Result<Vec<u8>, String> {
    attachment_size(root, rel)?;
    let base = root.join("board/attachments").canonicalize().map_err(|e| e.to_string())?;
    let real = root.join(rel).canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&base) {
        return Err(format!("rejected: {} resolves outside the snapshot", root.join(rel).display()));
    }
    fs::read(real).map_err(|e| e.to_string())
}

// --- plan ---------------------------------------------------------------------

struct Plan {
    src: Source,
    /// kond agent name -> conde name.
    names: BTreeMap<String, String>,
    /// kond channel name -> conde name.
    chans: BTreeMap<String, String>,
    /// kond channel -> kond agent recorded as its creator.
    creators: BTreeMap<String, String>,
    /// kond agent -> (effective address, pin).
    bindings: BTreeMap<String, (Option<Address>, Option<Address>)>,
    /// DM id -> read_at.
    read: BTreeMap<String, String>,
    notes: Vec<String>,
    renames: Vec<(String, String)>,
}

fn map_part(part: &str) -> String {
    let mapped: String = part
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    if mapped.is_empty() { "x".to_string() } else { mapped }
}

/// Valid names map to themselves; invalid ones get `-` for each bad
/// character and a `-N` suffix on collision.
fn map_names(names: &BTreeSet<String>, renames: &mut Vec<(String, String)>) -> BTreeMap<String, String> {
    let valid = |n: &str| n.split('.').all(|p| !p.is_empty() && map_part(p) == p);
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    for n in names.iter().filter(|n| valid(n)) {
        out.insert(n.clone(), n.clone());
        taken.insert(n.clone());
    }
    for n in names.iter().filter(|n| !valid(n)) {
        let base: String = n.split('.').map(map_part).collect::<Vec<_>>().join(".");
        let mut candidate = base.clone();
        let mut k = 1;
        while taken.contains(&candidate) {
            k += 1;
            candidate = format!("{base}-{k}");
        }
        taken.insert(candidate.clone());
        renames.push((n.clone(), candidate.clone()));
        out.insert(n.clone(), candidate);
    }
    out
}

fn kond_address(raw: &str, host: &str) -> Result<Address, String> {
    let (scheme, target) = raw.split_once(':').ok_or_else(|| format!("bad address '{raw}'"))?;
    let kind = Kind::parse(scheme).filter(|k| *k != Kind::Mailbox).ok_or_else(|| format!("bad address '{raw}'"))?;
    let target = if kind == Kind::Human { target.to_lowercase() } else { target.to_string() };
    Address::parse(&format!("{}:{target}@{host}", kind.as_str()), host).map_err(|e| e.to_string())
}

fn plan(mut src: Source, host: &str, import_time: &str) -> Plan {
    let mut notes = Vec::new();

    let lost_channels: Vec<String> = src
        .messages
        .iter()
        .filter(|(_, m)| matches!(&m.to, To::Channel(c) if !src.channels.contains_key(c)))
        .map(|(id, _)| id.clone())
        .collect();
    if !lost_channels.is_empty() {
        notes.push(format!("skipped {} message(s) to a channel the board no longer has: {}", lost_channels.len(), lost_channels.join(" ")));
    }
    let unroutable: Vec<String> = src
        .messages
        .iter()
        .filter(|(_, m)| m.pending && matches!(&m.to, To::Agent(a) if !src.agents.contains_key(a)))
        .map(|(id, _)| id.clone())
        .collect();
    if !unroutable.is_empty() {
        notes.push(format!("skipped {} queued message(s) to an unregistered name: {}", unroutable.len(), unroutable.join(" ")));
    }
    for id in lost_channels.iter().chain(&unroutable) {
        src.messages.remove(id);
    }
    let orphans: Vec<String> = src
        .reactions
        .keys()
        .filter(|(target, _, _)| !src.messages.contains_key(target))
        .map(|(t, a, e)| format!("{a} {e} on {t}"))
        .collect();
    src.reactions.retain(|(target, _, _), _| src.messages.contains_key(target));
    if !orphans.is_empty() {
        notes.push(format!("skipped {} reaction(s) on a message not on the board: {}", orphans.len(), orphans.join("; ")));
    }

    let mut names: BTreeSet<String> = src.agents.keys().cloned().collect();
    for m in src.messages.values() {
        names.insert(m.from.clone());
        if let To::Agent(a) = &m.to {
            names.insert(a.clone());
        }
    }
    names.extend(src.channels.values().flat_map(|members| members.keys().cloned()));
    names.extend(src.reactions.keys().map(|(_, actor, _)| actor.clone()));
    let mut renames = Vec::new();
    let mapped = map_names(&names, &mut renames);
    let channel_names: BTreeSet<String> = src.channels.keys().cloned().collect();
    let chans = map_names(&channel_names, &mut renames);

    let mut creators = BTreeMap::new();
    for (c, members) in &src.channels {
        let first_poster = src.messages.values().find(|m| m.to == To::Channel(c.clone())).map(|m| m.from.clone());
        match first_poster.or_else(|| members.keys().next().cloned()) {
            Some(creator) => {
                creators.insert(c.clone(), creator);
            }
            None => notes.push(format!("channel #{c} has no members and no posts; created by the owner's first agent")),
        }
    }

    let mut bindings = BTreeMap::new();
    for name in &names {
        let facts = src.agents.get(name);
        let parse = |raw: Option<&String>, what: &str, errors: &mut Vec<String>| {
            raw.and_then(|r| {
                kond_address(r, host)
                    .map_err(|e| errors.push(format!("{what} of {name}: {e}; imported unbound")))
                    .ok()
            })
        };
        let pin = parse(facts.and_then(|f| f.pin.as_ref()), "registry pin", &mut src.parse_errors);
        let registry = parse(facts.and_then(|f| f.registry.as_ref()), "registry address", &mut src.parse_errors);
        bindings.insert(name.clone(), (pin.clone().or(registry), pin));
    }

    let mut read = BTreeMap::new();
    let mut unstamped = 0;
    for (id, m) in &src.messages {
        if !matches!(m.to, To::Agent(_)) || m.pending {
            continue;
        }
        if let Some(stamp) = src.stamps.get(id) {
            read.insert(id.clone(), normalize_stamp(stamp));
        } else if src.inbox_cur.contains(id) {
            unstamped += 1;
            read.insert(id.clone(), import_time.to_string());
        }
    }
    if unstamped > 0 {
        notes.push(format!("{unstamped} DM(s) read without a .read stamp: read_at = import time"));
    }
    Plan { src, names: mapped, chans, creators, bindings, read, notes, renames }
}

// --- target -------------------------------------------------------------------

type Existing = ImportState;

/// Source rows per table; with `ex`, only those already in conde.
fn tally(plan: &Plan, ex: Option<&Existing>) -> Counts {
    let src = &plan.src;
    let name = |n: &String| plan.names[n].clone();
    let has = |f: &dyn Fn(&Existing) -> bool| ex.is_none_or(f);
    let mut c = Counts {
        agents: plan.names.values().filter(|n| has(&|e| e.agents.contains_key(*n))).count(),
        channels: plan.chans.values().filter(|n| has(&|e| e.channels.contains(*n))).count(),
        ..Counts::default()
    };
    for (id, m) in &src.messages {
        c.messages += usize::from(has(&|e| e.messages.contains(id)));
        for a in m.attachments.iter().filter(|a| a.file.is_ok()) {
            let key = (id.clone(), a.filename.clone());
            c.attachments += usize::from(has(&|e| e.attachments.contains(&key)));
        }
    }
    for (ch, members) in &src.channels {
        for member in members.keys() {
            let key = (plan.chans[ch].clone(), name(member));
            c.memberships += usize::from(has(&|e| e.memberships.contains(&key)));
        }
    }
    for id in plan.read.keys() {
        let To::Agent(rcpt) = &src.messages[id].to else { continue };
        let key = (id.clone(), name(rcpt));
        c.dm_read += usize::from(has(&|e| e.dm_read.contains(&key)));
    }
    for (target, actor, emoji) in src.reactions.keys() {
        let key = (target.clone(), name(actor), emoji.clone());
        c.reactions += usize::from(has(&|e| e.reactions.contains(&key)));
    }
    c
}

fn sub(a: Counts, b: Counts) -> Counts {
    Counts {
        agents: a.agents - b.agents,
        channels: a.channels - b.channels,
        messages: a.messages - b.messages,
        attachments: a.attachments - b.attachments,
        memberships: a.memberships - b.memberships,
        dm_read: a.dm_read - b.dm_read,
        reactions: a.reactions - b.reactions,
    }
}

fn host_owner(tx: &dyn Tx, host: &str) -> StoreResult<Option<String>> {
    Ok(tx.host_owner(host)?.map(|(_, name)| name))
}

// --- apply --------------------------------------------------------------------

fn channel_cursor_seq(tx: &dyn Tx, channel_id: &str, cursor: &str) -> StoreResult<i64> {
    if cursor.is_empty() {
        return Ok(0);
    }
    tx.channel_seq_at(channel_id, cursor)
}

/// Everything in one transaction: the board gains the whole import or none of it.
fn apply(store: &dyn Store, plan: &Plan, opts: &Options, missing: &mut Vec<String>) -> anyhow::Result<Counts> {
    let boxed = store.begin(Mode::Write)?;
    let tx = &*boxed;
    let mut added = Counts::default();
    let owner_id = tx.ensure_user(&opts.owner)?;
    tx.ensure_host(&opts.host, owner_id)?;
    match host_owner(tx, &opts.host)? {
        Some(o) if o == opts.owner => {}
        other => bail!("host '{}' belongs to user '{}', not '{}'", opts.host, other.unwrap_or_default(), opts.owner),
    }

    let mut agents: HashMap<&str, String> = HashMap::new();
    for (kond, name) in &plan.names {
        let id = match tx.agent_by_name(owner_id, name)?.map(|a| a.id) {
            Some(id) => id,
            None => {
                let (address, pin) = &plan.bindings[kond];
                let display = plan.src.agents.get(kond).and_then(|a| a.display.clone());
                let id = new_ulid();
                let pin = pin.as_ref().map(ToString::to_string);
                tx.insert_imported_agent(&ImportedAgent {
                    id: &id,
                    owner_id,
                    name,
                    display: display.as_deref(),
                    address: address.as_ref(),
                    binding_version: i64::from(address.is_some()),
                    pinned: pin.as_deref(),
                })?;
                added.agents += 1;
                id
            }
        };
        agents.insert(kond.as_str(), id);
    }
    let fallback_creator = plan.names.keys().next().map(|k| agents[k.as_str()].clone());

    let mut channels: HashMap<&str, String> = HashMap::new();
    for (kond, name) in &plan.chans {
        let existing = tx.channel_id_by_name(name)?;
        let id = match existing {
            Some(id) => id,
            None => {
                let creator = plan.creators.get(kond).map(|c| agents[c.as_str()].clone()).or(fallback_creator.clone());
                let Some(creator) = creator else { bail!("channel #{kond} has no agent to record as its creator") };
                let id = new_ulid();
                tx.insert_channel(&Channel { id: id.clone(), name: name.clone(), created_by: creator, topic: None })?;
                added.channels += 1;
                id
            }
        };
        channels.insert(kond.as_str(), id);
    }

    let total = plan.src.messages.len();
    for (done, (id, m)) in plan.src.messages.iter().enumerate() {
        if opts.progress && done > 0 && done.is_multiple_of(PROGRESS_EVERY) {
            println!("import: wrote {done} of {total} messages ({} attachments)", added.attachments);
        }
        let (recipient, channel) = match &m.to {
            To::Agent(a) => (Some(agents[a.as_str()].as_str()), None),
            To::Channel(c) => (None, Some(channels[c.as_str()].as_str())),
        };
        let inserted = tx.insert_message_if_absent(&NewMessage {
            id,
            sender_id: &agents[m.from.as_str()],
            recipient_id: recipient,
            channel_id: channel,
            text: &m.body,
            reply_to: m.reply_to.as_deref(),
            created_at: &m.date,
        })?;
        added.messages += usize::from(inserted);
        for a in &m.attachments {
            let Ok((rel, _)) = &a.file else { continue };
            // A message already imported still gets the attachments that were missing then.
            if !inserted && tx.attachment_sizes(id, &a.filename)?.is_some() {
                continue;
            }
            match read_attachment(&opts.from, rel) {
                Ok(bytes) => {
                    tx.insert_attachment(&new_ulid(), id, &a.filename, &bytes)?;
                    added.attachments += 1;
                }
                Err(e) => missing.push(format!("{id}: {} ({e}; at import time)", a.header)),
            }
        }
    }

    for (ch, members) in &plan.src.channels {
        let cid = &channels[ch.as_str()];
        for (member, cursor) in members {
            let aid = &agents[member.as_str()];
            let seq = channel_cursor_seq(tx, cid, cursor)?;
            if tx.read_seq(cid, aid)?.is_some() {
                tx.advance_cursor(cid, aid, seq)?;
            } else {
                tx.insert_membership(cid, aid, seq)?;
                added.memberships += 1;
            }
        }
    }

    for (id, read_at) in &plan.read {
        let To::Agent(rcpt) = &plan.src.messages[id].to else { continue };
        added.dm_read += usize::from(tx.mark_dm_read(id, &agents[rcpt.as_str()], read_at)?);
    }
    for ((target, actor, emoji), (_, present)) in &plan.src.reactions {
        added.reactions += usize::from(tx.insert_reaction_if_absent(target, &agents[actor.as_str()], emoji, *present)?);
    }
    boxed.commit()?;
    Ok(added)
}

// --- validation ---------------------------------------------------------------

fn validate(store: &dyn Store, plan: &Plan, opts: &Options, source: Counts) -> anyhow::Result<Validation> {
    let tx = store.begin(Mode::Read)?;
    let conn = &*tx;
    let mut v = Validation::default();
    let ex = conn.import_state(&opts.owner)?;
    let after = tally(plan, Some(&ex));
    for ((table, want), (_, got)) in source.rows().iter().zip(after.rows()) {
        if *want != got {
            v.problems.push(format!("{table}: {got} of {want} source rows are in conde"));
        }
    }
    if after == source {
        v.lines.push("counts: every source row of every table is in conde".to_string());
    }

    // Every body and every attachment by sha256: the source file re-read now against the stored bytes.
    let mut body_hashes = Vec::with_capacity(plan.src.messages.len());
    let (mut equal, mut checked, mut matched) = (0, 0, 0);
    for (id, m) in &plan.src.messages {
        let on_disk = fs::read(&m.path).ok().and_then(|raw| String::from_utf8(raw).ok()).map(|raw| MailMessage::parse(&raw).body);
        let stored = conn.message_by_id(id)?.map(|m| sha256_hex(m.text.as_bytes()));
        match (on_disk.map(|b| sha256_hex(b.as_bytes())), stored) {
            (Some(a), Some(b)) if a == b => {
                equal += 1;
                body_hashes.push((id.clone(), b));
            }
            (None, _) => v.problems.push(format!("body {id}: source file unreadable at validation")),
            (_, None) => v.problems.push(format!("body {id}: not in conde")),
            _ => v.problems.push(format!("body {id}: sha256 differs from {}", m.path.display())),
        }
        let stored_atts = conn.attachment_list(id)?;
        for a in &m.attachments {
            let Ok((rel, _)) = &a.file else { continue };
            checked += 1;
            let Ok(want) = read_attachment(&opts.from, rel).map(|b| sha256_hex(&b)) else {
                v.problems.push(format!("attachment {id}/{}: source file unreadable at validation", a.filename));
                continue;
            };
            let named: Vec<_> = stored_atts.iter().filter(|s| s.filename == a.filename).collect();
            if named.is_empty() {
                v.problems.push(format!("attachment {id}/{}: not in conde", a.filename));
                continue;
            }
            let mut equal = false;
            for meta in named {
                equal |= conn.attachment(&meta.id)?.is_some_and(|s| sha256_hex(&s.bytes) == want);
            }
            if equal {
                matched += 1;
            } else {
                v.problems.push(format!("attachment {id}/{}: sha256 differs from {}", a.filename, rel.display()));
            }
        }
    }
    v.lines.push(format!("bodies: {equal} of {} sha256-equal to the source file", plan.src.messages.len()));
    v.lines.push(format!("bodies digest: {}", rows_digest(body_hashes.iter().map(|(i, h)| (i.as_str(), h.as_str())))));
    v.lines.push(format!("attachments: {matched} of {checked} sha256-equal to the source file"));

    let uid = ex.user_id.unwrap_or(-1);
    let (mut cursors, mut cursors_ok) = (0, 0);
    for (ch, members) in &plan.src.channels {
        let cid = conn.channel_id_by_name(&plan.chans[ch])?;
        let Some(cid) = cid else { continue };
        for (member, cursor) in members {
            cursors += 1;
            let want = channel_cursor_seq(conn, &cid, cursor)?;
            let got = match conn.agent_by_name(uid, &plan.names[member])? {
                Some(a) => conn.read_seq(&cid, &a.id)?,
                None => None,
            };
            match got {
                Some(g) if g >= want => cursors_ok += 1,
                _ => v.problems.push(format!("cursor #{ch} {member}: read_seq {got:?}, expected at least {want}")),
            }
        }
    }
    v.lines.push(format!("cursors: {cursors} checked, {cursors_ok} at or past the kond cursor"));
    for name in &opts.skip_agents {
        if conn.agent_by_name(uid, name)?.is_some() {
            v.problems.push(format!("skipped agent {name} is on the board"));
        }
    }

    let notified = conn.notifications()?.iter().filter(|n| plan.src.messages.contains_key(&n.message_id)).count();
    if notified > 0 {
        v.problems.push(format!("{notified} notification(s) exist for imported messages"));
    }
    v.lines.push(format!("notifications for imported messages: {notified}"));
    Ok(v)
}

// --- entry points -------------------------------------------------------------

pub fn run(opts: &Options) -> anyhow::Result<Report> {
    let owner = match split_qualified(&format!("{}/x", opts.owner)) {
        Some((Some(o), _)) if o == opts.owner => o,
        _ => bail!("invalid owner '{}' (lowercase letters, digits, '_' and '-')", opts.owner),
    };
    Address::parse(&format!("human:x@{}", opts.host), &opts.host)
        .ok()
        .filter(|a| a.host == opts.host)
        .with_context(|| format!("invalid host id '{}'", opts.host))?;
    if !opts.from.join("board").is_dir() && !opts.from.join("registry").is_dir() {
        bail!("{} is not a kond data directory (no board/ or registry/)", opts.from.display());
    }
    if let Some(want) = opts.expect_files {
        let have = count_files(&opts.from)?;
        if have != want {
            bail!("{} holds {have} files, the snapshot {want}: not the snapshot; nothing read", opts.from.display());
        }
    }

    let mut scanner =
        Scanner { root: &opts.from, progress: opts.progress, files: 0, src: Source::default(), symlinks: HashSet::new() };
    scanner.scan();
    let skipped = (!opts.skip_agents.is_empty()).then(|| skip_agents(&mut scanner.src, &opts.skip_agents)).transpose()?;
    let plan = plan(scanner.src, &opts.host, &iso_from_ms(now_ms()));

    let label = target_label(opts);
    let (target, refused) = match inspect_target(opts)? {
        Some(store) => {
            let tx = store.begin(Mode::Read)?;
            if let Some(o) = host_owner(&*tx, &opts.host)?.filter(|o| *o != owner) {
                bail!("host '{}' belongs to user '{o}' on this board, not '{owner}'", opts.host);
            }
            let existing = tx.import_state(&owner).with_context(|| format!("reading {label}"))?;
            let refused = channels_with_native_posts(&*tx, &plan)?;
            (existing, refused)
        }
        None => (Existing::default(), Vec::new()),
    };

    let mut report = Report {
        from: opts.from.clone(),
        db: opts.db.clone(),
        target: label.clone(),
        owner: owner.clone(),
        host: opts.host.clone(),
        dry_run: opts.dry_run,
        source: tally(&plan, None),
        present: tally(&plan, Some(&target)),
        ..Report::default()
    };
    report.added = sub(report.source, report.present);
    describe(&plan, &target, opts, &mut report);
    report.refused = refused;
    report.skipped = skipped;

    if !opts.dry_run && !report.refused.is_empty() {
        report.added = Counts::default();
        report.notes.push("refused: nothing was written".to_string());
    } else if !opts.dry_run {
        let store = open_target(opts).with_context(|| format!("opening {label}"))?;
        report.added = apply(&*store, &plan, opts, &mut report.missing_attachments)?;
        if opts.progress {
            println!("import: committed; validating");
        }
        report.validation = Some(validate(&*store, &plan, opts, report.source)?);
    }
    Ok(report)
}

/// Leave the named kond agents out of `src`: their registry facts, every message they sent or
/// received (with its attachments and read state), their channel memberships and every reaction
/// by them or on a removed message. A name the snapshot does not know is an error (a typo would
/// otherwise import what the owner struck).
fn skip_agents(src: &mut Source, names: &BTreeSet<String>) -> anyhow::Result<Skipped> {
    let known: BTreeSet<&String> = src
        .agents
        .keys()
        .chain(src.messages.values().flat_map(|m| std::iter::once(&m.from).chain(match &m.to {
            To::Agent(a) => Some(a),
            To::Channel(_) => None,
        })))
        .chain(src.channels.values().flat_map(|members| members.keys()))
        .collect();
    let unknown: Vec<&String> = names.iter().filter(|n| !known.contains(n)).collect();
    if !unknown.is_empty() {
        bail!("--skip-agents names no agent of this snapshot: {}", unknown.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", "));
    }
    let mut out = Skipped { agents: names.iter().cloned().collect(), ..Skipped::default() };
    for n in names {
        src.agents.remove(n);
    }
    let gone: Vec<String> = src
        .messages
        .iter()
        .filter(|(_, m)| names.contains(&m.from) || matches!(&m.to, To::Agent(a) if names.contains(a)))
        .map(|(id, _)| id.clone())
        .collect();
    for id in &gone {
        if let Some(m) = src.messages.remove(id) {
            out.messages += 1;
            out.attachments += m.attachments.iter().filter(|a| a.file.is_ok()).count();
        }
        src.stamps.remove(id);
        src.inbox_cur.remove(id);
    }
    for members in src.channels.values_mut() {
        let before = members.len();
        members.retain(|m, _| !names.contains(m));
        out.memberships += before - members.len();
    }
    let before = src.reactions.len();
    src.reactions.retain(|(target, actor, _), _| !names.contains(actor) && src.messages.contains_key(target));
    out.reactions = before - src.reactions.len();
    Ok(out)
}

/// One kond name's activity, for the owner's prune list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentActivity {
    pub name: String,
    /// The name on the conde board when kond's is invalid there.
    pub conde_name: String,
    /// The registry's address kind (`claude`, `codex`, `human`), None when not registered.
    pub registered: Option<String>,
    pub pinned: bool,
    /// DMs sent and received plus channel posts sent.
    pub messages: usize,
    /// The latest of those messages' dates, else the registration time; None when neither.
    pub last_activity: Option<String>,
    /// Whether `last_activity` is the registration time (no message at all).
    pub from_registration: bool,
}

/// Every kond name of the snapshot with its activity, oldest last activity first.
pub fn agent_activity(root: &Path) -> anyhow::Result<Vec<AgentActivity>> {
    let src = complete_scan(root)?;
    let mut count: HashMap<String, (usize, String)> = HashMap::new();
    for m in src.messages.values() {
        let parties = std::iter::once(&m.from).chain(match &m.to {
            To::Agent(a) => Some(a),
            To::Channel(_) => None,
        });
        for p in parties {
            let e = count.entry(p.clone()).or_default();
            e.0 += 1;
            if m.date > e.1 {
                e.1 = m.date.clone();
            }
        }
    }
    let registry: Vec<(String, Option<String>, bool, Option<String>)> = src
        .agents
        .iter()
        .map(|(n, a)| {
            let kind = a.registry.as_deref().and_then(|r| r.split_once(':')).map(|(k, _)| k.to_lowercase());
            let at = a.registered.map(|t| {
                iso_from_ms(t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0))
            });
            (n.clone(), kind, a.pin.is_some(), at)
        })
        .collect();
    let plan = plan(src, "snapshot", &iso_from_ms(now_ms()));
    if !plan.src.parse_errors.is_empty() {
        bail!("{} has {} parse errors in its registry:\n  {}", root.display(), plan.src.parse_errors.len(), plan.src.parse_errors.join("\n  "));
    }
    let mut out: Vec<AgentActivity> = plan
        .names
        .iter()
        .map(|(kond, conde)| {
            let facts = registry.iter().find(|(n, ..)| n == kond);
            let (messages, last) = count.get(kond).cloned().unwrap_or_default();
            let registered_at = facts.and_then(|f| f.3.clone());
            AgentActivity {
                name: kond.clone(),
                conde_name: conde.clone(),
                registered: facts.and_then(|f| f.1.clone()),
                pinned: facts.is_some_and(|f| f.2),
                messages,
                from_registration: messages == 0 && registered_at.is_some(),
                last_activity: if messages > 0 { Some(last) } else { registered_at },
            }
        })
        .collect();
    out.sort_by(|a, b| (&a.last_activity, &a.name).cmp(&(&b.last_activity, &b.name)));
    Ok(out)
}

/// Channels the import would add posts to that already hold messages not from
/// this source. Imported history gets later commit sequences than those
/// posts, so converting a kond cursor would mark them read: refused. A channel
/// whose source posts are all present already gains no sequence and is not
/// refused (a re-run after later posts, such as the history copy of the v2 switch).
fn channels_with_native_posts(tx: &dyn Tx, plan: &Plan) -> StoreResult<Vec<String>> {
    let mut out = Vec::new();
    for (kond, name) in &plan.chans {
        let Some(channel_id) = tx.channel_id_by_name(name)? else { continue };
        let ids = tx.channel_message_ids(&channel_id)?;
        let present: HashSet<&String> = ids.iter().collect();
        let to = To::Channel(kond.clone());
        if !plan.src.messages.iter().any(|(id, m)| m.to == to && !present.contains(id)) {
            continue;
        }
        let native = ids.iter().filter(|id| !plan.src.messages.contains_key(*id)).count();
        if native > 0 {
            out.push(format!(
                "#{name}: holds {native} conde message(s) not from this source; importing would mark them read"
            ));
        }
    }
    Ok(out)
}

/// Names, one per line (the first tab- or space-separated field; `#` comments and blank lines
/// ignored), lowercased: the prune file the owner writes from the prune list.
pub fn read_name_list(path: &Path) -> anyhow::Result<BTreeSet<String>> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(raw
        .lines()
        .filter_map(|l| l.split(['\t', ' ']).next())
        .map(|n| n.trim().to_lowercase())
        .filter(|n| !n.is_empty() && !n.starts_with('#'))
        .collect())
}

/// Regular files under `root`, no symlink followed (`find root -type f | wc -l`).
pub fn count_files(root: &Path) -> anyhow::Result<usize> {
    let mut n = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let e = e?;
            let t = e.file_type()?;
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() {
                n += 1;
            }
        }
    }
    Ok(n)
}

/// sha256 over `id sha256` lines sorted by id and joined by '\n': one value for a set of rows,
/// comparable between the importer's report and the history manifest.
pub fn rows_digest<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut lines: Vec<String> = rows.into_iter().map(|(id, h)| format!("{id} {h}")).collect();
    lines.sort();
    sha256_hex(lines.join("\n").as_bytes())
}

/// A kond snapshot as the importer reads it: the messages it imports with the sha256 of each
/// body and attachment, and the conde names of its agents and channels.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub messages: BTreeMap<String, SnapshotMessage>,
    pub agents: BTreeSet<String>,
    pub channels: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub struct SnapshotMessage {
    pub body_sha256: String,
    /// (filename, sha256) of each attachment the import stores.
    pub attachments: Vec<(String, String)>,
}

/// Scan a snapshot for the history tools, which describe the whole of it: any parse error
/// refuses the read (a lost message would otherwise look absent from kond).
fn complete_scan(root: &Path) -> anyhow::Result<Source> {
    if !root.join("board").is_dir() && !root.join("registry").is_dir() {
        bail!("{} is not a kond data directory (no board/ or registry/)", root.display());
    }
    let mut scanner = Scanner { root, progress: false, files: 0, src: Source::default(), symlinks: HashSet::new() };
    scanner.scan();
    let errors = &scanner.src.parse_errors;
    if !errors.is_empty() {
        bail!("{} has {} parse errors; the snapshot is incomplete:\n  {}", root.display(), errors.len(), errors.join("\n  "));
    }
    Ok(scanner.src)
}

pub fn snapshot(root: &Path) -> anyhow::Result<Snapshot> {
    let src = complete_scan(root)?;
    let plan = plan(src, "snapshot", &iso_from_ms(now_ms()));
    if !plan.src.parse_errors.is_empty() {
        bail!("{} has {} parse errors in its registry:\n  {}", root.display(), plan.src.parse_errors.len(), plan.src.parse_errors.join("\n  "));
    }
    let mut out = Snapshot {
        agents: plan.names.values().cloned().collect(),
        channels: plan.chans.values().cloned().collect(),
        ..Snapshot::default()
    };
    for (id, m) in &plan.src.messages {
        let attachments = m
            .attachments
            .iter()
            .filter_map(|a| {
                let (rel, _) = a.file.as_ref().ok()?;
                Some((a.filename.clone(), sha256_hex(&read_attachment(root, rel).ok()?)))
            })
            .collect();
        out.messages.insert(id.clone(), SnapshotMessage { body_sha256: sha256_hex(m.body.as_bytes()), attachments });
    }
    Ok(out)
}

fn target_label(opts: &Options) -> String {
    match &opts.database_url {
        Some(_) => "the Postgres board (DATABASE_URL)".to_string(),
        None => opts.db.display().to_string(),
    }
}

/// The existing board, opened without creating, migrating or writing it;
/// None when there is none yet.
fn inspect_target(opts: &Options) -> anyhow::Result<Option<Box<dyn Store>>> {
    match &opts.database_url {
        Some(url) => {
            let store = PgStore::open(url, PgOpen::INSPECT)?;
            Ok(store.has_schema()?.then(|| Box::new(store) as Box<dyn Store>))
        }
        None if opts.db.exists() => {
            let store = SqliteStore::open_read_only(&opts.db)
                .with_context(|| format!("opening {} read-only", opts.db.display()))?;
            Ok(Some(Box::new(store)))
        }
        None => Ok(None),
    }
}

/// The board to write, created or migrated as needed.
fn open_target(opts: &Options) -> anyhow::Result<Box<dyn Store>> {
    Ok(match &opts.database_url {
        Some(url) => Box::new(PgStore::open(url, PgOpen::COMMAND)?),
        None => Box::new(SqliteStore::open(&opts.db)?),
    })
}

fn describe(plan: &Plan, target: &Existing, opts: &Options, r: &mut Report) {
    let src = &plan.src;
    let (mut dms, mut posts, mut pending) = (0, 0, 0);
    for m in src.messages.values() {
        match m.to {
            To::Agent(_) => dms += 1,
            To::Channel(_) => posts += 1,
        }
        pending += usize::from(m.pending);
    }
    r.notes.push(format!(
        "messages: {dms} DM, {posts} channel post, {pending} still queued at snapshot time (imported, not notified)"
    ));
    r.notes.push(format!(
        "reactions: {} reaction event message(s) folded into {} reaction row(s) (last event wins)",
        src.reaction_events.len(),
        src.reactions.len()
    ));
    if !src.bounced.is_empty() {
        r.notes.push(format!(
            "not imported: {} bounced message(s) in outbox/failed (never delivered): {}",
            src.bounced.len(),
            src.bounced.join(", ")
        ));
    }
    let state: Vec<String> = src.state_counts.iter().map(|(k, n)| format!("{k} ({n})")).collect();
    if !state.is_empty() {
        r.notes.push(format!(
            "not imported (runtime or UI state, no conde record): {}",
            state.join(", ")
        ));
    }
    if src.profile_extra > 0 {
        r.notes.push(format!("not imported: {} profile field(s) other than Display", src.profile_extra));
    }
    if src.staging_leftovers > 0 {
        r.notes.push(format!("not imported: {} staging leftover(s) in tmp/", src.staging_leftovers));
    }
    if src.dates_repaired > 0 {
        r.notes.push(format!("{} message(s) without a usable Date: date taken from the message id", src.dates_repaired));
    }
    if !src.divergent.is_empty() {
        r.notes.push(format!("copies differ (first copy kept): {}", src.divergent.join(" ")));
    }
    if !src.unknown_headers.is_empty() {
        let h: Vec<String> = src.unknown_headers.iter().map(|(k, n)| format!("{k} x{n}")).collect();
        r.notes.push(format!("headers with no conde field (dropped): {}", h.join(", ")));
    }
    let long: Vec<&String> = src.messages.iter().filter(|(_, m)| m.body.len() > MAX_TEXT).map(|(id, _)| id).collect();
    let big: Vec<String> = src
        .messages
        .iter()
        .filter(|(_, m)| {
            let sizes: Vec<u64> = m.attachments.iter().filter_map(|a| a.file.as_ref().ok().map(|f| f.1)).collect();
            sizes.iter().any(|s| *s > MAX_ATTACHMENT as u64) || sizes.iter().sum::<u64>() > MAX_MESSAGE_ATTACHMENTS as u64
        })
        .map(|(id, _)| id.clone())
        .collect();
    if !long.is_empty() || !big.is_empty() {
        r.notes.push(format!(
            "over conde's send limits, imported verbatim: {} long bod(ies), {} message(s) with large attachments {}",
            long.len(),
            big.len(),
            big.join(" ")
        ));
    }
    r.notes.extend(plan.notes.iter().cloned());

    for (kond, (address, pin)) in &plan.bindings {
        let name = format!("{}/{}", r.owner, plan.names[kond]);
        if let Some(pin) = pin {
            r.pins.push(format!("{name} -> {pin}"));
        }
        if let Some(a) = address {
            match a.kind {
                Kind::Claude => {
                    if let Some(live) = &opts.live_claude
                        && !live.contains(&a.target)
                    {
                        r.stale.get_or_insert_with(Vec::new).push(format!("{name} -> {a}"));
                    }
                }
                Kind::Codex => r.codex_bindings += 1,
                Kind::Human | Kind::Mailbox => {}
            }
        }
        if let Some((have, have_pin)) = target.agents.get(&plan.names[kond]) {
            let want = address.as_ref().map(ToString::to_string);
            let want_pin = pin.as_ref().map(ToString::to_string);
            if *have != want || *have_pin != want_pin {
                let show = |a: &Option<String>, p: &Option<String>| {
                    format!("{}{}", a.as_deref().unwrap_or("(unbound)"), p.as_ref().map(|p| format!(" pinned {p}")).unwrap_or_default())
                };
                r.conflicts.push(format!(
                    "{name}: conde has {}, kond has {}; conde's binding is kept",
                    show(have, have_pin),
                    show(&want, &want_pin)
                ));
            }
        }
    }
    if opts.live_claude.is_some() && r.stale.is_none() {
        r.stale = Some(Vec::new());
    }
    for (id, m) in &src.messages {
        for a in &m.attachments {
            if let Err(e) = &a.file {
                r.missing_attachments.push(format!("{id}: {} ({e})", a.header));
            }
        }
    }
    r.renames = plan.renames.clone();
    r.parse_errors = src.parse_errors.clone();
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = if self.dry_run { "dry run, nothing written" } else { "import" };
        writeln!(f, "conde import ({mode})")?;
        writeln!(f, "  from:   {}", self.from.display())?;
        writeln!(f, "  into:   {} (owner {}, host {})", self.target, self.owner, self.host)?;
        writeln!(f)?;
        let last = if self.dry_run { "to add" } else { "added" };
        writeln!(f, "{:<12} {:>8} {:>9} {:>8}", "table", "source", "in conde", last)?;
        for (((table, s), (_, p)), (_, a)) in self.source.rows().iter().zip(self.present.rows()).zip(self.added.rows()) {
            writeln!(f, "{table:<12} {s:>8} {p:>9} {a:>8}")?;
        }
        writeln!(f)?;
        writeln!(f, "notes")?;
        for n in &self.notes {
            writeln!(f, "  {n}")?;
        }
        let section = |f: &mut fmt::Formatter<'_>, title: &str, items: &[String]| -> fmt::Result {
            writeln!(f)?;
            writeln!(f, "{title} ({})", items.len())?;
            items.iter().try_for_each(|i| writeln!(f, "  {i}"))
        };
        section(f, "pins", &self.pins)?;
        match &self.stale {
            Some(stale) => section(f, "stale bindings: Claude session not live", stale)?,
            None => writeln!(f, "\nstale bindings: not checked (no Claude sessions registry on this machine)")?,
        }
        writeln!(f, "  codex bindings ({}) are not liveness-checked: a thread outlives its process", self.codex_bindings)?;
        section(f, "missing or rejected attachments (message imported without them)", &self.missing_attachments)?;
        section(f, "name conflicts with existing conde agents", &self.conflicts)?;
        let renames: Vec<String> = self.renames.iter().map(|(a, b)| format!("{a} -> {b}")).collect();
        section(f, "names invalid under [a-z0-9_-] (mapping applied)", &renames)?;
        section(f, "parse errors", &self.parse_errors)?;
        if let Some(s) = &self.skipped {
            writeln!(f)?;
            writeln!(
                f,
                "skipped by --skip-agents ({} names): {} messages, {} attachments, {} memberships, {} reactions; not imported, not validated",
                s.agents.len(),
                s.messages,
                s.attachments,
                s.memberships,
                s.reactions
            )?;
            writeln!(f, "  {}", s.agents.join(" "))?;
        }
        if !self.refused.is_empty() {
            section(f, "REFUSED: channels with native conde messages (re-runs are supported only before cutover writes)", &self.refused)?;
        }
        if let Some(v) = &self.validation {
            writeln!(f)?;
            writeln!(f, "validation")?;
            for l in &v.lines {
                writeln!(f, "  {l}")?;
            }
            for p in &v.problems {
                writeln!(f, "  PROBLEM {p}")?;
            }
        }
        writeln!(f)?;
        writeln!(f, "result: {}", if self.ok() { "OK" } else { "ISSUES (see above)" })
    }
}

/// Explicit `--owner` / `--host` are used as given, with no identity derived.
/// Defaults (the login user, this machine's host id) apply only where a login
/// user exists: without USER (a container job) a default would silently be a
/// uid or the job's hostname, so both flags are required there.
fn owner_and_host(owner: Option<&str>, host: Option<&str>, user: Option<String>) -> anyhow::Result<(String, String)> {
    match (owner, host, user) {
        (Some(o), Some(h), _) => Ok((o.to_lowercase(), h.to_lowercase())),
        (o, h, Some(user)) => Ok((
            o.map_or_else(|| user.to_lowercase(), str::to_lowercase),
            h.map_or_else(crate::util::local_host, str::to_lowercase),
        )),
        (_, _, None) => bail!(
            "USER is not set (e.g. a container job): give both --owner <owner> and --host <host id of the kond machine>"
        ),
    }
}

/// Live Claude session ids, or None (unchecked) where this machine has no
/// Claude sessions registry, as in a container job importing a snapshot.
fn live_claude_sessions() -> Option<HashSet<String>> {
    let config = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?).join(".claude"),
    };
    config
        .join("sessions")
        .is_dir()
        .then(|| conde_transport::discover_claude(Some(&config)).into_iter().map(|s| s.session_id).collect())
}

/// `conde import`: owner and host from the flags (see `owner_and_host`),
/// Claude liveness from the local sessions registry when there is one.
pub fn main(
    from: &Path,
    dry_run: bool,
    owner: Option<&str>,
    host: Option<&str>,
    expect_files: Option<usize>,
    skip_file: Option<&Path>,
) -> anyhow::Result<i32> {
    let skip_agents = match skip_file {
        Some(p) => read_name_list(p)?,
        None => BTreeSet::new(),
    };
    let (owner, host) = owner_and_host(owner, host, std::env::var("USER").ok().filter(|u| !u.is_empty()))?;
    let data_dir = crate::paths::data_dir();
    let opts = Options {
        from: from.to_path_buf(),
        db: data_dir.join("conde.db"),
        database_url: std::env::var("DATABASE_URL").ok().filter(|u| !u.is_empty()),
        owner,
        host,
        dry_run,
        live_claude: live_claude_sessions(),
        progress: true,
        expect_files,
        skip_agents,
    };
    if !dry_run && opts.database_url.is_none() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&data_dir)
            .with_context(|| format!("creating {}", data_dir.display()))?;
    }
    let report = run(&opts)?;
    print!("{report}");
    Ok(i32::from(!report.ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_and_host_need_no_login_user_when_both_are_given() {
        assert_eq!(owner_and_host(Some("KDobmayer"), Some("HomePC"), None).unwrap(), ("kdobmayer".into(), "homepc".into()));
        let err = owner_and_host(Some("kdobmayer"), None, None).unwrap_err().to_string();
        assert!(err.contains("--owner") && err.contains("--host"), "{err}");
        assert!(owner_and_host(None, None, None).is_err());
        assert_eq!(owner_and_host(None, Some("box"), Some("Me".into())).unwrap(), ("me".into(), "box".into()));
    }

    #[test]
    fn name_mapping_keeps_valid_names_and_resolves_collisions() {
        let names: BTreeSet<String> =
            ["weird name", "weird+name", "weird-name-2x", "ok.name", "a.b+c"].iter().map(|s| s.to_string()).collect();
        let mut renames = Vec::new();
        let m = map_names(&names, &mut renames);
        assert_eq!(m["ok.name"], "ok.name");
        assert_eq!(m["weird name"], "weird-name");
        assert_eq!(m["weird+name"], "weird-name-2");
        assert_eq!(m["a.b+c"], "a.b-c");
        assert_eq!(renames.len(), 3);
    }

    #[test]
    fn stamps_and_dates() {
        assert_eq!(normalize_stamp("2026-08-15T03:25:03Z"), "2026-08-15T03:25:03.000Z");
        assert_eq!(normalize_stamp("garbage"), "garbage");
        assert!(is_iso("2026-08-15T03:25:03.063Z"));
        assert!(!is_iso("2026-08-15T03:25:03Z"));
        assert_eq!(kond_address("human:Tester", "box").unwrap().to_string(), "human:tester@box");
        assert!(kond_address("nats:x", "box").is_err());
    }
}
