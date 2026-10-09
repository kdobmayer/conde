//! The store suite: the same cases against SQLite (in memory) and against
//! Postgres 18 on a throwaway local cluster (initdb + pg_ctl on a temp dir,
//! unix socket only, stopped and deleted at the end), plus Postgres-only
//! cases for migrations, cross-replica wake-ups and the listener.
//!
//! Custom harness so the cluster has one owner and is torn down after the
//! last case. `cargo test --test store -- <substring>` filters cases.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use conde::address::{Address, Kind};
use conde::store::postgres::{MIGRATIONS, PgStore, migrate};
use conde::store::sqlite::SqliteStore;
use conde::store::{Channel, Mode, NewMessage, NewNotification, SearchScope, Store, Tx, UserRow, Wake, WebSessionRow};

type Case = (&'static str, fn(&dyn Store));

const CASES: &[Case] = &[
    ("principals", principals),
    ("credentials", credentials),
    ("idempotency", idempotency),
    ("agents_and_byte_order", agents_and_byte_order),
    ("messages_and_attachments", messages_and_attachments),
    ("rollback_leaves_nothing", rollback_leaves_nothing),
    ("failed_statement_then_reuse", failed_statement_then_reuse),
    ("failed_statement_refuses_commit", failed_statement_refuses_commit),
    ("channels", channels),
    ("inbox_unread_sent_search", inbox_unread_sent_search),
    ("reactions_and_read_state", reactions_and_read_state),
    ("notifications_and_attempts", notifications_and_attempts),
    ("diagnostics", diagnostics),
    ("fencing_and_endpoints", fencing_and_endpoints),
    ("provisional_acceptance_goes_stale", provisional_acceptance_goes_stale),
    ("import_helpers", import_helpers),
    ("channel_topics", channel_topics),
    ("writes_are_serialized", writes_are_serialized),
    ("pending_wakes_on_commit_only", pending_wakes_on_commit_only),
    ("web_sign_in_and_disabled_users", web_sign_in_and_disabled_users),
];

type PgCase = (&'static str, fn(&Cluster));

const PG_CASES: &[PgCase] = &[
    ("concurrent_startup_migrates_once", concurrent_startup_migrates_once),
    ("edited_migration_is_refused", edited_migration_is_refused),
    ("wake_crosses_replicas", wake_crosses_replicas),
    ("listener_survives_connection_loss", listener_survives_connection_loss),
    ("read_transactions_are_read_only", read_transactions_are_read_only),
    ("stalled_writer_releases_the_board", stalled_writer_releases_the_board),
    ("phase_one_database_gains_topics", phase_one_database_gains_topics),
    ("missed_fence_closes_the_old_connection", missed_fence_closes_the_old_connection),
    ("dropping_a_board_on_an_async_worker", dropping_a_board_on_an_async_worker),
    ("replicas_share_idempotency_and_claims", replicas_share_idempotency_and_claims),
    ("deliveries_from_before_migration_5_still_count_as_reached", deliveries_from_before_migration_5_still_count_as_reached),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let filter = args.iter().find(|a| !a.starts_with('-')).cloned();
    if args.iter().any(|a| a == "--list") {
        for (name, _) in CASES {
            println!("sqlite::{name}: test\npostgres::{name}: test");
        }
        for (name, _) in PG_CASES {
            println!("postgres::{name}: test");
        }
        return;
    }
    let wanted = |name: &str| filter.as_deref().is_none_or(|f| name.contains(f));
    let mut report = Report::default();

    for (name, case) in CASES {
        let full = format!("sqlite::{name}");
        if wanted(&full) {
            report.run(&full, || case(&SqliteStore::open_in_memory().unwrap()));
        }
    }
    if wanted("sqlite::old_board_file_gains_topics") {
        report.run("sqlite::old_board_file_gains_topics", old_board_file_gains_topics);
    }
    if wanted("sqlite::deliveries_without_a_settled_time_still_count_as_reached") {
        report.run(
            "sqlite::deliveries_without_a_settled_time_still_count_as_reached",
            deliveries_without_a_settled_time_still_count_as_reached_sqlite,
        );
    }

    let pg_wanted =
        CASES.iter().map(|c| c.0).chain(PG_CASES.iter().map(|c| c.0)).any(|n| wanted(&format!("postgres::{n}")));
    if pg_wanted {
        match Cluster::start() {
            Err(Skip(why)) => {
                println!("postgres: SKIPPED, {why}");
                report.skipped = true;
            }
            Ok(cluster) => {
                for (name, case) in CASES {
                    let full = format!("postgres::{name}");
                    if wanted(&full) {
                        report.run(&full, || case(&cluster.store()));
                    }
                }
                for (name, case) in PG_CASES {
                    let full = format!("postgres::{name}");
                    if wanted(&full) {
                        report.run(&full, || case(&cluster));
                    }
                }
            }
        }
    }
    report.finish();
}

#[derive(Default)]
struct Report {
    passed: usize,
    failed: Vec<String>,
    skipped: bool,
}

impl Report {
    fn run(&mut self, name: &str, f: impl FnOnce()) {
        let start = Instant::now();
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(()) => {
                println!("test {name} ... ok ({} ms)", start.elapsed().as_millis());
                self.passed += 1;
            }
            Err(_) => {
                println!("test {name} ... FAILED");
                self.failed.push(name.to_string());
            }
        }
    }

    fn finish(self) {
        let skipped = if self.skipped { "; postgres skipped" } else { "" };
        if self.failed.is_empty() {
            println!("\nstore: ok. {} passed; 0 failed{skipped}\n", self.passed);
        } else {
            println!(
                "\nstore: FAILED. {} passed; {} failed: {}{skipped}\n",
                self.passed,
                self.failed.len(),
                self.failed.join(", ")
            );
            std::process::exit(1);
        }
    }
}

// --- throwaway Postgres cluster ---------------------------------------------------

struct Skip(String);

struct Cluster {
    dir: tempfile::TempDir,
    socket: tempfile::TempDir,
    next_db: std::sync::atomic::AtomicUsize,
}

fn tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?}: {}\n{}", out.status, String::from_utf8_lossy(&out.stderr)))
    }
}

