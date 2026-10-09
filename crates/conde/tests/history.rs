//! The history tools of the v2 switch: the owner-only `history.copy` read, the H1 inventory
//! (classification by message relationships, the extraction query on Postgres) and the H3 copy
//! into a local board restored by the H2 import, validated by hashes.

#[path = "pg/mod.rs"]
mod pg;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use conde::dispatch::{Dispatcher, Principal};
use conde::history::{self, Class, CopyOptions, Extract, XAgent, XAttachment, XMessage};
use conde::import::{self, Options};
use conde::protocol::{ErrorCode, Request};
use conde::store::NewMessage;
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

// --- history.copy ---------------------------------------------------------------------------

#[test]
fn history_copy_returns_owner_internal_rows_and_refuses_everything_else() {
    let tmp = TempDir::new("op");
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    let url = pg::fresh_database();
    let report = import::run(&import_opts(&kond, &tmp.0.join("central.db"), url.clone())).unwrap();
    assert!(report.ok(), "{report}");
    let d = match &url {
        Some(u) => Dispatcher::connect(u, conde::store::postgres::PgOpen::COMMAND).unwrap(),
        None => Dispatcher::open(&tmp.0.join("central.db")).unwrap(),
    };
    let n = native_traffic(&d);
    let p = d.principal("tester", "testhost", false).unwrap();
    let copy = |p: &Principal, ids: &[&String]| call(&d, p, LEAD, "history.copy", json!({"ids": ids}));

    let rows = copy(&p, &[&n.b[0], &n.b[2]]).unwrap()["messages"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2);
    let dm = &rows[0];
    assert_eq!((dm["from"].as_str(), dm["to"].as_str()), (Some("tester/desk"), Some("tester/lead")));
    assert_eq!(dm["sha256"], sha256_hex(b"native internal"));
    assert_eq!(dm["attachments"][0]["filename"], "native.bin");
    assert_eq!(B64.decode(dm["attachments"][0]["data"].as_str().unwrap()).unwrap(), vec![0u8, 1, 2, 255]);
    assert_eq!(dm["attachments"][0]["sha256"], sha256_hex(&[0, 1, 2, 255]));
    assert!(dm["read"]["read_at"].as_str().is_some_and(|r| r.starts_with("20")), "{dm}");
    assert_eq!(dm["reactions"], json!([{"emoji": "\u{1F44D}", "by": "tester/lead"}]));
    let post = &rows[1];
    assert_eq!(post["channel"], "team");
    let members: BTreeMap<String, bool> = post["read"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["name"].as_str().unwrap().to_string(), m["read"].as_bool().unwrap()))
        .collect();
    // the board's own state: a sender's cursor does not move past its own post
    assert_eq!(members, BTreeMap::from([("tester/lead".into(), false), ("tester/worker".into(), true)]));

    // A kond (a) id is owner-internal too: the server cannot tell; the copy tool refuses it.
    assert!(copy(&p, &[&kid(1)]).is_ok());
    for shared in &n.c {
        let (code, message) = copy(&p, &[&n.b[0], shared]).unwrap_err();
        assert_eq!(code, ErrorCode::Forbidden);
        assert!(message.contains(shared.as_str()) && !message.contains(n.b[0].as_str()), "{message}");
    }
    let bob = d.principal("bob", "bobhost", false).unwrap();
    assert_eq!(call(&d, &bob, RELAY, "history.copy", json!({"ids": [n.b[0]]})).unwrap_err().0, ErrorCode::Forbidden);
    assert_eq!(copy(&p, &[&new_ulid()]).unwrap_err().0, ErrorCode::NotFound);
    assert_eq!(copy(&p, &[]).unwrap_err().0, ErrorCode::BadRequest);
    let bridge = d.bridge_principal(&p);
    assert_eq!(call(&d, &bridge, LEAD, "history.copy", json!({"ids": [n.b[0]]})).unwrap_err().0, ErrorCode::Forbidden);
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

// --- H3: the copy ---------------------------------------------------------------------------

struct Central {
    _rt: tokio::runtime::Runtime,
    d: Arc<Dispatcher>,
    url: String,
    key: String,
}

