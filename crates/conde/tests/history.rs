//! The history tools of the v2 switch: the H1 inventory (classification by message relationships,
//! the extraction query on Postgres), the H3 copy from the purge's saved rows into a local board
//! restored by the H2 import, validated by hashes, and the Postgres COPY format it reads.

#[path = "pg/mod.rs"]
mod pg;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use conde::dispatch::{Dispatcher, Principal};
use conde::history::{self, Class, CopyOptions, Extract, XAgent, XAttachment, XMessage};
use conde::import::{self, Options};
use conde::protocol::{ErrorCode, Request};
use conde::util::{new_ulid, sha256_hex};
use serde_json::{Value, json};

const LEAD: &str = "claude:s-lead";
const WORKER: &str = "claude:s-worker";
const DESK: &str = "claude:s-desk";
const RELAY: &str = "claude:s-relay";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("conde-history-{tag}-{}", new_ulid()));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn kid(n: u32) -> String {
    format!("01K{n:023}")
}

fn put(path: &Path, content: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn mail(headers: &[(&str, &str)], body: &str) -> String {
    let mut out: String = headers.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
    out.push('\n');
    out.push_str(body);
    out
}

/// A small kond board: lead and worker (owner tester), a read DM with an attachment, and a post
/// in #team that worker has read.
fn kond_fixture(root: &Path) {
    put(&root.join("registry/lead"), "claude:s-lead\n");
    put(&root.join("registry/worker"), "claude:s-worker\n");
    let att = root.join("board/attachments").join(kid(1)).join("a.txt");
    put(&att, b"kond attachment");
    let dm = mail(
        &[("From", "lead"), ("To", "worker"), ("Date", "2025-01-01T00:00:01.000Z"), ("Attachment", att.to_str().unwrap())],
        "kond dm\n",
    );
    put(&root.join("board/agents/lead/outbox/sent").join(kid(1)), &dm);
    put(&root.join("board/agents/lead/outbox/sent").join(format!("{}.read", kid(1))), "2025-01-01T00:01:00Z\n");
    put(&root.join("board/agents/worker/inbox/cur").join(kid(1)), &dm);
    let post = mail(&[("From", "lead"), ("To", "#team"), ("Date", "2025-01-01T00:00:02.000Z")], "kond post\n");
    put(&root.join("board/agents/lead/outbox/sent").join(kid(2)), &post);
    put(&root.join("board/channels/team/messages").join(kid(2)), &post);
    put(&root.join("board/channels/team/cursors/lead"), "");
    put(&root.join("board/channels/team/cursors/worker"), kid(2));
}

fn import_opts(from: &Path, db: &Path, database_url: Option<String>) -> Options {
    Options {
        from: from.to_path_buf(),
        db: db.to_path_buf(),
        database_url,
        owner: "tester".into(),
        host: "testhost".into(),
        dry_run: false,
        live_claude: None,
        progress: false,
        expect_files: None,
        skip_agents: Default::default(),
    }
}

fn call(d: &Dispatcher, p: &Principal, actor: &str, op: &str, body: Value) -> Result<Value, (ErrorCode, String)> {
    let mut req = Request::new(new_ulid(), op, body);
    req.actor = Some(actor.to_string());
    req.idempotency_key = Some(new_ulid());
    let r = d.dispatch(p, &req);
    match r.error {
        None => Ok(r.result.unwrap()),
        Some(e) => Err((e.code, e.message)),
    }
}

fn ok(d: &Dispatcher, p: &Principal, actor: &str, op: &str, body: Value) -> Value {
    call(d, p, actor, op, body).unwrap_or_else(|e| panic!("{op} as {actor}: {e:?}"))
}

fn send(d: &Dispatcher, p: &Principal, actor: &str, to: &str, text: &str, extra: Value) -> String {
    let target = if to.starts_with('#') { json!({"channel": to}) } else { json!({"agent": to}) };
    let mut body = json!({"to": target, "text": text});
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    ok(d, p, actor, "message.send", body)["message_id"].as_str().unwrap().to_string()
}

/// Ids of the native traffic on a central board that holds the kond fixture.
struct Native {
    /// owner-internal: desk -> lead (attachment, read, reacted), lead -> desk (reply), lead in #team
    b: Vec<String>,
    /// shared: relay -> worker, worker -> relay, desk in #mixed (bob's relay a member)
    c: Vec<String>,
}

fn native_traffic(d: &Dispatcher) -> Native {
    let p = d.principal("tester", "testhost", false).unwrap();
    let bob = d.principal("bob", "bobhost", false).unwrap();
    ok(d, &p, DESK, "agent.register", json!({"name": "desk"}));
    ok(d, &bob, RELAY, "agent.register", json!({"name": "relay"}));
    let att = json!({"attachments": [{"filename": "native.bin", "data": B64.encode([0u8, 1, 2, 255])}]});
    let b1 = send(d, &p, DESK, "lead", "native internal", att);
    ok(d, &p, LEAD, "message.read", json!({"id": b1}));
    ok(d, &p, LEAD, "reaction.set", json!({"message_id": b1, "emoji": "\u{1F44D}", "present": true}));
    let b2 = send(d, &p, LEAD, "desk", "a reply", json!({"reply_to": b1}));
    let b3 = send(d, &p, LEAD, "#team", "native post in a kond channel", json!({}));
    ok(d, &p, WORKER, "message.read", json!({"channel": "#team"}));
    let c1 = send(d, &bob, RELAY, "tester/worker", "hello from bob", json!({}));
    let c2 = send(d, &p, WORKER, "bob/relay", "hello bob", json!({}));
    ok(d, &p, DESK, "channel.create", json!({"channel": "#mixed"}));
    ok(d, &bob, RELAY, "channel.join", json!({"channel": "#mixed"}));
    let c3 = send(d, &p, DESK, "#mixed", "shared post", json!({}));
    Native { b: vec![b1, b2, b3], c: vec![c1, c2, c3] }
}

// --- H1: classification ---------------------------------------------------------------------

/// The extraction's content for `owner`, read through the store (the SQL's job on Postgres).
fn extract_via_store(d: &Dispatcher, owner: &str) -> Extract {
    d.with_conn(|tx| {
        let mut x = Extract { owner: owner.to_string(), ..Extract::default() };
        let mut ids: Vec<String> = Vec::new();
        let mut channels = HashSet::new();
        for a in tx.bound_agents()?.into_iter().chain(owner_agents(tx, owner)?) {
            for m in tx.sent_by(&a.id, 0, 10_000)?.into_iter().chain(tx.inbox(&a.id, 0, 10_000)?) {
                ids.push(m.id.clone());
                channels.extend(m.channel_id.clone());
            }
        }
        for c in &channels {
            ids.extend(tx.channel_message_ids(c)?);
        }
        ids.sort();
        ids.dedup();
        let see = |tx: &dyn conde::store::Tx, id: &str, x: &mut Extract| -> Result<(), conde::store::StoreError> {
            if !x.agents.contains_key(id)
                && let Some(a) = tx.agent_by_id(id)?
            {
                let (kind, host) = a.address.map(|ad| (Some(ad.kind.as_str().to_string()), Some(ad.host))).unwrap_or_default();
                x.agents.insert(id.to_string(), XAgent { owner: a.owner, name: a.name, kind, host, pinned: false });
            }
            Ok(())
        };
        for id in &ids {
            let m = tx.message_by_id(id)?.unwrap();
            see(tx, &m.sender_id, &mut x)?;
            if let Some(r) = &m.recipient_id {
                see(tx, r, &mut x)?;
            }
            for meta in tx.attachment_list(id)? {
                let a = tx.attachment(&meta.id)?.unwrap();
                x.attachments.push(XAttachment {
                    id: meta.id,
                    message_id: id.clone(),
                    filename: a.filename,
                    size: a.bytes.len() as i64,
                    sha256: sha256_hex(&a.bytes),
                });
            }
            x.messages.insert(
                id.clone(),
                XMessage {
                    id: id.clone(),
                    seq: m.seq,
                    sender: m.sender_id,
                    recipient: m.recipient_id,
                    channel: m.channel_id,
                    reply_to: m.reply_to,
                    created_at: m.created_at,
                    sha256: sha256_hex(m.text.as_bytes()),
                    bytes: m.text.len() as i64,
                },
            );
        }
        for c in &channels {
            let mut found = None;
            for s in tx.channel_summaries()? {
                found = found.or(tx.channel_by_name(&s.name)?.filter(|ch| &ch.id == c));
            }
            let ch = found.unwrap();
            see(tx, &ch.created_by, &mut x)?;
            x.channels.insert(c.clone(), (ch.name, ch.created_by));
            for member in tx.channel_member_ids(c)? {
                see(tx, &member, &mut x)?;
                let seq = tx.read_seq(c, &member)?.unwrap_or(0);
                x.members.entry(c.clone()).or_default().insert(member, seq);
            }
        }
        x.hosts.push(("testhost".into(), true));
        x.freeze_seq = x.messages.values().map(|m| m.seq).max().unwrap_or(0);
        Ok(x)
    })
    .unwrap()
}

fn owner_agents(tx: &dyn conde::store::Tx, owner: &str) -> Result<Vec<conde::store::Agent>, conde::store::StoreError> {
    let Some(uid) = tx.user_id(owner)? else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for name in ["lead", "worker", "desk"] {
        out.extend(tx.agent_by_name(uid, name)?);
    }
    Ok(out)
}

fn assert_fixture_classes(inv: &history::Inventory, n: &Native) {
    assert_eq!(inv.class[&kid(1)], Class::Imported);
    assert_eq!(inv.class[&kid(2)], Class::Imported);
    for id in &n.b {
        assert_eq!(inv.class[id], Class::OwnerInternal, "{id}");
    }
    for id in &n.c {
        assert_eq!(inv.class[id], Class::Shared, "{id}");
    }
    assert!(inv.snapshot_missing.is_empty() && inv.snapshot_differs.is_empty());
    let name = |n: &str| inv.names.iter().find(|r| r.name == n).unwrap().clone();
    assert!(!name("lead").cross_owner());
    assert_eq!(name("worker").relation(), "later rows");
    assert!(name("worker").imported && !name("desk").imported);
    assert_eq!(name("desk").posts.keys().cloned().collect::<Vec<_>>(), vec!["#mixed".to_string()]);
    let team = inv.channels.iter().find(|c| c.name == "team").unwrap();
    assert!(team.imported && team.owner_only);
    assert_eq!((team.posts.get(&Class::Imported), team.posts.get(&Class::OwnerInternal)), (Some(&1), Some(&1)));
    let mixed = inv.channels.iter().find(|c| c.name == "mixed").unwrap();
    assert!(!mixed.imported && !mixed.owner_only);
    let md = inv.markdown("test");
    assert!(md.contains("`testhost` (host key active; 3 bound names): `desk`, `worker`"), "{md}");
    assert!(md.contains("Imported names that acquired a cross-owner relationship after the import: `worker`."), "{md}");
}

#[test]
fn inventory_classifies_by_message_relationships() {
    let tmp = TempDir::new("classify");
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    let db = tmp.0.join("central.db");
    assert!(import::run(&import_opts(&kond, &db, None)).unwrap().ok());
    let d = Dispatcher::open(&db).unwrap();
    let n = native_traffic(&d);
    let snap = import::snapshot(&kond).unwrap();
    assert_eq!(snap.messages.len(), 2);
    let inv = history::classify(extract_via_store(&d, "tester"), &snap);
    assert_fixture_classes(&inv, &n);
    assert_eq!(inv.count(Class::OwnerInternal), 3);

    // The manifest round-trips: every row with its class and hashes.
    let stem = tmp.0.join("manifest");
    history::write_manifest(&inv, &stem, "test").unwrap();
    let m = history::read_manifest(&stem).unwrap();
    assert_eq!(m.owner, "tester");
    let b: Vec<&str> = m.class(Class::OwnerInternal).iter().map(|r| r.id.as_str()).collect();
    assert_eq!(b, n.b.iter().map(String::as_str).collect::<Vec<_>>());
    let first = m.class(Class::OwnerInternal)[0];
    assert_eq!(first.attachments, vec![("native.bin".to_string(), sha256_hex(&[0, 1, 2, 255]))]);
    assert_eq!(first.sha256, sha256_hex(b"native internal"));
    assert_eq!(m.class(Class::OwnerInternal)[1].reply_to.as_deref(), Some(n.b[0].as_str()));
    assert_eq!(m.class(Class::Imported)[0].attachments[0].0, "a.txt");
}

/// The extraction query itself, read-only on Postgres: its lines parse, verify and classify as
/// the store-built view does; a dropped or altered line is detected.
#[test]
fn extraction_query_on_postgres_classifies_and_detects_lost_lines() {
    let Some(url) = pg::fresh_database() else {
        eprintln!("skipped: set CONDE_TEST_PG to run the extraction query");
        return;
    };
    let tmp = TempDir::new("extract");
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    assert!(import::run(&import_opts(&kond, &tmp.0.join("unused.db"), Some(url.clone()))).unwrap().ok());
    let d = Dispatcher::connect(&url, conde::store::postgres::PgOpen::COMMAND).unwrap();
    let n = native_traffic(&d);
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(postgres::IsolationLevel::RepeatableRead)
        .start()
        .unwrap();
    tx.batch_execute("SET LOCAL conde.owner = 'tester'").unwrap();
    let mut out = String::new();
    for msg in tx.simple_query(history::EXTRACT_SQL).unwrap() {
        if let postgres::SimpleQueryMessage::Row(row) = msg {
            out.push_str(row.get(0).unwrap());
            out.push('\n');
        }
    }
    tx.commit().unwrap();
    let x = history::parse_extract(&out).unwrap();
    // kond 2, native 6, and the join notice relay posted in #mixed
    assert_eq!((x.board_messages, x.messages.len()), (9, 9));
    let max_seq = x.messages.values().map(|m| m.seq).max().unwrap();
    assert_eq!(x.freeze_seq, max_seq, "the freeze point is the board's max commit_seq");
    assert_eq!(x.hosts, vec![("testhost".to_string(), false)]);
    let snap = import::snapshot(&kond).unwrap();
    let mut inv = history::classify(x, &snap);
    inv.x.hosts = vec![("testhost".into(), true)];
    assert_fixture_classes(&inv, &n);
    // the same classes as the store-built view
    let via_store = history::classify(extract_via_store(&d, "tester"), &snap);
    assert_eq!(inv.class, via_store.class);

    let lines: Vec<&str> = out.lines().collect();
    let dropped: String = lines.iter().skip(1).map(|l| format!("{l}\n")).collect();
    assert!(history::parse_extract(&dropped).unwrap_err().to_string().contains("lines are missing"));
    let altered = out.replacen(n.b[0].as_str(), &new_ulid(), 1);
    assert!(history::parse_extract(&altered).is_err());
    assert!(history::parse_extract(&format!("{out}{out}")).is_ok(), "a line read back twice counts once");
}

// --- H3: the copy from the purge's saved rows ----------------------------------------------

fn table_counts(db: &Path) -> Vec<i64> {
    let conn = rusqlite::Connection::open(db).unwrap();
    ["agents", "channels", "messages", "attachments", "memberships", "dm_read", "reactions"]
        .iter()
        .map(|t| conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap())
        .collect()
}

/// A field as Postgres `COPY ... (FORMAT csv)` writes it: NULL unquoted and empty, a value
/// quoted when it holds a separator, a quote, a line break or is empty.
fn csv_field(v: Option<&str>) -> String {
    match v {
        None => String::new(),
        Some(s) if s.is_empty() || s.contains([',', '"', '\n', '\r']) => format!("\"{}\"", s.replace('"', "\"\"")),
        Some(s) => s.to_string(),
    }
}

fn csv_file(dir: &Path, table: &str, header: &[&str], rows: Vec<Vec<Option<String>>>) {
    let mut out = header.join(",") + "\n";
    for r in rows {
        out.push_str(&r.iter().map(|f| csv_field(f.as_deref())).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    fs::write(dir.join(format!("{table}.csv")), out).unwrap();
}

/// What the purge saved: the agents in `gone` and the DMs `ids`, with counts.txt and
/// preview.txt in the purge's own format.
fn save_rows(d: &Dispatcher, dir: &Path, gone: &[&str], ids: &[&String]) {
    fs::create_dir_all(dir).unwrap();
    let mut counts = String::new();
    d.with_conn(|tx| {
        let uid = tx.user_id("tester")?.unwrap();
        let mut agents = Vec::new();
        for n in gone {
            let a = tx.agent_by_name(uid, n)?.unwrap();
            agents.push(vec![Some(a.id), Some(uid.to_string()), Some(a.name)]);
        }
        let (mut msgs, mut atts, mut reads, mut reacts) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for id in ids {
            let m = tx.message_by_id(id)?.unwrap();
            let rcpt = m.recipient_id.clone();
            msgs.push(vec![
                Some(m.seq.to_string()),
                Some(m.id.clone()),
                Some(m.sender_id.clone()),
                rcpt.clone(),
                m.channel_id.clone(),
                Some(m.text.clone()),
                m.reply_to.clone(),
                Some(m.created_at.clone()),
            ]);
            for meta in tx.attachment_list(id)? {
                let a = tx.attachment(&meta.id)?.unwrap();
                let hex: String = a.bytes.iter().map(|b| format!("{b:02x}")).collect();
                atts.push(vec![Some(meta.id), Some(m.id.clone()), Some(a.filename), Some(format!("\\x{hex}")), Some(a.bytes.len().to_string())]);
            }
            if let Some(r) = &rcpt
                && let Some(at) = tx.dm_read_at(id, r)?
            {
                reads.push(vec![Some(m.id.clone()), Some(r.clone()), Some(at)]);
            }
            for (emoji, by) in tx.reactions(id)? {
                let actor = tx.agent_by_name(uid, by.split_once('/').unwrap().1)?.unwrap().id;
                reacts.push(vec![Some(m.id.clone()), Some(actor), Some(emoji), Some("t".into())]);
            }
        }
        let mh = ["commit_seq", "message_id", "sender_agent_id", "recipient_agent_id", "channel_id", "text", "reply_to", "created_at"];
        for (table, n) in [("agents", agents.len()), ("messages", msgs.len()), ("attachments", atts.len()), ("reactions", reacts.len()), ("dm_read", reads.len())] {
            counts.push_str(&format!("COPY  ( SELECT x.* FROM {table} x WHERE true ) TO STDOUT WITH (FORMAT csv, HEADER)\nCOPY {n}\n"));
        }
        csv_file(dir, "agents", &["agent_id", "owner_id", "name_lower"], agents);
        csv_file(dir, "messages", &mh, msgs);
        csv_file(dir, "attachments", &["attachment_id", "message_id", "filename", "bytes", "size"], atts);
        csv_file(dir, "dm_read", &["message_id", "agent_id", "read_at"], reads);
        csv_file(dir, "reactions", &["message_id", "actor_id", "emoji", "present"], reacts);
        Ok(())
    })
    .unwrap();
    fs::write(dir.join("counts.txt"), counts).unwrap();
    let preview: String = gone.iter().map(|n| format!("gone agent | {n}\n")).collect();
    fs::write(dir.join("preview.txt"), format!("what | name_lower\n{preview}")).unwrap();
}

/// Central with the kond fixture and native traffic (desk and helper kept, a DM between them
/// retained), the manifest frozen, then one DM after the freeze; the purge saved lead and worker
/// with their DMs. Central is served, so the copy reads the retained DM back as desk.
struct Switch {
    tmp: TempDir,
    stem: PathBuf,
    csv: PathBuf,
    local: PathBuf,
    kond: PathBuf,
    n: Native,
    later: String,
    retained: String,
    central: Arc<Dispatcher>,
    url: String,
    key: String,
    _rt: tokio::runtime::Runtime,
}

const HELPER: &str = "claude:s-helper";

fn switch_with(tag: &str, prune: &[&str]) -> Switch {
    let tmp = TempDir::new(tag);
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    let db = tmp.0.join("central.db");
    assert!(import::run(&import_opts(&kond, &db, None)).unwrap().ok());
    let d = Arc::new(Dispatcher::open(&db).unwrap());
    let mut n = native_traffic(&d);
    // The kond channels were retired before the purge: no class (b) post remains to restore.
    let post = n.b.pop().unwrap();
    rusqlite::Connection::open(&db).unwrap().execute("DELETE FROM messages WHERE message_id = ?1", [&post]).unwrap();
    let p = d.principal("tester", "testhost", false).unwrap();
    ok(&d, &p, HELPER, "agent.register", json!({"name": "helper"}));
    let retained = send(&d, &p, DESK, "helper", "kept on the shared board", json!({}));
    let inv = history::classify(extract_via_store(&d, "tester"), &import::snapshot(&kond).unwrap());
    let stem = tmp.0.join("manifest");
    history::write_manifest(&inv, &stem, "test").unwrap();
    let later = send(&d, &p, DESK, "worker", "after the freeze, \"quoted\", with a comma\nand a line", json!({}));
    let csv = tmp.0.join("purge");
    save_rows(&d, &csv, &["lead", "worker"], &[&kid(1), &n.b[0], &n.b[1], &later]);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let handle = rt.block_on(conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), conde_remote::ServerConfig::default())).unwrap();
    let url = format!("http://{}", handle.local_addr());
    std::mem::forget(handle);
    let key = format!("conde_test_{}", new_ulid());
    let digest = conde_remote::token_digest(&key);
    d.with_tx(|c| conde::ops::hosts::insert_key(c, "tester", "testhost", &digest, "test", "2026-10-09T00:00:00.000Z")).unwrap();

    let local = tmp.0.join("local/conde.db");
    fs::create_dir_all(local.parent().unwrap()).unwrap();
    let skip: std::collections::BTreeSet<String> = prune.iter().map(|s| s.to_string()).collect();
    assert!(import::run(&Options { skip_agents: skip, ..import_opts(&kond, &local, None) }).unwrap().ok());
    Switch { stem, csv, local, kond, n, later, retained, central: d, url, key, _rt: rt, tmp }
}

fn switch(tag: &str) -> Switch {
    switch_with(tag, &[])
}

fn copy_opts(s: &Switch, dry_run: bool) -> CopyOptions {
    CopyOptions {
        manifest: s.stem.clone(),
        from_csv: s.csv.clone(),
        db: s.local.clone(),
        skip_agents: Default::default(),
        central: Some(history::Central { server: s.url.clone(), key: s.key.clone(), actor: DESK.into() }),
        dry_run,
    }
}

#[test]
fn copy_from_the_saved_rows_accounts_for_every_row_and_validates_by_hashes() {
    let s = switch("csv");
    let before = table_counts(&s.local);
    let dry = history::copy(&copy_opts(&s, true)).unwrap();
    assert!(dry.ok() && !dry.written, "{dry}");
    assert_eq!(table_counts(&s.local), before, "a dry run writes nothing");

    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(report.ok() && report.written, "{report}");
    assert_eq!((report.saved, report.saved_imported, report.saved_frozen, report.saved_newer), (4, 1, 2, 1), "{report}");
    assert_eq!((report.restored_frozen, report.restored_newer.clone()), (2, vec![s.later.clone()]));
    assert_eq!(report.retained_verified, vec![s.retained.clone()]);
    assert_eq!(report.kept_names, vec!["desk".to_string(), "helper".to_string()]);
    assert_eq!(report.agents_created, vec!["desk".to_string()]);
    let text = report.to_string();
    for line in [
        "saved messages: 4 = class (a) 1 (on the local board by the import 1, skipped by the prune list 0) + frozen class (b) 2 + newer than the freeze 1",
        "class (b) in the manifest: 3 = restored 2 + skipped 0 + retained on the shared board 1",
        "ids: 3 of 3 on the local board",
        "bodies: 3 of 3 sha256-equal",
        "attachments: 1 of 1",
        "reply-to: 3 of 3",
        "read state: 3 of 3",
        "reactions: 3 of 3",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }

    let d = Dispatcher::open(&s.local).unwrap();
    let p = d.principal("tester", "testhost", true).unwrap();
    let shown = ok(&d, &p, LEAD, "message.get", json!({"id": s.n.b[0]}))["message"].clone();
    assert_eq!(shown["text"], "native internal");
    assert_eq!(shown["reactions"], json!([{"emoji": "\u{1F44D}", "by": ["tester/lead"]}]));
    assert_eq!(shown["attachments"][0]["filename"], "native.bin");
    let later = ok(&d, &p, WORKER, "message.get", json!({"id": s.later}))["message"].clone();
    assert_eq!(later["text"], "after the freeze, \"quoted\", with a comma\nand a line");
    assert!(d.with_conn(|tx| tx.message_by_id(&s.retained)).unwrap().is_none(), "the retained DM stays on the shared board");
    drop(d);

    let after = table_counts(&s.local);
    let again = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!again.ok() && !again.written, "{again}");
    assert_eq!(table_counts(&s.local), after);
    let rerun = import::run(&import_opts(&s.kond, &s.local, None)).unwrap();
    assert!(rerun.ok() && rerun.refused.is_empty(), "{rerun}");
}