impl Cluster {
    fn start() -> Result<Cluster, Skip> {
        let (Some(initdb), Some(pg_ctl)) = (tool("initdb"), tool("pg_ctl")) else {
            return Err(Skip(
                "initdb / pg_ctl not on PATH (install PostgreSQL 18 to test the Postgres backend)".into(),
            ));
        };
        // Data under cargo's integration-test scratch dir (on disk, not a small tmpfs);
        // the socket dir in the system temp dir, since socket paths must stay short.
        let dir = tempfile::Builder::new()
            .prefix("conde-pg-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("temp dir for the cluster");
        let socket = tempfile::Builder::new().prefix("conde-pg-sock-").tempdir().expect("temp dir for the socket");
        let data = dir.path().join("data");
        // A non-C default collation, as on Azure, so byte-order assumptions are tested.
        let init = |locale: &str| {
            run(Command::new(&initdb)
                .arg("-D")
                .arg(&data)
                .args(["-U", "conde", "-A", "trust", "-E", "UTF8", "--no-sync"])
                .arg(format!("--locale={locale}")))
        };
        if let Err(e) = init("en_US.UTF-8") {
            eprintln!("initdb with en_US.UTF-8 failed, retrying with C.UTF-8: {e}");
            let _ = std::fs::remove_dir_all(&data);
            init("C.UTF-8").expect("initdb");
        }
        let options = format!("-k {} -c listen_addresses='' -c fsync=off -c max_connections=200", socket.path().display());
        run(Command::new(&pg_ctl)
            .arg("-D")
            .arg(&data)
            .arg("-l")
            .arg(dir.path().join("log"))
            .args(["-w", "-o", &options, "start"]))
        .expect("pg_ctl start");
        let cluster = Cluster { dir, socket, next_db: 0.into() };
        let version = cluster.admin().query_one("SHOW server_version", &[]).unwrap().get::<_, String>(0);
        println!("postgres: throwaway cluster {version} at {}", cluster.dir.path().display());
        Ok(cluster)
    }

    fn url(&self, db: &str) -> String {
        format!("host={} user=conde dbname={db} sslmode=disable", self.socket.path().display())
    }

    fn admin(&self) -> postgres::Client {
        postgres::Client::connect(&self.url("postgres"), postgres::NoTls).unwrap()
    }

    fn client(&self, db: &str) -> postgres::Client {
        postgres::Client::connect(&self.url(db), postgres::NoTls).unwrap()
    }

    /// A fresh, empty database.
    fn database(&self) -> String {
        let n = self.next_db.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("case_{n}");
        self.admin().batch_execute(&format!("CREATE DATABASE {name}")).unwrap();
        name
    }

    /// A store on a fresh database.
    fn store(&self) -> PgStore {
        PgStore::connect(&self.url(&self.database())).unwrap()
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let data: &Path = &self.dir.path().join("data");
        if let Some(pg_ctl) = tool("pg_ctl")
            && let Err(e) = run(Command::new(pg_ctl).arg("-D").arg(data).args(["-m", "immediate", "-w", "stop"]))
        {
            eprintln!("stopping the throwaway cluster: {e}");
        }
    }
}

// --- helpers -----------------------------------------------------------------------

fn write<T>(s: &dyn Store, f: impl FnOnce(&dyn Tx) -> T) -> T {
    let tx = s.begin(Mode::Write).unwrap();
    let out = f(&*tx);
    tx.commit().unwrap();
    out
}

fn read<T>(s: &dyn Store, f: impl FnOnce(&dyn Tx) -> T) -> T {
    let tx = s.begin(Mode::Read).unwrap();
    let out = f(&*tx);
    tx.commit().unwrap();
    out
}

fn addr(kind: Kind, target: &str, host: &str) -> Address {
    Address::new(kind, target, host)
}

/// krisz on "box" with a (claude), b (codex), human krisz; ana on "far" with c (claude).
struct World {
    krisz: i64,
    ana: i64,
    a: String,
    b: String,
    h: String,
    c: String,
}

fn world(s: &dyn Store) -> World {
    write(s, |tx| {
        let krisz = tx.ensure_user("krisz").unwrap();
        tx.ensure_host("box", krisz).unwrap();
        let ana = tx.ensure_user("ana").unwrap();
        tx.ensure_host("far", ana).unwrap();
        let bind = |owner: i64, id: &str, name: &str, a: Address| {
            tx.insert_agent(id, owner, name).unwrap();
            tx.set_binding(id, Some(&a)).unwrap();
            id.to_string()
        };
        World {
            krisz,
            ana,
            a: bind(krisz, "01AGENTA00000000000000000A", "a", addr(Kind::Claude, "s1", "box")),
            b: bind(krisz, "01AGENTB00000000000000000B", "b", addr(Kind::Codex, "t1", "box")),
            h: bind(krisz, "01AGENTH00000000000000000H", "krisz", addr(Kind::Human, "krisz", "box")),
            c: bind(ana, "01AGENTC00000000000000000C", "c", addr(Kind::Claude, "s9", "far")),
        }
    })
}

/// Message ids sort in insertion order, like ULIDs.
fn mid(n: usize) -> String {
    format!("01MSG{n:021}")
}

fn dm(tx: &dyn Tx, n: usize, from: &str, to: &str, text: &str) -> i64 {
    tx.insert_message(&NewMessage {
        id: &mid(n),
        sender_id: from,
        recipient_id: Some(to),
        channel_id: None,
        text,
        reply_to: None,
        created_at: "2026-10-09T00:00:00.000Z",
    })
    .unwrap()
}

fn post(tx: &dyn Tx, n: usize, from: &str, channel: &str, text: &str) -> i64 {
    tx.insert_message(&NewMessage {
        id: &mid(n),
        sender_id: from,
        recipient_id: None,
        channel_id: Some(channel),
        text,
        reply_to: Some("01REPLY0000000000000000000"),
        created_at: "2026-10-09T00:00:01.000Z",
    })
    .unwrap()
}

fn notification<'a>(id: &'a str, message_id: &'a str, recipient_id: &'a str, at: i64) -> NewNotification<'a> {
    NewNotification { id, message_id, recipient_id, kind: "dm", binding_version: 1, next_attempt_at: at }
}

fn chan(id: &str, name: &str, created_by: &str) -> Channel {
    Channel { id: id.into(), name: name.into(), created_by: created_by.into(), topic: None }
}

fn ids(msgs: &[conde::store::Message]) -> Vec<String> {
    msgs.iter().map(|m| m.id.clone()).collect()
}

