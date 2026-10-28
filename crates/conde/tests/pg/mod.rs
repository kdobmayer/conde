//! Postgres for the dispatcher and cloud suites: set CONDE_TEST_PG to a
//! connection string (key=value or URL) whose user may CREATE DATABASE, and
//! every board gets a fresh database there. Unset, the suites use SQLite and
//! the Postgres-only cases report their skip with `skip`.

#![allow(dead_code)]

/// The URL of a fresh, empty database on the CONDE_TEST_PG server, or None.
pub fn fresh_database() -> Option<String> {
    let admin = std::env::var("CONDE_TEST_PG")
        .ok()
        .filter(|s| !s.is_empty())?;
    let name = format!("t_{}", conde::util::new_ulid().to_lowercase());
    let mut client = postgres::Client::connect(&admin, postgres::NoTls)
        .unwrap_or_else(|e| panic!("CONDE_TEST_PG is set but unreachable: {e}"));
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .expect("CREATE DATABASE");
    let joiner = if !admin.contains("://") {
        " "
    } else if admin.contains('?') {
        "&"
    } else {
        "?"
    };
    Some(format!("{admin}{joiner}dbname={name}"))
}

/// Reports a Postgres case that did not run. Written to the stderr handle, not with
/// `eprintln!`, so the line shows in a plain `cargo test` run (libtest captures the macros).
pub fn skip(what: &str) {
    use std::io::Write as _;
    // CI runs a Postgres service; a skip there means the variable went missing
    assert!(
        std::env::var_os("CI").is_none(),
        "CONDE_TEST_PG is unset in CI: {what} would not run"
    );
    let test = std::thread::current().name().unwrap_or("?").to_string();
    let _ = writeln!(
        std::io::stderr(),
        "{test}: skipped {what} (CONDE_TEST_PG unset)"
    );
}