/// Astra's probe: a saved frozen DM missing from messages.csv is never taken for one kept on
/// the shared board, whether or not counts.txt was cut to match.
#[test]
fn review_missing_saved_frozen_dm_must_not_be_reported_as_still_on_central() {
    let s = switch("review-missing-saved");
    let path = s.csv.join("messages.csv");
    let mut records = history::read_csv(&fs::read_to_string(&path).unwrap()).unwrap();
    let header: Vec<String> = records.remove(0).into_iter().map(Option::unwrap).collect();
    let before = records.len();
    records.retain(|r| r[1].as_deref() != Some(s.n.b[1].as_str()));
    assert_eq!(records.len(), before - 1);
    csv_file(&s.csv, "messages", &header.iter().map(String::as_str).collect::<Vec<_>>(), records);
    let rows_before = table_counts(&s.local);
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.ok() && !report.written, "copy silently accepted a missing saved frozen DM and called it still on central: {report}");
    assert!(report.problems.iter().any(|p| p.contains("messages.csv holds 3 rows, counts.txt states 4")), "{report}");
    assert!(report.problems.iter().any(|p| p.contains(&s.n.b[1]) && p.contains("missing")), "{report}");

    let counts = s.csv.join("counts.txt");
    fs::write(&counts, fs::read_to_string(&counts).unwrap().replace("COPY 4\n", "COPY 3\n")).unwrap();
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains(&s.n.b[1]) && p.contains("missing")), "{report}");
    assert_eq!(table_counts(&s.local), rows_before);
}