/// Consumes wakes until `host` has been quiet for `quiet`.
fn settle(rt: &tokio::runtime::Runtime, watch: &mut conde::store::PendingWatch, host: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while rt.block_on(watch.wait(host, Duration::from_millis(400))) {
        assert!(Instant::now() < deadline, "wakes for {host} never went quiet");
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
}

// --- shared cases -------------------------------------------------------------------

fn principals(s: &dyn Store) {
    write(s, |tx| {
        let k = tx.ensure_user("krisz").unwrap();
        assert_eq!(tx.ensure_user("krisz").unwrap(), k);
        let ana = tx.ensure_user("ana").unwrap();
        assert_ne!(ana, k);
        assert_eq!(tx.ensure_host("box", k).unwrap(), k);
        assert_eq!(tx.ensure_host("box", ana).unwrap(), k, "a host keeps its first owner");
        assert_eq!(tx.user_id("ana").unwrap(), Some(ana));
        assert_eq!(tx.user_id("nobody").unwrap(), None);
    });
}

fn credentials(s: &dyn Store) {
    write(s, |tx| {
        let k = tx.ensure_user("krisz").unwrap();
        tx.ensure_host("box", k).unwrap();
        tx.ensure_host("old", k).unwrap();
        assert_eq!(tx.host_owner("box").unwrap(), Some((k, "krisz".to_string())));
        assert_eq!(tx.host_owner("nope").unwrap(), None);
        tx.insert_credential("bbbbbbbbbbbbdigest", k, "box", "laptop", "2026-10-09T02:00:00.000Z").unwrap();
        tx.insert_credential("aaaaaaaaaaaadigest", k, "old", "first", "2026-10-09T01:00:00.000Z").unwrap();
        assert_eq!(tx.authenticate("bbbbbbbbbbbbdigest").unwrap(), Some((k, "box".to_string())));
        assert_eq!(tx.authenticate("nope").unwrap(), None);
        let listed: Vec<(String, String, Option<String>)> =
            tx.credentials().unwrap().into_iter().map(|c| (c.host, c.user, c.label)).collect();
        assert_eq!(listed, vec![
            ("old".to_string(), "krisz".to_string(), Some("first".to_string())),
            ("box".to_string(), "krisz".to_string(), Some("laptop".to_string())),
        ]);
        assert_eq!(tx.revoke_credential("bbbbbbbbbbbb", "2026-10-09T03:00:00.000Z").unwrap(), 1);
        assert_eq!(tx.revoke_credential("bbbbbbbbbbbb", "2026-10-09T04:00:00.000Z").unwrap(), 0);
        assert_eq!(tx.authenticate("bbbbbbbbbbbbdigest").unwrap(), None, "revoked keys do not authenticate");
        let revoked = tx.credentials().unwrap().into_iter().find(|c| c.host == "box").unwrap();
        assert_eq!(revoked.revoked_at.as_deref(), Some("2026-10-09T03:00:00.000Z"));
    });
}

fn idempotency(s: &dyn Store) {
    write(s, |tx| {
        assert_eq!(tx.idempotency_get("1|claude:s1@box|", "message.send", "k1").unwrap(), None);
        tx.idempotency_put("1|claude:s1@box|", "message.send", "k1", "h1", r#"{"ok":1}"#, "t").unwrap();
    });
    read(s, |tx| {
        let got = tx.idempotency_get("1|claude:s1@box|", "message.send", "k1").unwrap();
        assert_eq!(got, Some(("h1".into(), r#"{"ok":1}"#.into())));
        assert_eq!(tx.idempotency_get("1|claude:s1@box|", "message.read", "k1").unwrap(), None);
    });
    let tx = s.begin(Mode::Write).unwrap();
    assert!(tx.idempotency_put("1|claude:s1@box|", "message.send", "k1", "h2", "{}", "t").is_err());
}

fn agents_and_byte_order(s: &dyn Store) {
    let names = ["x_1", "x.1", "x1", "x-1", "xa", "x"];
    let w = world(s);
    write(s, |tx| {
        for (i, n) in names.iter().enumerate() {
            let id = format!("01ORDER{i:019}");
            tx.insert_agent(&id, w.krisz, n).unwrap();
            assert_eq!(tx.agent_by_id(&id).unwrap().unwrap().binding_version, 0);
            tx.set_binding(&id, Some(&addr(Kind::Codex, "shared", "box"))).unwrap();
        }
        let at = tx.agents_at_address(w.krisz, &addr(Kind::Codex, "shared", "box")).unwrap();
        let mut expected: Vec<&str> = names.to_vec();
        expected.sort();
        assert_eq!(at.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), expected, "byte order, not locale order");
        assert!(tx.agents_at_address(w.ana, &addr(Kind::Codex, "shared", "box")).unwrap().is_empty());

        let bound = tx.bound_agents().unwrap();
        let mut sorted: Vec<String> = bound.iter().map(|a| a.canonical()).collect();
        let listed = sorted.clone();
        sorted.sort_by(|x, y| {
            let (xo, xn) = x.split_once('/').unwrap();
            let (yo, yn) = y.split_once('/').unwrap();
            (xo, xn).cmp(&(yo, yn))
        });
        assert_eq!(listed, sorted);
        assert_eq!(listed.first().map(String::as_str), Some("ana/c"));

        let a = tx.agent_by_name(w.krisz, "a").unwrap().unwrap();
        assert_eq!(a.id, w.a);
        assert_eq!(a.canonical(), "krisz/a");
        assert_eq!(a.address, Some(addr(Kind::Claude, "s1", "box")));
        assert_eq!(a.binding_version, 1);
        assert!(!a.is_human());
        assert!(tx.agent_by_id(&w.h).unwrap().unwrap().is_human());
        assert_eq!(tx.agent_by_name(w.ana, "a").unwrap(), None);

        tx.set_binding(&w.a, None).unwrap();
        let a = tx.agent_by_id(&w.a).unwrap().unwrap();
        assert_eq!((a.address, a.binding_version), (None, 2));
        tx.set_pin(&w.a, Some("claude:s1@box")).unwrap();
        tx.set_display(&w.a, Some("Alpha")).unwrap();
        let a = tx.agent_by_id(&w.a).unwrap().unwrap();
        assert_eq!((a.pinned.as_deref(), a.display.as_deref()), (Some("claude:s1@box"), Some("Alpha")));
        tx.set_pin(&w.a, None).unwrap();
        tx.set_display(&w.a, None).unwrap();
        let a = tx.agent_by_id(&w.a).unwrap().unwrap();
        assert_eq!((a.pinned, a.display), (None, None));
        assert!(!tx.bound_agents().unwrap().iter().any(|x| x.id == w.a));
    });
    let tx = s.begin(Mode::Write).unwrap();
    assert!(tx.insert_agent("01DUP", w.krisz, "b").is_err(), "one name per owner");
}

fn messages_and_attachments(s: &dyn Store) {
    let w = world(s);
    let blob: Vec<u8> = (0..=255u8).cycle().take(1 << 20).collect();
    let (s1, s2) = write(s, |tx| {
        let s1 = dm(tx, 1, &w.a, &w.b, "hello\nsecond line");
        tx.insert_attachment("01ATT2", &mid(1), "b.bin", &blob).unwrap();
        tx.insert_attachment("01ATT1", &mid(1), "a.txt", b"").unwrap();
        tx.insert_channel(&chan("ch1", "dev", &w.a)).unwrap();
        let s2 = post(tx, 2, &w.b, "ch1", "posted");
        (s1, s2)
    });
    assert!(s2 > s1 && s1 > 0);
    read(s, |tx| {
        let m = tx.message_by_id(&mid(1)).unwrap().unwrap();
        assert_eq!(
            (m.seq, m.sender.as_str(), m.recipient.as_deref(), m.channel.as_deref()),
            (s1, "krisz/a", Some("krisz/b"), None)
        );
        assert_eq!((m.place(), m.to(), m.preview()), ("dm".to_string(), "krisz/b".to_string(), "hello".to_string()));
        assert_eq!((m.reply_to, m.created_at.as_str()), (None, "2026-10-09T00:00:00.000Z"));
        let p = tx.message_by_id(&mid(2)).unwrap().unwrap();
        assert_eq!((p.place(), p.to(), p.recipient_id), ("#dev".to_string(), "#dev".to_string(), None));
        assert_eq!(p.reply_to.as_deref(), Some("01REPLY0000000000000000000"));
        assert_eq!(tx.message_by_id("01NOPE").unwrap(), None);

        let list = tx.attachment_list(&mid(1)).unwrap();
        assert_eq!(
            list.iter().map(|a| (a.id.as_str(), a.filename.as_str(), a.size)).collect::<Vec<_>>(),
            vec![("01ATT1", "a.txt", 0), ("01ATT2", "b.bin", 1 << 20)]
        );
        let att = tx.attachment("01ATT2").unwrap().unwrap();
        assert_eq!((att.message_id.as_str(), att.filename.as_str()), (mid(1).as_str(), "b.bin"));
        assert!(att.bytes == blob, "attachment bytes round-trip");
        assert_eq!(tx.attachment("01ATT1").unwrap().unwrap().bytes, Vec::<u8>::new());
        assert_eq!(tx.attachment("nope").unwrap(), None);
        assert_eq!(tx.message_count().unwrap(), 2);
    });
}

fn rollback_leaves_nothing(s: &dyn Store) {
    let w = world(s);
    {
        let tx = s.begin(Mode::Write).unwrap();
        dm(&*tx, 1, &w.a, &w.b, "never");
        tx.insert_attachment("01ATT", &mid(1), "f", b"bytes").unwrap();
        tx.insert_notification(&notification("n1", &mid(1), &w.b, 0)).unwrap();
        tx.idempotency_put("actor", "message.send", "k", "h", "{}", "t").unwrap();
        // dropped without commit
    }
    read(s, |tx| {
        assert_eq!(tx.message_count().unwrap(), 0);
        assert_eq!(tx.attachment("01ATT").unwrap(), None);
        assert!(tx.notifications().unwrap().is_empty());
        assert_eq!(tx.idempotency_get("actor", "message.send", "k").unwrap(), None);
    });
}

fn failed_statement_then_reuse(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        dm(tx, 1, &w.a, &w.b, "first");
    });
    for _ in 0..3 {
        let tx = s.begin(Mode::Write).unwrap();
        dm(&*tx, 2, &w.a, &w.b, "second");
        let dup = tx.insert_message(&NewMessage {
            id: &mid(1),
            sender_id: &w.a,
            recipient_id: Some(&w.b),
            channel_id: None,
            text: "dup",
            reply_to: None,
            created_at: "t",
        });
        assert!(dup.is_err(), "duplicate message id");
    }
    let seq = write(s, |tx| dm(tx, 3, &w.a, &w.b, "third"));
    read(s, |tx| {
        assert_eq!(tx.message_count().unwrap(), 2, "failed transactions left nothing");
        assert_eq!(tx.message_by_id(&mid(3)).unwrap().unwrap().seq, seq);
    });
}

/// A swallowed storage error must not let the op commit what is left: on
/// Postgres the transaction is already aborted and COMMIT would roll back silently.
fn failed_statement_refuses_commit(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        dm(tx, 1, &w.a, &w.b, "first");
    });
    let tx = s.begin(Mode::Write).unwrap();
    dm(&*tx, 2, &w.a, &w.b, "second");
    let _ = tx.insert_agent(&w.b, w.krisz, "again");
    let err = tx.commit().expect_err("commit after a failed statement");
    assert!(err.0.contains("aborted"), "{err}");
    read(s, |tx| assert_eq!(tx.message_count().unwrap(), 1));
}

fn channels(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        let ch = chan("ch1", "dev", &w.a);
        tx.insert_channel(&ch).unwrap();
        tx.insert_channel(&chan("ch0", "a-team", &w.a)).unwrap();
        assert_eq!(tx.channel_by_name("dev").unwrap(), Some(ch));
        assert_eq!(tx.channel_by_name("nope").unwrap(), None);
        assert_eq!(tx.channel_last_seq("ch1").unwrap(), 0);
        tx.insert_membership("ch1", &w.b, 0).unwrap();
        tx.insert_membership("ch1", &w.a, 0).unwrap();
        let p1 = post(tx, 1, &w.a, "ch1", "one");
        let p2 = post(tx, 2, &w.b, "ch1", "two");
        assert_eq!(tx.channel_last_seq("ch1").unwrap(), p2);
        tx.insert_membership("ch1", &w.c, p2).unwrap();
        assert_eq!(tx.channel_member_ids("ch1").unwrap(), vec![w.a.clone(), w.b.clone(), w.c.clone()]);
        assert_eq!(tx.read_seq("ch1", &w.a).unwrap(), Some(0));
        tx.advance_cursor("ch1", &w.a, p2).unwrap();
        tx.advance_cursor("ch1", &w.a, p1).unwrap();
        assert_eq!(tx.read_seq("ch1", &w.a).unwrap(), Some(p2), "cursors never move back");
        assert_eq!(tx.read_seq("ch1", &w.h).unwrap(), None);
        let summaries: Vec<(String, i64, i64)> =
            tx.channel_summaries().unwrap().into_iter().map(|c| (c.name, c.members, c.messages)).collect();
        assert_eq!(summaries, vec![("a-team".into(), 0, 0), ("dev".into(), 3, 2)]);
        assert_eq!(
            tx.channel_member_status("ch1").unwrap(),
            vec![("ana/c".into(), 0), ("krisz/a".into(), 0), ("krisz/b".into(), 2)]
        );
        tx.delete_membership("ch1", &w.b).unwrap();
        assert_eq!(tx.read_seq("ch1", &w.b).unwrap(), None);
    });
    let tx = s.begin(Mode::Write).unwrap();
    assert!(tx.insert_channel(&chan("ch2", "dev", &w.a)).is_err(), "names are unique");
}

