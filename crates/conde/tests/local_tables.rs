//! The local board's own tables (DESIGN-v2.md §4): created on open, including in an existing
//! file, and the schema enforces the publication and operation-record invariants.

use conde::store::sqlite::SqliteStore;
use rusqlite::{Connection, params};

fn open() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conde.db");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    // The rows below need no agents.
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    (dir, conn)
}

fn publication(c: &Connection, agent: &str, name: &str, mailbox: Option<&str>, generation: Option<i64>, state: &str) -> rusqlite::Result<usize> {
    c.execute(
        "INSERT INTO publications(board, agent_id, shared_name, mailbox_id, route_generation, state, updated_at)
         VALUES ('dakai', ?1, ?2, ?3, ?4, ?5, 't0')",
        params![agent, name, mailbox, generation, state],
    )
}

fn op(c: &Connection, id: &str, kind: &str, agent: Option<&str>, state: &str) -> rusqlite::Result<usize> {
    c.execute(
        "INSERT INTO board_ops(op_id, board, kind, agent_id, request_json, state, created_at, updated_at)
         VALUES (?1, 'dakai', ?2, ?3, '{}', ?4, 't0', 't0')",
        params![id, kind, agent, state],
    )
}

#[test]
fn an_existing_file_gains_the_tables_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conde.db");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE publications; DROP TABLE board_ops;").unwrap();
    drop(conn);
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('publications', 'board_ops')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn a_publication_has_its_mailbox_exactly_when_not_publishing() {
    let (_dir, c) = open();
    publication(&c, "a1", "worker", None, None, "publishing").unwrap();
    publication(&c, "a2", "desk", Some("01MB2"), Some(1), "published").unwrap();
    publication(&c, "a3", "lane", Some("01MB3"), Some(4), "unpublishing").unwrap();
    assert!(publication(&c, "a4", "x4", Some("01MB4"), Some(1), "publishing").is_err(), "publishing with a mailbox");
    assert!(publication(&c, "a5", "x5", None, None, "published").is_err(), "published without a mailbox");
    assert!(publication(&c, "a6", "x6", Some("01MB6"), None, "published").is_err(), "mailbox without a generation");
    assert!(publication(&c, "a7", "x7", Some("01MB7"), Some(1), "gone").is_err(), "unknown state");
    assert!(publication(&c, "a8", "desk", Some("01MB8"), Some(1), "published").is_err(), "shared name taken");
    assert!(publication(&c, "a9", "x9", Some("01MB2"), Some(1), "published").is_err(), "mailbox taken");
    assert!(publication(&c, "a2", "x10", Some("01MB10"), Some(1), "published").is_err(), "agent published twice");
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert!(publication(&c, "a11", "x11", None, None, "publishing").is_err(), "no such local agent");
}

#[test]
fn one_pending_op_per_agent_and_one_live_migrate_per_board() {
    let (_dir, c) = open();
    op(&c, "op1", "publish", Some("a1"), "done").unwrap();
    op(&c, "op2", "unpublish", Some("a1"), "pending").unwrap();
    assert!(op(&c, "op3", "publish", Some("a1"), "pending").is_err(), "second pending op of an agent");
    op(&c, "op4", "publish", Some("a2"), "pending").unwrap();

    op(&c, "m1", "migrate", None, "failed").unwrap();
    op(&c, "m2", "migrate", None, "pending").unwrap();
    assert!(op(&c, "m3", "migrate", None, "done").is_err(), "second live migrate");

    assert!(op(&c, "op5", "publish", None, "pending").is_err(), "publish without an agent");
    assert!(op(&c, "op6", "migrate", Some("a3"), "failed").is_err(), "migrate with an agent");
    assert!(op(&c, "op7", "rename", Some("a3"), "pending").is_err(), "unknown kind");
    assert!(op(&c, "op8", "publish", Some("a3"), "running").is_err(), "unknown state");
}