fn central(tmp: &Path, kond: &Path) -> Central {
    let url = pg::fresh_database();
    let db = tmp.join("central.db");
    assert!(import::run(&import_opts(kond, &db, url.clone())).unwrap().ok());
    let d = Arc::new(match &url {
        Some(u) => Dispatcher::connect(u, conde::store::postgres::PgOpen::SERVER).unwrap(),
        None => Dispatcher::open(&db).unwrap(),
    });
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let config = conde_remote::ServerConfig::default();
    let handle = rt.block_on(conde::cloud::start(d.clone(), "127.0.0.1:0".parse().unwrap(), config)).unwrap();
    let addr = handle.local_addr();
    std::mem::forget(handle);
    let key = format!("conde_test_{}", new_ulid());
    let digest = conde_remote::token_digest(&key);
    d.with_tx(|c| conde::ops::hosts::insert_key(c, "tester", "testhost", &digest, "test", "2026-10-09T00:00:00.000Z"))
        .unwrap();
    Central { _rt: rt, d, url: format!("http://{addr}"), key }
}

fn table_counts(db: &Path) -> Vec<i64> {
    let conn = rusqlite::Connection::open(db).unwrap();
    ["agents", "channels", "messages", "attachments", "memberships", "dm_read", "reactions"]
        .iter()
        .map(|t| conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap())
        .collect()
}