fn inbox_unread_sent_search(s: &dyn Store) {
    let w = world(s);
    let seqs = write(s, |tx| {
        tx.insert_channel(&chan("ch1", "dev", &w.a)).unwrap();
        tx.insert_channel(&chan("ch2", "ops", &w.c)).unwrap();
        tx.insert_membership("ch1", &w.a, 0).unwrap();
        tx.insert_membership("ch1", &w.b, 0).unwrap();
        tx.insert_membership("ch2", &w.c, 0).unwrap();
        vec![
            dm(tx, 1, &w.b, &w.a, "b to a: needle"),
            dm(tx, 2, &w.a, &w.b, "a to b: needle"),
            post(tx, 3, &w.a, "ch1", "a posts needle"),
            post(tx, 4, &w.b, "ch1", "b posts"),
            post(tx, 5, &w.c, "ch2", "c posts needle elsewhere"),
            dm(tx, 6, &w.c, &w.b, "c to b"),
        ]
    });
    read(s, |tx| {
        assert_eq!(ids(&tx.inbox(&w.a, 0, 100).unwrap()), vec![mid(1), mid(4)]);
        assert_eq!(ids(&tx.inbox(&w.a, seqs[0], 100).unwrap()), vec![mid(4)]);
        assert_eq!(ids(&tx.inbox(&w.a, 0, 1).unwrap()), vec![mid(1)]);
        assert_eq!(ids(&tx.unread_all(&w.a, 100).unwrap()), vec![mid(1), mid(3), mid(4)], "own posts count as unread");
        assert_eq!(ids(&tx.unread_in_channel("ch1", &w.b, 100).unwrap()), vec![mid(3), mid(4)]);
        assert_eq!(ids(&tx.sent_by(&w.a, 0, 100).unwrap()), vec![mid(2), mid(3)]);
        assert_eq!(ids(&tx.sent_by(&w.a, seqs[1], 100).unwrap()), vec![mid(3)]);

        assert_eq!(ids(&tx.search_scan(&w.a, 0, SearchScope::All, 100).unwrap()), vec![mid(1), mid(2), mid(3), mid(4)]);
        assert_eq!(ids(&tx.search_scan(&w.a, seqs[1], SearchScope::All, 2).unwrap()), vec![mid(3), mid(4)]);
        assert_eq!(ids(&tx.search_scan(&w.a, 0, SearchScope::Channel("dev"), 100).unwrap()), vec![mid(3), mid(4)]);
        assert!(tx.search_scan(&w.a, 0, SearchScope::Channel("ops"), 100).unwrap().is_empty(), "not a member");
        assert_eq!(ids(&tx.search_scan(&w.a, 0, SearchScope::Partner(&w.b), 100).unwrap()), vec![mid(1), mid(2)]);
        assert_eq!(ids(&tx.search_scan(&w.b, 0, SearchScope::Partner(&w.c), 100).unwrap()), vec![mid(6)]);
        assert_eq!(ids(&tx.search_scan(&w.c, 0, SearchScope::All, 100).unwrap()), vec![mid(5), mid(6)]);
    });
    write(s, |tx| {
        tx.mark_dm_read(&mid(1), &w.a, "t").unwrap();
        tx.advance_cursor("ch1", &w.a, seqs[3]).unwrap();
        assert!(tx.inbox(&w.a, 0, 100).unwrap().is_empty());
        assert!(tx.unread_all(&w.a, 100).unwrap().is_empty());
    });
}

fn reactions_and_read_state(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        dm(tx, 1, &w.a, &w.b, "x");
        tx.set_reaction(&mid(1), &w.b, "👍", true).unwrap();
        tx.set_reaction(&mid(1), &w.a, "👍", true).unwrap();
        tx.set_reaction(&mid(1), &w.a, "+1", true).unwrap();
        tx.set_reaction(&mid(1), &w.a, "zz", true).unwrap();
        tx.set_reaction(&mid(1), &w.a, "zz", false).unwrap();
        let mut expected = vec![
            ("+1".to_string(), "krisz/a".to_string()),
            ("👍".to_string(), "krisz/a".to_string()),
            ("👍".to_string(), "krisz/b".to_string()),
        ];
        expected.sort();
        assert_eq!(tx.reactions(&mid(1)).unwrap(), expected);

        assert_eq!(tx.dm_read_at(&mid(1), &w.b).unwrap(), None);
        tx.mark_dm_read(&mid(1), &w.b, "first").unwrap();
        tx.mark_dm_read(&mid(1), &w.b, "second").unwrap();
        assert_eq!(tx.dm_read_at(&mid(1), &w.b).unwrap().as_deref(), Some("first"));

        tx.insert_channel(&chan("ch1", "dev", &w.a)).unwrap();
        for m in [&w.a, &w.b, &w.h, &w.c] {
            tx.insert_membership("ch1", m, 0).unwrap();
        }
        let p = post(tx, 2, &w.a, "ch1", "y");
        tx.advance_cursor("ch1", &w.b, p).unwrap();
        tx.advance_cursor("ch1", &w.c, p + 10).unwrap();
        assert_eq!(tx.channel_read_count("ch1", &w.a, p).unwrap(), (2, 3));
        assert_eq!(tx.channel_read_count("ch2", &w.a, p).unwrap(), (0, 0));
    });
}

fn notifications_and_attempts(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        let unbound = "01UNBOUND0000000000000000U";
        tx.insert_agent(unbound, w.krisz, "gone").unwrap();
        dm(tx, 1, &w.a, &w.b, "x");
        // inserted z before y: due order follows insertion, not id
        tx.insert_notification(&notification("z", &mid(1), &w.b, 100)).unwrap();
        tx.insert_notification(&notification("y", &mid(1), &w.h, 100)).unwrap();
        tx.insert_notification(&notification("x", &mid(1), &w.a, 50)).unwrap();
        tx.insert_notification(&notification("later", &mid(1), &w.a, 10_000)).unwrap();
        tx.insert_notification(&notification("far", &mid(1), &w.c, 0)).unwrap();
        tx.insert_notification(&notification("lost", &mid(1), unbound, 0)).unwrap();
        let due: Vec<String> = tx.due_notifications(1000, "box", 10).unwrap().into_iter().map(|d| d.id).collect();
        assert_eq!(due, vec!["x", "z", "y"]);
        assert_eq!(tx.due_notifications(1000, "box", 2).unwrap().len(), 2);
        let d = &tx.due_notifications(1000, "far", 10).unwrap()[0];
        assert_eq!(
            (d.id.as_str(), d.message_id.as_str(), d.recipient_id.as_str(), d.kind.as_str()),
            ("far", mid(1).as_str(), w.c.as_str(), "dm")
        );

        tx.set_notification_status("z", "dispatching").unwrap();
        tx.insert_notification(&NewNotification { kind: "post", ..notification("z", &mid(1), &w.b, 0) }).unwrap();
        let z = tx.notifications().unwrap().into_iter().find(|n| n.id == "z").unwrap();
        assert_eq!(
            (z.status.as_str(), z.kind.as_str(), z.next_attempt_at),
            ("dispatching", "dm", 100),
            "duplicate insert ignored"
        );
        assert_eq!(z.recipient_name, "krisz/b");
        tx.settle_notification("z", "pending", Some(5000)).unwrap();
        tx.settle_notification("y", "held", None).unwrap();
        let all = tx.notifications().unwrap();
        let get = |id: &str| all.iter().find(|n| n.id == id).unwrap().clone();
        assert_eq!((get("z").status, get("z").next_attempt_at), ("pending".into(), 5000));
        assert_eq!((get("y").status, get("y").next_attempt_at), ("held".into(), 100));
        assert_eq!(tx.count_notifications(w.krisz, &["pending"]).unwrap(), 4);
        assert_eq!(tx.count_notifications(w.ana, &["pending"]).unwrap(), 1);
        assert_eq!(tx.count_notifications(w.krisz, &["held", "denied"]).unwrap(), 1);
        assert_eq!(tx.count_notifications(w.ana, &["held", "denied"]).unwrap(), 0);
        assert_eq!(tx.count_notifications(w.krisz, &[]).unwrap(), 0);
        assert_eq!(tx.unbound_waiting(w.krisz).unwrap(), vec![("krisz/gone".to_string(), 1)]);
        assert_eq!(tx.unbound_waiting(w.ana).unwrap(), vec![]);

        // attempts: "m2" created after "m9", so it is the latest
        tx.insert_attempt("m9", "x", "bridge-1", 1, "box", "text m9").unwrap();
        tx.insert_attempt("m2", "x", "bridge-1", 1, "box", "text m2").unwrap();
        tx.insert_attempt("h1", "y", "bridge-1", 1, "box", "text h1").unwrap();
        tx.insert_attempt("old", "z", "bridge-0", 1, "box", "text old").unwrap();
        assert_eq!(tx.latest_attempt("x").unwrap().as_deref(), Some("m2"));
        assert_eq!(tx.latest_attempt("nope").unwrap(), None);
        assert_eq!(tx.attempt("m9").unwrap(), Some(("x".into(), "dispatching".into())));
        assert_eq!(tx.attempt("nope").unwrap(), None);
        tx.update_attempt("h1", "held", None, None, "2026-10-09T00:00:00.000Z").unwrap();
        tx.update_attempt("old", "held", Some("parked"), Some(3), "2026-10-09T00:00:00.000Z").unwrap();
        assert_eq!(tx.attempt_owner("old").unwrap(), Some(("bridge-0".to_string(), Some("box".to_string()), 3)));
        assert_eq!(tx.attempt_owner("h1").unwrap(), Some(("bridge-1".to_string(), Some("box".to_string()), 0)));
        assert_eq!(tx.attempt_owner("nope").unwrap(), None);
        let open: Vec<(String, String, String)> =
            tx.open_attempts("bridge-1").unwrap().into_iter().map(|o| (o.attempt_id, o.address, o.text)).collect();
        assert_eq!(open, vec![
            ("m9".to_string(), "claude:s1@box".to_string(), "text m9".to_string()),
            ("m2".to_string(), "claude:s1@box".to_string(), "text m2".to_string()),
            ("h1".to_string(), "human:krisz@box".to_string(), "text h1".to_string()),
        ]);
        let first = &tx.open_attempts("bridge-1").unwrap()[0];
        assert_eq!((first.recipient.as_str(), first.message_id.as_str(), first.binding_version), ("krisz/a", mid(1).as_str(), 1));
        let delivery = |status: &str, last: Option<&str>, reason: Option<&str>| {
            Some(conde::store::DmDelivery { status: status.into(), last_attempt: last.map(Into::into), reason: reason.map(Into::into) })
        };
        let latest = delivery("pending", Some("held"), Some("parked"));
        assert_eq!(tx.dm_delivery(&mid(1), &w.b).unwrap(), latest, "z with its latest attempt");
        assert_eq!(tx.dm_delivery(&mid(1), &w.a).unwrap(), delivery("pending", None, None), "first by id: 'later'");
        assert_eq!(tx.dm_delivery(&mid(1), &w.c).unwrap(), delivery("pending", None, None));
        assert_eq!(tx.dm_delivery("nope", &w.a).unwrap(), None);
        assert_eq!(tx.held_attempts("bridge-1").unwrap(), vec![("h1".to_string(), "human:krisz@box".to_string())]);
        // rebinding a outdates its attempts; bridge-0's hold has no live bridge
        tx.set_binding(&w.a, Some(&addr(Kind::Claude, "s2", "box"))).unwrap();
        let stale = tx.stale_attempts("bridge-1", "box").unwrap();
        assert_eq!(
            stale,
            vec![
                ("m9".to_string(), "recipient rebound".to_string()),
                ("m2".to_string(), "recipient rebound".to_string()),
                ("old".to_string(), "bridge instance gone".to_string()),
            ]
        );
        assert!(tx.stale_attempts("bridge-1", "far").unwrap().is_empty());
        let attempts = tx.attempts().unwrap();
        assert_eq!(attempts.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["m9", "m2", "h1", "old"]);
        let old = attempts.iter().find(|a| a.id == "old").unwrap();
        assert_eq!(
            (old.last_event_seq, old.status.as_str(), old.reason.as_deref(), old.bridge_instance.as_str()),
            (3, "held", Some("parked"), "bridge-0")
        );
        assert_eq!((old.notification_id.as_str(), old.target_binding_version), ("z", 1));
    });
}