#[test]
fn copy_refuses_unverified_retained_rows_and_an_incomplete_source() {
    let s = switch("csv-retained");
    let before = table_counts(&s.local);
    let report = history::copy(&CopyOptions { central: None, ..copy_opts(&s, false) }).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains("expected on the shared board") || p.contains("verify them")), "{report}");
    // The retained DM gone from central: not readable there.
    let conn = rusqlite::Connection::open(s.tmp.0.join("central.db")).unwrap();
    for sql in [
        "DELETE FROM attempts WHERE notification_id IN (SELECT notification_id FROM notifications WHERE message_id = ?1)",
        "DELETE FROM notifications WHERE message_id = ?1",
        "DELETE FROM messages WHERE message_id = ?1",
    ] {
        conn.execute(sql, [&s.retained]).unwrap();
    }
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains(&s.retained) && p.contains("not readable there")), "{report}");
    let _ = &s.central;
    // preview.txt naming another agent set.
    let s = switch("csv-preview");
    fs::write(s.csv.join("preview.txt"), "what | name_lower\ngone agent | lead\n").unwrap();
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains("only in agents.csv: [worker]")), "{report}");
    assert_eq!(table_counts(&s.local), before);
}

#[test]
fn the_prune_list_skips_rows_after_the_completeness_check() {
    let s = switch_with("csv-prune", &["worker"]);
    let opts = CopyOptions { skip_agents: ["worker".to_string()].into(), ..copy_opts(&s, false) };
    let report = history::copy(&opts).unwrap();
    assert!(report.ok(), "{report}");
    assert_eq!(report.skipped, vec![s.later.clone()], "the class (a) row is the import's to skip");
    assert_eq!(report.restored_frozen, 2);
    // Pruned at the copy but not at the import: the kond row of the pruned name is on the board.
    let s = switch("csv-prune-mismatch");
    let opts = CopyOptions { skip_agents: ["worker".to_string()].into(), ..copy_opts(&s, false) };
    let report = history::copy(&opts).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains(&kid(1)) && p.contains("pruned party")), "{report}");
}