#[test]
fn copy_restores_class_b_after_the_import_validated_by_hashes() {
    let tmp = TempDir::new("copy");
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    let c = central(&tmp.0, &kond);
    let n = native_traffic(&c.d);
    let inv = history::classify(extract_via_store(&c.d, "tester"), &import::snapshot(&kond).unwrap());
    let stem = tmp.0.join("manifest");
    history::write_manifest(&inv, &stem, "test").unwrap();

    // H2: the kond snapshot into a fresh local board.
    let local = tmp.0.join("local/conde.db");
    fs::create_dir_all(local.parent().unwrap()).unwrap();
    let h2 = import::run(&import_opts(&kond, &local, None)).unwrap();
    assert!(h2.ok(), "{h2}");
    let opts = |dry_run| CopyOptions { manifest: stem.clone(), db: local.clone(), server: c.url.clone(), key: c.key.clone(), dry_run };

    let before = table_counts(&local);
    let dry = history::copy(&opts(true)).unwrap();
    assert!(dry.ok() && !dry.written, "{dry}");
    assert_eq!(table_counts(&local), before, "a dry run writes nothing");

    let report = history::copy(&opts(false)).unwrap();
    assert!(report.ok(), "{report}");
    assert!(report.written);
    assert_eq!(report.agents_created, vec!["desk".to_string()]);
    let text = report.to_string();
    for line in [
        "ids: 3 of 3 frozen class (b) ids",
        "bodies: 3 of 3 sha256-equal",
        "attachments: 1 of 1 sha256-equal",
        "reply-to: 3 of 3 resolve",
        "read state: 3 of 3 equal",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    assert!(text.contains(&format!("bodies digest: {}", inv.class_digest(Class::OwnerInternal))), "{text}");

    // Through the local dispatcher: order, read state, reactions, cursors.
    let d = Dispatcher::open(&local).unwrap();
    let p = d.principal("tester", "testhost", true).unwrap();
    let shown = ok(&d, &p, LEAD, "message.get", json!({"id": n.b[0]}))["message"].clone();
    assert_eq!(shown["text"], "native internal");
    assert_eq!(shown["reactions"], json!([{"emoji": "\u{1F44D}", "by": ["tester/lead"]}]));
    let seq = |id: &str| d.with_conn(|tx| tx.message_by_id(id)).unwrap().unwrap().seq;
    assert!(seq(&n.b[0]) > seq(&kid(2)) && seq(&n.b[2]) > seq(&n.b[0]), "appended after the kond import, in board order");
    let unread = ok(&d, &p, WORKER, "inbox.list", json!({}));
    assert_eq!(unread["items"], json!([]), "worker read the native #team post on central: read here too");
    for id in &n.c {
        assert!(d.with_conn(|tx| tx.message_by_id(id)).unwrap().is_none(), "class (c) stays on central");
    }
    drop(d);

    // Run again: refused before any fetch, nothing written.
    let after = table_counts(&local);
    let again = history::copy(&opts(false)).unwrap();
    assert!(!again.ok() && !again.written, "{again}");
    assert_eq!(table_counts(&local), after);

    // The importer re-run over the restored board: the copied #team post does not block it.
    let rerun = import::run(&import_opts(&kond, &local, None)).unwrap();
    assert!(rerun.ok() && rerun.refused.is_empty(), "{rerun}");
    assert_eq!(table_counts(&local), after);
}

#[test]
fn copy_refuses_a_hash_mismatch_a_shared_id_and_an_order_inversion_writing_nothing() {
    let tmp = TempDir::new("copy-refused");
    let kond = tmp.0.join("kond");
    kond_fixture(&kond);
    let c = central(&tmp.0, &kond);
    let n = native_traffic(&c.d);
    // A native row between lead and worker dated before the kond DM of that conversation.
    let old = new_ulid();
    let p = c.d.principal("tester", "testhost", false).unwrap();
    c.d.with_tx(|tx| {
        let lead = tx.agent_by_name(p.user_id, "lead")?.unwrap().id;
        let worker = tx.agent_by_name(p.user_id, "worker")?.unwrap().id;
        tx.insert_message(&NewMessage {
            id: &old,
            sender_id: &lead,
            recipient_id: Some(&worker),
            channel_id: None,
            text: "dated before kond",
            reply_to: None,
            created_at: "2024-12-31T00:00:00.000Z",
        })?;
        Ok(())
    })
    .unwrap();
    let inv = history::classify(extract_via_store(&c.d, "tester"), &import::snapshot(&kond).unwrap());
    assert_eq!(inv.class[&old], Class::OwnerInternal);
    let stem = tmp.0.join("manifest");
    history::write_manifest(&inv, &stem, "test").unwrap();
    let [_, messages, _, _] = history::manifest_paths(&stem);
    let pristine = fs::read_to_string(&messages).unwrap();

    let local = tmp.0.join("conde.db");
    assert!(import::run(&import_opts(&kond, &local, None)).unwrap().ok());
    let before = table_counts(&local);
    let opts = CopyOptions { manifest: stem.clone(), db: local.clone(), server: c.url.clone(), key: c.key.clone(), dry_run: false };

    let report = history::copy(&opts).unwrap();
    assert!(!report.ok() && !report.written, "{report}");
    assert!(
        report.problems.iter().any(|p| p.contains(&old) && p.contains("older than") && p.contains(&kid(1))),
        "{:?}",
        report.problems
    );
    assert_eq!(table_counts(&local), before);

    // Without the inverted row: a body hash in the manifest that the board does not have.
    let without: String = pristine.lines().filter(|l| !l.contains(&old)).map(|l| format!("{l}\n")).collect();
    let sha = sha256_hex(b"native internal");
    fs::write(&messages, without.replace(&sha, &sha256_hex(b"something else"))).unwrap();
    let report = history::copy(&opts).unwrap();
    assert!(!report.written && report.problems.iter().any(|p| p.contains(&n.b[0]) && p.contains("sha256")), "{report}");
    assert_eq!(table_counts(&local), before);

    // A shared id listed as class (b): the board refuses the read, nothing is written.
    fs::write(&messages, without.replace(&format!("c\t{}", n.c[0]), &format!("b\t{}", n.c[0]))).unwrap();
    let err = history::copy(&opts).unwrap_err().to_string();
    assert!(err.contains("not owner-internal") && err.contains(&n.c[0]), "{err}");
    assert_eq!(table_counts(&local), before);

    // The kond id listed as class (b): refused locally, before any fetch.
    fs::write(&messages, without.replace(&format!("a\t{}", kid(1)), &format!("b\t{}", kid(1)))).unwrap();
    let report = history::copy(&opts).unwrap();
    assert!(report.problems.iter().any(|p| p.contains(&kid(1)) && p.contains("already on the local board")), "{report}");
    assert_eq!(table_counts(&local), before);

    // A local board that the import has not restored is refused.
    let empty = tmp.0.join("empty.db");
    drop(Dispatcher::open(&empty).unwrap());
    let err = history::copy(&CopyOptions { db: empty, ..opts }).unwrap_err().to_string();
    assert!(err.contains("run the H2 import"), "{err}");
}