fn diagnostics(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        assert_eq!(tx.integrity_check().unwrap(), "ok");
        assert_eq!(tx.count_bound_agents().unwrap(), 4);
        tx.set_binding(&w.c, None).unwrap();
        assert_eq!(tx.count_bound_agents().unwrap(), 3);
        tx.insert_channel(&chan("ch1", "dev", &w.a)).unwrap();
        tx.insert_channel(&chan("ch2", "b-ops", &w.a)).unwrap();
        for ch in ["ch1", "ch2"] {
            tx.insert_membership(ch, &w.a, 0).unwrap();
            tx.insert_membership(ch, &w.b, 0).unwrap();
        }
        post(tx, 1, &w.b, "ch1", "1");
        post(tx, 2, &w.b, "ch1", "2");
        post(tx, 3, &w.a, "ch1", "own");
        post(tx, 4, &w.b, "ch2", "3");
        assert_eq!(tx.unread_badges(&w.a).unwrap(), vec![("b-ops".to_string(), 1), ("dev".to_string(), 2)]);
        assert!(tx.unread_badges(&w.h).unwrap().is_empty());
    });
    assert_eq!(w.ana, read(s, |tx| tx.user_id("ana").unwrap().unwrap()));
}

fn writes_are_serialized(s: &dyn Store) {
    let started = Barrier::new(2);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let tx = s.begin(Mode::Write).unwrap();
            started.wait();
            std::thread::sleep(Duration::from_millis(300));
            tx.ensure_user("first").unwrap();
            tx.commit().unwrap();
        });
        started.wait();
        let t = Instant::now();
        let tx = s.begin(Mode::Write).unwrap();
        assert!(t.elapsed() >= Duration::from_millis(200), "second writer did not wait ({:?})", t.elapsed());
        assert!(tx.user_id("first").unwrap().is_some(), "second writer sees the first one's commit");
        tx.ensure_user("second").unwrap();
        tx.commit().unwrap();
        first.join().unwrap();
    });
}

fn pending_wakes_on_commit_only(s: &dyn Store) {
    let w = world(s);
    let rt = runtime();
    let mut watch = s.pending().subscribe();
    let mut elsewhere = s.pending().subscribe();
    settle(&rt, &mut watch, "box");
    settle(&rt, &mut elsewhere, "far");
    write(s, |tx| {
        dm(tx, 1, &w.a, &w.b, "x");
    });
    assert!(!rt.block_on(watch.wait("box", Duration::from_millis(400))), "no notification, no wake");

    {
        let tx = s.begin(Mode::Write).unwrap();
        tx.insert_notification(&notification("rolled", &mid(1), &w.b, 0)).unwrap();
    }
    assert!(!rt.block_on(watch.wait("box", Duration::from_millis(400))), "rolled back, no wake");

    let t = Instant::now();
    write(s, |tx| tx.insert_notification(&notification("n1", &mid(1), &w.b, 0)).unwrap());
    assert!(rt.block_on(watch.wait("box", Duration::from_secs(5))), "pending notification wakes its host");
    assert!(t.elapsed() < Duration::from_secs(1), "woken in {:?}", t.elapsed());
    assert!(!rt.block_on(elsewhere.wait("far", Duration::from_millis(400))), "other hosts stay asleep");
    settle(&rt, &mut watch, "box");

    // an abandoned attempt makes its notification pending again
    write(s, |tx| tx.settle_notification("n1", "dispatching", None).unwrap());
    assert!(!rt.block_on(watch.wait("box", Duration::from_millis(400))));
    write(s, |tx| tx.settle_notification("n1", "pending", Some(0)).unwrap());
    assert!(rt.block_on(watch.wait("box", Duration::from_secs(5))), "back to pending wakes");
    settle(&rt, &mut watch, "box");

    // binding an agent onto a host wakes that host
    write(s, |tx| tx.set_binding(&w.c, Some(&addr(Kind::Claude, "s9", "box"))).unwrap());
    assert!(rt.block_on(watch.wait("box", Duration::from_secs(5))), "rebinding wakes the new host");

    // the blocking waiter, for the local bridge thread
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(100));
            write(s, |tx| tx.insert_notification(&notification("n2", &mid(1), &w.b, 0)).unwrap());
        });
        let t = Instant::now();
        assert!(s.pending().wait_blocking(Duration::from_secs(5)));
        assert!(t.elapsed() < Duration::from_secs(2));
    });
}

// --- Postgres-only cases -------------------------------------------------------------

fn concurrent_startup_migrates_once(c: &Cluster) {
    let db = c.database();
    let url = c.url(&db);
    let barrier = Arc::new(Barrier::new(6));
    let replicas: Vec<_> = (0..6)
        .map(|_| {
            let (url, barrier) = (url.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                PgStore::connect(&url).map(|_| ())
            })
        })
        .collect();
    for r in replicas {
        r.join().unwrap().expect("every replica starts");
    }
    let mut client = c.client(&db);
    let rows = client.query("SELECT version FROM schema_migrations ORDER BY version", &[]).unwrap();
    let versions: Vec<i32> = rows.iter().map(|r| r.get(0)).collect();
    assert_eq!(versions, MIGRATIONS.iter().map(|m| m.0).collect::<Vec<_>>());
    assert!(migrate(&mut client).unwrap().is_empty(), "nothing left to apply");
}

fn edited_migration_is_refused(c: &Cluster) {
    let db = c.database();
    drop(PgStore::connect(&c.url(&db)).unwrap());
    c.client(&db).batch_execute("UPDATE schema_migrations SET checksum = 'edited' WHERE version = 1").unwrap();
    let err = PgStore::connect(&c.url(&db)).err().expect("refused");
    assert!(err.0.contains("changed after it was applied"), "{err}");
}

fn wake_crosses_replicas(c: &Cluster) {
    let url = c.url(&c.database());
    let (one, two) = (PgStore::connect(&url).unwrap(), PgStore::connect(&url).unwrap());
    let w = world(&one);
    write(&one, |tx| {
        dm(tx, 1, &w.a, &w.b, "x");
    });
    let rt = runtime();
    let mut watch = two.pending().subscribe();
    settle(&rt, &mut watch, "box");
    let t = Instant::now();
    write(&one, |tx| tx.insert_notification(&notification("n1", &mid(1), &w.b, 0)).unwrap());
    assert!(rt.block_on(watch.wait("box", Duration::from_secs(5))), "the other replica wakes");
    assert!(t.elapsed() < Duration::from_secs(1), "woken in {:?}", t.elapsed());
    // and replica two sees the committed row
    assert_eq!(read(&two, |tx| tx.due_notifications(0, "box", 10).unwrap()).len(), 1);
}