#[test]
fn copy_refuses_a_hash_mismatch_and_a_channel_post_writing_nothing() {
    let s = switch("csv-refused");
    let before = table_counts(&s.local);
    let messages = s.csv.join("messages.csv");
    let pristine = fs::read_to_string(&messages).unwrap();
    fs::write(&messages, pristine.replace("native internal", "native internaL")).unwrap();
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains(&s.n.b[0]) && p.contains("sha256")), "{report}");
    assert_eq!(table_counts(&s.local), before);

    // A channel post among the saved rows: the copy restores DMs only.
    let lead = history::read_csv(&fs::read_to_string(s.csv.join("agents.csv")).unwrap()).unwrap()[1][0].clone().unwrap();
    fs::write(&messages, format!("{pristine}999,{},{lead},,{},post,,2026-10-09T00:00:00.000Z\n", new_ulid(), new_ulid())).unwrap();
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains("DMs only")), "{report}");
    assert_eq!(table_counts(&s.local), before);

    // A board the import has not restored is refused.
    fs::write(&messages, pristine).unwrap();
    let empty = s.local.with_file_name("empty.db");
    drop(Dispatcher::open(&empty).unwrap());
    let err = history::copy(&CopyOptions { db: empty, ..copy_opts(&s, false) }).unwrap_err().to_string();
    assert!(err.contains("run the H2 import"), "{err}");
}