fn listener_survives_connection_loss(c: &Cluster) {
    let db = c.database();
    let store = PgStore::connect(&c.url(&db)).unwrap();
    let w = world(&store);
    write(&store, |tx| {
        dm(tx, 1, &w.a, &w.b, "x");
    });
    let rt = runtime();
    let mut watch = store.pending().subscribe();
    settle(&rt, &mut watch, "box");
    let killed: i64 = c
        .client(&db)
        .query_one(
            "SELECT COUNT(pg_terminate_backend(pid)) FROM pg_stat_activity
             WHERE datname = current_database() AND query LIKE 'LISTEN %'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(killed, 1, "one listener connection");
    assert!(rt.block_on(watch.wait("any-host", Duration::from_secs(5))), "a lost listener wakes everyone");
    settle(&rt, &mut watch, "box");
    write(&store, |tx| tx.insert_notification(&notification("n1", &mid(1), &w.b, 0)).unwrap());
    assert!(rt.block_on(watch.wait("box", Duration::from_secs(5))), "wakes again after reconnecting");
    assert!(store.ping());
}

fn read_transactions_are_read_only(c: &Cluster) {
    let store = c.store();
    let tx = store.begin(Mode::Read).unwrap();
    assert!(tx.ensure_user("x").is_err());
    drop(tx);
    write(&store, |tx| tx.ensure_user("x").unwrap());
}

/// A writer that stops talking inside its transaction (a replica killed
/// mid-op) loses its session, and with it the writer lock.
fn stalled_writer_releases_the_board(c: &Cluster) {
    let url = format!("{} options='-c idle_in_transaction_session_timeout=500'", c.url(&c.database()));
    let store = PgStore::connect(&url).unwrap();
    let stalled = store.begin(Mode::Write).unwrap();
    stalled.ensure_user("stalled").unwrap();
    let t = Instant::now();
    write(&store, |tx| tx.ensure_user("next").unwrap());
    assert!(t.elapsed() < Duration::from_secs(5), "next writer waited {:?}", t.elapsed());
    assert!(stalled.commit().is_err(), "the stalled transaction is gone");
    read(&store, |tx| {
        assert_eq!(tx.user_id("stalled").unwrap(), None);
        assert!(tx.user_id("next").unwrap().is_some());
    });
}

fn fencing_and_endpoints(s: &dyn Store) {
    let w = world(s);
    let mut events = s.pending().events();
    write(s, |tx| {
        assert_eq!(tx.bridge_instance("b1").unwrap(), None);
        assert!(tx.fence("box", "b1", "t1").unwrap().is_empty());
        assert_eq!(tx.active_bridges("box").unwrap(), vec!["b1".to_string()]);
        assert_eq!(tx.fence("box", "b2", "t2").unwrap(), vec!["b1".to_string()]);
        assert_eq!(tx.bridge_instance("b1").unwrap(), Some(("box".to_string(), "fenced".to_string())));
        assert_eq!(tx.bridge_instance("b2").unwrap(), Some(("box".to_string(), "active".to_string())));
        assert_eq!(tx.active_bridges("box").unwrap(), vec!["b2".to_string()]);
        assert!(tx.fence("far", "f1", "t3").unwrap().is_empty(), "hosts fence independently");
        assert_eq!(tx.fence("box", "b2", "t4").unwrap(), Vec::<String>::new(), "re-fencing as the active one is a no-op");
    });
    let rt = runtime();
    let fenced = rt.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Ok(conde::store::Wake::Fenced(i))) => return Some(i),
                Ok(Ok(_)) => continue,
                _ => return None,
            }
        }
    });
    assert_eq!(fenced.as_deref(), Some("b1"), "the fence is announced");

    write(s, |tx| {
        dm(tx, 1, &w.a, &w.a, "self");
        tx.insert_notification(&notification("n1", &mid(1), &w.a, 0)).unwrap();
        tx.insert_attempt("held1", "n1", "b2", 1, "box", "t").unwrap();
        tx.update_attempt("held1", "held", None, None, "2026-10-09T00:00:00.000Z").unwrap();
        tx.replace_endpoints("box", "b2", &[("claude", "s1"), ("codex", "t1")], "t5").unwrap();
        assert_eq!(tx.count_endpoints().unwrap(), 2);
        assert!(tx.gone_holds("b2", "box").unwrap().is_empty(), "s1 is live");
        tx.replace_endpoints("box", "b2", &[("codex", "t1"), ("codex", "t1")], "t6").unwrap();
        assert_eq!(tx.count_endpoints().unwrap(), 1, "replaced, duplicates folded");
        assert_eq!(tx.gone_holds("b2", "box").unwrap(), vec!["held1".to_string()]);
        assert!(tx.gone_holds("b1", "box").unwrap().is_empty(), "only the instance's own holds");
    });
}

fn provisional_acceptance_goes_stale(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        dm(tx, 1, &w.b, &w.a, "x");
        tx.insert_notification(&notification("n1", &mid(1), &w.a, 0)).unwrap();
        tx.insert_attempt("acc", "n1", "old", 1, "box", "t").unwrap();
        tx.update_attempt("acc", "accepted", None, None, "2026-10-09T00:00:00.000Z").unwrap();
        tx.settle_notification("n1", "notified", None).unwrap();
        assert_eq!(tx.stale_attempts("new", "box").unwrap(), vec![("acc".to_string(), "bridge instance gone".to_string())]);
        tx.settle_notification("n1", "delivered", None).unwrap();
        assert!(tx.stale_attempts("new", "box").unwrap().is_empty(), "confirmed deliveries are never touched");
    });
}

fn import_helpers(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        let pinned = addr(Kind::Codex, "t9", "box");
        tx.insert_imported_agent(&conde::store::ImportedAgent {
            id: "01IMPORTED0000000000000000",
            owner_id: w.krisz,
            name: "old.role",
            display: Some("Old Role"),
            address: Some(&pinned),
            binding_version: 1,
            pinned: Some("codex:t9@box"),
        })
        .unwrap();
        let a = tx.agent_by_name(w.krisz, "old.role").unwrap().unwrap();
        assert_eq!((a.display.as_deref(), a.address.as_ref(), a.binding_version), (Some("Old Role"), Some(&pinned), 1));
        tx.insert_channel(&chan("ch1", "dev", &w.a)).unwrap();
        tx.insert_membership("ch1", &w.a, 0).unwrap();
        let m = NewMessage {
            id: &mid(5),
            sender_id: &w.a,
            recipient_id: None,
            channel_id: Some("ch1"),
            text: "imported",
            reply_to: None,
            created_at: "2025-01-01T00:00:00.000Z",
        };
        assert!(tx.insert_message_if_absent(&m).unwrap());
        assert!(!tx.insert_message_if_absent(&NewMessage { text: "changed", ..m }).unwrap());
        assert_eq!(tx.message_by_id(&mid(5)).unwrap().unwrap().text, "imported", "the first copy stays");
        post(tx, 7, &w.a, "ch1", "later");
        let at5 = tx.channel_seq_at("ch1", &mid(5)).unwrap();
        assert_eq!(tx.channel_seq_at("ch1", &mid(6)).unwrap(), at5);
        assert!(tx.channel_seq_at("ch1", &mid(9)).unwrap() > at5);
        assert_eq!(tx.channel_seq_at("ch1", &mid(1)).unwrap(), 0);
        assert_eq!(tx.channel_message_ids("ch1").unwrap(), vec![mid(5), mid(7)]);
        tx.insert_attachment("01ATT", &mid(5), "f.bin", &[1, 2, 3]).unwrap();
        assert_eq!(tx.attachment_sizes(&mid(5), "f.bin").unwrap(), Some((3, 3)));
        assert_eq!(tx.attachment_sizes(&mid(5), "nope").unwrap(), None);
        assert!(tx.insert_reaction_if_absent(&mid(5), &w.a, "+1", true).unwrap());
        assert!(!tx.insert_reaction_if_absent(&mid(5), &w.a, "+1", false).unwrap());
        assert_eq!(tx.reactions(&mid(5)).unwrap().len(), 1, "the existing reaction is kept");
        dm(tx, 8, &w.b, &w.a, "dm");
        assert!(tx.mark_dm_read(&mid(8), &w.a, "t").unwrap());
        assert!(!tx.mark_dm_read(&mid(8), &w.a, "t2").unwrap());

        let st = tx.import_state("krisz").unwrap();
        assert_eq!(st.user_id, Some(w.krisz));
        assert_eq!(st.agents.get("old.role"), Some(&(Some("codex:t9@box".to_string()), Some("codex:t9@box".to_string()))));
        assert_eq!(st.agents.get("a").map(|x| x.0.clone()), Some(Some("claude:s1@box".to_string())));
        assert!(!st.agents.contains_key("c"), "other owners' agents are not this owner's");
        assert!(st.channels.contains("dev") && st.messages.contains(&mid(5)));
        assert!(st.attachments.contains(&(mid(5), "f.bin".to_string())));
        assert!(st.memberships.contains(&("dev".to_string(), "a".to_string())));
        assert!(st.dm_read.contains(&(mid(8), "a".to_string())));
        assert!(st.reactions.contains(&(mid(5), "a".to_string(), "+1".to_string())));
        assert_eq!(tx.import_state("nobody").unwrap().user_id, None);
    });
}