/// Postgres's own COPY CSV (quotes, commas, line breaks, NULL against the empty string, bytea)
/// reads back exactly.
#[test]
fn postgres_copy_csv_reads_back_exactly() {
    let Some(url) = pg::fresh_database() else {
        eprintln!("skipped: set CONDE_TEST_PG to read a real COPY");
        return;
    };
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    client.batch_execute("CREATE TABLE t(a text, b text, c bytea)").unwrap();
    let tricky = "a \"quoted\", comma\nline\r\nend \u{1F44D}";
    client.execute("INSERT INTO t VALUES ($1, '', $2), (NULL, 'x', NULL)", &[&tricky, &vec![0u8, 255, 10]]).unwrap();
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut client.copy_out("COPY t TO STDOUT WITH (FORMAT csv, HEADER)").unwrap(), &mut out).unwrap();
    let rows = history::read_csv(&String::from_utf8(out).unwrap()).unwrap();
    assert_eq!(rows[0], vec![Some("a".into()), Some("b".into()), Some("c".into())]);
    assert_eq!(rows[1], vec![Some(tricky.to_string()), Some(String::new()), Some("\\x00ff0a".into())]);
    assert_eq!(rows[2], vec![None, Some("x".into()), None]);
}

// --- an incomplete snapshot -----------------------------------------------------------------