fn channel_topics(s: &dyn Store) {
    let w = world(s);
    write(s, |tx| {
        tx.insert_channel(&Channel { topic: Some("planning".into()), ..chan("ch1", "dev", &w.a) }).unwrap();
        tx.insert_channel(&chan("ch2", "ops", &w.b)).unwrap();
        let dev = tx.channel_by_name("dev").unwrap().unwrap();
        assert_eq!(tx.channel_id_by_name("dev").unwrap().as_deref(), Some("ch1"));
        assert_eq!(tx.channel_id_by_name("nope").unwrap(), None);
        assert_eq!((dev.created_by.as_str(), dev.topic.as_deref()), (w.a.as_str(), Some("planning")));
        tx.set_channel_topic("ch2", Some("pager rota")).unwrap();
        tx.set_channel_topic("ch1", None).unwrap();
        let topics: Vec<(String, Option<String>)> =
            tx.channel_summaries().unwrap().into_iter().map(|c| (c.name, c.topic)).collect();
        assert_eq!(topics, vec![("dev".into(), None), ("ops".into(), Some("pager rota".into()))]);
    });
}

/// Owner 1's agent `a` with one delivered attempt and agent `b` with a delivered one, as SQL.
const REACH_ROWS: &str = "
    INSERT INTO users(name) VALUES ('krisz');
    INSERT INTO agents(agent_id, owner_id, name_lower, address_kind, address_target, host_id)
        VALUES ('a', 1, 'a', 'claude', 's-a', 'box'), ('b', 1, 'b', 'claude', 's-b', 'box'),
               ('c', 1, 'c', 'claude', 's-c', 'box');
    INSERT INTO messages(message_id, sender_agent_id, recipient_agent_id, text, created_at)
        VALUES ('m1', 'c', 'a', 'x', '2026-10-01T00:00:00.000Z'), ('m2', 'c', 'b', 'y', '2026-10-01T00:00:00.000Z');
    INSERT INTO notifications(notification_id, message_id, recipient_agent_id, kind, binding_version, status, next_attempt_at)
        VALUES ('m1.a', 'm1', 'a', 'dm', 1, 'delivered', 0), ('m2.b', 'm2', 'b', 'dm', 1, 'notified', 0);
    INSERT INTO attempts(attempt_id, notification_id, bridge_instance, target_binding_version, status)
        VALUES ('t1', 'm1.a', 'i', 1, 'delivered'), ('t2', 'm2.b', 'i', 1, 'accepted');";

/// What agent_health says of `a` (delivered, no time) and `b` (accepted Claude: not reached),
/// then of `b` after an older writer (`older_write`, raw SQL without `settled_at`) delivers it.
fn reach_without_time(s: &dyn Store, older_write: impl FnOnce(&str)) {
    let health = read(s, |tx| tx.agent_health(1, "2026-10-01T00:00:00.000Z").unwrap());
    let of = |id: &str| health.iter().find(|h| h.agent_id == id).unwrap().clone();
    assert_eq!((of("a").reached, of("a").last_reached), (true, None), "reached, at a time not recorded");
    assert_eq!((of("b").reached, of("b").last_reached), (false, None), "never reached");
    // An older writer during a roll records a delivery without the time: reached, and no
    // invented timestamp.
    older_write("UPDATE attempts SET status = 'delivered' WHERE attempt_id = 't2'");
    let health = read(s, |tx| tx.agent_health(1, "2026-10-01T00:00:00.000Z").unwrap());
    let b = health.iter().find(|h| h.agent_id == "b").unwrap();
    assert_eq!((b.reached, b.last_reached.as_deref()), (true, None));
}

/// SQLite: deliveries recorded before `settled_at` existed (the column added on open) or by an
/// older writer keep counting as reached, without a time.
fn deliveries_without_a_settled_time_still_count_as_reached_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conde.db");
    drop(SqliteStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("ALTER TABLE attempts DROP COLUMN settled_at").unwrap();
    conn.execute_batch(REACH_ROWS).unwrap();
    drop(conn);
    let store = SqliteStore::open(&path).unwrap();
    reach_without_time(&store, |sql| {
        store.with_connection(|c| c.execute_batch(sql)).unwrap();
    });
}

/// Postgres: a board migrated to version 4 with delivered attempts gains `settled_at` (NULL for
/// them) at the next start; they still count as reached, without a time.
fn deliveries_from_before_migration_5_still_count_as_reached(c: &Cluster) {
    let db = c.database();
    let mut client = c.client(&db);
    client
        .batch_execute(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL,
             applied_at TIMESTAMPTZ NOT NULL DEFAULT now());",
        )
        .unwrap();
    for (version, name, sql) in &MIGRATIONS[..4] {
        client.batch_execute(sql).unwrap();
        client
            .execute(
                "INSERT INTO schema_migrations(version, name, checksum) VALUES ($1, $2, $3)",
                &[version, name, &conde::util::sha256_hex(sql.as_bytes())],
            )
            .unwrap();
    }
    client.batch_execute(REACH_ROWS).unwrap();
    let store = PgStore::connect(&c.url(&db)).unwrap();
    reach_without_time(&store, |sql| client.batch_execute(sql).unwrap());
}

/// A board file from before topics: opening it adds the column, data intact.
fn old_board_file_gains_topics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conde.db");
    {
        let s = SqliteStore::open(&path).unwrap();
        let w = world(&s);
        write(&s, |tx| tx.insert_channel(&chan("ch1", "dev", &w.a)).unwrap());
    }
    rusqlite::Connection::open(&path).unwrap().execute_batch("ALTER TABLE channels DROP COLUMN topic").unwrap();
    let s = SqliteStore::open(&path).unwrap();
    write(&s, |tx| {
        assert_eq!(tx.channel_by_name("dev").unwrap().unwrap().topic, None);
        tx.set_channel_topic("ch1", Some("back")).unwrap();
    });
    assert_eq!(read(&s, |tx| tx.channel_by_name("dev").unwrap().unwrap().topic).as_deref(), Some("back"));
}

/// A database the phase-one binary migrated (version 1 only) gets migration 2 on the next start.
fn phase_one_database_gains_topics(c: &Cluster) {
    let db = c.database();
    let mut client = c.client(&db);
    client.batch_execute(MIGRATIONS[0].2).unwrap();
    client
        .batch_execute(&format!(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL,
             applied_at TIMESTAMPTZ NOT NULL DEFAULT now());
             INSERT INTO schema_migrations(version, name, checksum) VALUES (1, 'init', '{}');
             INSERT INTO users(name) VALUES ('krisz');
             INSERT INTO agents(agent_id, owner_id, name_lower) VALUES ('a1', 1, 'a');
             INSERT INTO channels(channel_id, name_lower, created_by) VALUES ('ch1', 'dev', 'a1');",
            conde::util::sha256_hex(MIGRATIONS[0].2.as_bytes())
        ))
        .unwrap();
    let store = PgStore::connect(&c.url(&db)).unwrap();
    write(&store, |tx| {
        assert_eq!(tx.channel_by_name("dev").unwrap().unwrap().topic, None);
        tx.set_channel_topic("ch1", Some("migrated")).unwrap();
    });
    assert_eq!(read(&store, |tx| tx.integrity_check().unwrap()), "ok");
    let versions: Vec<i32> =
        c.client(&db).query("SELECT version FROM schema_migrations ORDER BY version", &[]).unwrap().iter().map(|r| r.get(0)).collect();
    assert_eq!(versions, MIGRATIONS.iter().map(|m| m.0).collect::<Vec<i32>>());
}