/// Both copies of one kond DM unreadable: the snapshot read is incomplete, so no manifest and no
/// prune list come out of it (a lost imported row would otherwise classify as conde-native).
#[test]
fn inventory_and_prune_list_refuse_a_snapshot_with_parse_errors() {
    let tmp = TempDir::new("corrupt");
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    let copies = [
        kond.join("board/agents/lead/outbox/sent").join(kid(1)),
        kond.join("board/agents/worker/inbox/cur").join(kid(1)),
    ];
    for c in &copies {
        fs::write(c, [0xffu8, 0xfe, b'\n']).unwrap();
    }
    let err = format!("{:#}", import::snapshot(&kond).unwrap_err());
    assert!(err.contains("2 parse errors") && copies.iter().all(|c| err.contains(c.to_str().unwrap())), "{err}");
    let err = format!("{:#}", import::agent_activity(&kond).unwrap_err());
    assert!(err.contains("2 parse errors"), "{err}");

    let extract = tmp.0.join("extract.txt");
    fs::write(&extract, "").unwrap();
    let stem = tmp.0.join("manifest");
    let run = |args: &[&std::ffi::OsStr]| std::process::Command::new(env!("CARGO_BIN_EXE_conde")).args(args).output().unwrap();
    let out = run(&["history".as_ref(), "inventory".as_ref(), "--extract".as_ref(), extract.as_os_str(), "--snapshot".as_ref(), kond.as_os_str(), "--out".as_ref(), stem.as_os_str()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains("2 parse errors"), "{stderr}");
    assert!(history::manifest_paths(&stem).iter().all(|p| !p.exists()), "no manifest file written");
    let list = tmp.0.join("prune.tsv");
    let out = run(&["history".as_ref(), "prune-candidates".as_ref(), "--snapshot".as_ref(), kond.as_os_str(), "--out".as_ref(), list.as_os_str()]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("2 parse errors"));
    assert!(!list.exists(), "no prune list written");
}

/// Astra's probe: the last ten bytes of messages.csv cut off, counts.txt and preview.txt intact.
/// The row count still matches, so only a strict read of every record refuses it.
#[test]
fn review_truncated_newer_metadata_must_be_refused() {
    let s = switch("truncated-newer-metadata");
    let p = s.csv.join("messages.csv");
    let raw = fs::read_to_string(&p).unwrap();
    let before_rows = history::read_csv(&raw).unwrap();
    assert_eq!(before_rows.last().unwrap()[1].as_deref(), Some(s.later.as_str()));
    fs::write(&p, &raw[..raw.len() - 10]).unwrap();
    let before = table_counts(&s.local);
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.ok() && !report.written, "truncated post-freeze metadata silently accepted: {report}");
    assert!(report.problems.iter().any(|p| p.contains("messages.csv") && p.contains("line break")), "{report}");
    assert_eq!(table_counts(&s.local), before);
    // A complete last record with a malformed field is refused as well.
    fs::write(&p, raw.replace(".000Z\n", "Z\n").replacen(&s.later, "not-a-ulid", 1)).unwrap();
    let report = history::copy(&copy_opts(&s, false)).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains("not-a-ulid")), "{report}");
}

/// prune-candidates reads the registry and pins too: an address it cannot parse refuses the list.
#[test]
fn prune_list_refuses_a_registry_parse_error() {
    let tmp = TempDir::new("prune-registry");
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    fs::write(kond.join("registry/lead"), "not-an-address\n").unwrap();
    let err = format!("{:#}", import::agent_activity(&kond).unwrap_err());
    assert!(err.contains("parse error") && err.contains("not-an-address"), "{err}");
}

/// A pruned name's class (a) rows among the saved rows are reported as skipped by the prune list.
#[test]
fn saved_class_a_rows_of_a_pruned_name_are_reported_as_skipped() {
    let s = switch_with("csv-prune-label", &["worker"]);
    let opts = CopyOptions { skip_agents: ["worker".to_string()].into(), ..copy_opts(&s, false) };
    let report = history::copy(&opts).unwrap();
    assert!(report.ok(), "{report}");
    assert_eq!((report.saved_imported, report.skipped_imported), (1, 1), "{report}");
    let text = report.to_string();
    assert!(text.contains("class (a) 1 (on the local board by the import 0, skipped by the prune list 1)"), "{text}");
}