/// Entra identities, disabled users, browser sessions and one-time records; revocations and
/// disables announce their hosts on commit.
fn web_sign_in_and_disabled_users(store: &dyn Store) {
    let mut events = store.pending().events();
    let w = store.begin(Mode::Write).unwrap();
    let ann = w.ensure_user("ann").unwrap();
    let row = |bound, disabled| UserRow { user_id: ann, name: "ann".into(), disabled, bound };
    assert_eq!(w.user_by_name("ann").unwrap(), Some(row(false, false)));
    assert_eq!(w.user_by_identity("t-1", "o-1").unwrap(), None);
    w.bind_identity(ann, "t-1", "o-1", Some("Ann"), Some("ann@example.com")).unwrap();
    assert_eq!(w.user_by_identity("t-1", "o-1").unwrap(), Some(row(true, false)));
    assert_eq!(w.user_by_id(ann).unwrap(), Some(row(true, false)));
    w.set_identity_profile(ann, Some("Ann B."), None).unwrap();
    w.ensure_host("box", ann).unwrap();
    w.insert_credential("digest-1", ann, "box", "box", "2026-10-09T00:00:00.000Z").unwrap();
    assert_eq!(w.authenticate("digest-1").unwrap(), Some((ann, "box".to_string())));
    assert_eq!(w.set_user_disabled(ann, true).unwrap(), vec!["box".to_string()]);
    assert_eq!(w.user_by_id(ann).unwrap(), Some(row(true, true)));
    assert_eq!(w.authenticate("digest-1").unwrap(), None, "a disabled user's key does not authenticate");
    w.set_user_disabled(ann, false).unwrap();
    assert_eq!(w.authenticate("digest-1").unwrap(), Some((ann, "box".to_string())));
    assert_eq!(w.host_last_seen("box").unwrap(), None);

    let session = WebSessionRow {
        user_id: ann,
        user_name: "ann".into(),
        display: "Ann".into(),
        csrf: "c".into(),
        created_at: 1_000,
        last_seen: 1_000,
    };
    w.insert_web_session("s-1", &session).unwrap();
    w.touch_web_session("s-1", 1_500).unwrap();
    assert_eq!(w.web_session("s-1").unwrap(), Some(WebSessionRow { last_seen: 1_500, ..session.clone() }));
    w.insert_web_session("s-old", &WebSessionRow { last_seen: 100, ..session.clone() }).unwrap();

    w.put_web_token("cli_code", "k-1", "{\"a\":1}", 1_000).unwrap();
    w.put_web_token("login", "k-1", "{}", 1_000).unwrap();
    w.put_web_token("cli_code", "k-old", "{}", 100).unwrap();
    assert_eq!(w.take_web_token("cli_code", "k-1").unwrap(), Some(("{\"a\":1}".to_string(), 1_000)));
    assert_eq!(w.take_web_token("cli_code", "k-1").unwrap(), None, "taken once");
    w.purge_web(1_600, 1_000, 10_000, &[("cli_code", 300), ("login", 600)]).unwrap();
    assert_eq!(w.take_web_token("cli_code", "k-old").unwrap(), None, "expired records are purged");
    assert!(w.take_web_token("login", "k-1").unwrap().is_some(), "kinds are separate");
    assert!(w.web_session("s-old").unwrap().is_none() && w.web_session("s-1").unwrap().is_some());
    w.delete_web_session("s-1").unwrap();
    assert!(w.web_session("s-1").unwrap().is_none());

    assert_eq!(w.revoke_host_credentials("box", "2026-10-09T00:00:01.000Z").unwrap(), 1);
    assert_eq!(w.authenticate("digest-1").unwrap(), None);
    assert_eq!(w.revoke_host_credentials("box", "2026-10-09T00:00:02.000Z").unwrap(), 0);
    while let Ok(wake) = events.try_recv() {
        assert!(!matches!(wake, Wake::Revoked(_)), "nothing is revoked before the commit");
    }
    w.commit().unwrap();

    if store.backend() == "sqlite" {
        // In-process on SQLite; Postgres announces through NOTIFY (the listener suites).
        let mut revoked = Vec::new();
        while let Ok(wake) = events.try_recv() {
            if let Wake::Revoked(host) = wake {
                revoked.push(host);
            }
        }
        assert!(!revoked.is_empty() && revoked.iter().all(|h| h == "box"), "{revoked:?}");
    }
}

fn register_frame(name: &str) -> serde_json::Value {
    let mut req = conde::protocol::Request::new(name, "agent.register", serde_json::json!({"name": name}));
    req.actor = Some(format!("codex:{name}"));
    req.idempotency_key = Some(name.into());
    serde_json::to_value(req).unwrap()
}

/// A takeover whose fence event never reaches the old bridge's replica (no
/// listener there; only the after-gap Wake::All): the old connection is still
/// closed once its poll finds the durable fence, and a request it carries after
/// the takeover is refused at admission, changing nothing.
fn missed_fence_closes_the_old_connection(c: &Cluster) {
    use conde::dispatch::Dispatcher;
    use conde::store::postgres::PgOpen;
    use conde_remote::{BridgeClient, BridgeConfig, ServerConfig, StopReason};
    let url = c.url(&c.database());
    let one = Arc::new(Dispatcher::connect(&url, PgOpen::COMMAND).unwrap());
    let two = Dispatcher::connect(&url, PgOpen::COMMAND).unwrap();
    let key = "lost-fence-key";
    one.with_tx(|tx| {
        conde::ops::hosts::insert_key(tx, "review", "box", &conde_remote::token_digest(key), "test", "2026-10-09T00:00:00Z")
    })
    .unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let slow = ServerConfig { poll_interval: Duration::from_secs(30), ..ServerConfig::default() };
    let (server, client) = rt.block_on(async {
        let server = conde::cloud::start(one.clone(), "127.0.0.1:0".parse().unwrap(), slow).await.unwrap();
        let client = BridgeClient::start(BridgeConfig::new(format!("ws://{}/v1/bridge", server.local_addr()), key, "box"), |_| {});
        let reply = tokio::time::timeout(Duration::from_secs(5), client.call(register_frame("before"))).await.unwrap().unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        (server, client)
    });
    let old = one.with_conn(|tx| tx.active_bridges("box")).unwrap().pop().unwrap();
    two.with_tx(|tx| Ok(tx.fence("box", "replacement", "2026-10-09T00:00:01Z")?)).unwrap();
    let uid = one.with_conn(|tx| tx.user_id("review")).unwrap().unwrap();

    // admission: a request carried by the fenced connection is refused
    let board = conde::cloud::Board::new(one.clone());
    let principal = conde_remote::Principal { user_id: uid.to_string(), host_id: "box".into() };
    let reply = conde_remote::Dispatcher::dispatch_bridge(&board, &principal, &old, register_frame("direct"));
    assert_eq!((reply["ok"].clone(), reply["error"]["code"].clone()), (serde_json::json!(false), serde_json::json!("forbidden")), "{reply}");

    // recovery: the after-gap signal closes the connection through its poll
    one.store().pending().raise(conde::store::Wake::All);
    rt.block_on(async {
        let reason = tokio::time::timeout(Duration::from_secs(5), client.stopped()).await.expect("the old bridge was closed");
        assert_eq!(reason, StopReason::Fenced);
        assert!(client.call(register_frame("after")).await.is_err(), "a stopped bridge carries no request");
        server.shutdown().await.unwrap();
    });
    for name in ["direct", "after"] {
        assert_eq!(two.with_conn(|tx| tx.agent_by_name(uid, name)).unwrap(), None, "{name} must not exist");
    }
    assert!(two.with_conn(|tx| tx.agent_by_name(uid, "before")).unwrap().is_some());
}

/// Two replicas on one board: the same idempotency key yields one message, and
/// concurrent claims for one bridge create one attempt.
fn replicas_share_idempotency_and_claims(c: &Cluster) {
    use conde::dispatch::Dispatcher;
    use conde::protocol::Request;
    use conde::store::postgres::PgOpen;
    use serde_json::json;
    let url = c.url(&c.database());
    let one = Arc::new(Dispatcher::connect(&url, PgOpen::COMMAND).unwrap());
    let two = Arc::new(Dispatcher::connect(&url, PgOpen::COMMAND).unwrap());
    let p = one.principal("review", "box", true).unwrap();
    for (actor, name) in [("codex:alice", "alice"), ("codex:bob", "bob")] {
        let mut req = Request::new(name, "agent.register", json!({"name": name}));
        req.actor = Some(actor.into());
        req.idempotency_key = Some(name.into());
        let r = one.dispatch(&p, &req);
        assert!(r.error.is_none(), "{:?}", r.error);
    }
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<serde_json::Value> = [one.clone(), two.clone()]
        .into_iter()
        .map(|d| {
            let (p, barrier) = (p.clone(), barrier.clone());
            std::thread::spawn(move || {
                let mut req = Request::new("send", "message.send", json!({"to": {"agent": "bob"}, "text": "one"}));
                req.actor = Some("codex:alice".into());
                req.idempotency_key = Some("same-key".into());
                barrier.wait();
                let r = d.dispatch(&p, &req);
                assert!(r.error.is_none(), "{:?}", r.error);
                r.result.unwrap()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    assert_eq!(results[0]["message_id"], results[1]["message_id"]);
    let bp = one.bridge_principal(&p);
    assert!(one.dispatch(&bp, &Request::new("fence", "bridge.fence", json!({}))).error.is_none());
    let barrier = Arc::new(Barrier::new(2));
    let jobs: usize = [one, two]
        .into_iter()
        .map(|d| {
            let (p, barrier) = (bp.clone(), barrier.clone());
            std::thread::spawn(move || {
                let req = Request::new("claim", "bridge.claim", json!({}));
                barrier.wait();
                let r = d.dispatch(&p, &req);
                assert!(r.error.is_none(), "{:?}", r.error);
                r.result.unwrap()["jobs"].as_array().unwrap().len()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .sum();
    assert_eq!(jobs, 1, "only one replica creates the attempt");
}

/// The last handle of a Postgres board may go away on an async worker (server
/// shutdown, test teardown): closing it must not build or block on a runtime.
fn dropping_a_board_on_an_async_worker(c: &Cluster) {
    let url = c.url(&c.database());
    let board = Arc::new(conde::dispatch::Dispatcher::connect(&url, conde::store::postgres::PgOpen::SERVER).unwrap());
    board.principal("krisz", "box", true).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let dropped = rt.block_on(async move { tokio::spawn(async move { drop(board) }).await });
    assert!(dropped.is_ok(), "drop panicked on the worker: {dropped:?}");
}
