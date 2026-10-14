//! Attempt recovery, ownership, fencing and endpoint liveness in the bridge ops.

mod common;

use common::*;
use conde::dispatch::Principal;
use serde_json::{Value, json};

fn setup() -> Board {
    let b = Board::new();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    b.register(CAROL, "carol");
    b
}

fn bridge_call(b: &Board, p: &Principal, op: &str, body: Value) -> Reply {
    b.call_as(p, "human:krisz", op, body)
}

#[test]
fn provisional_acceptance_is_resent_after_restart_and_after_rebind() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut old = b.bridge(FakeNotifier::default());
    old.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "notified");
    drop(old); // its receipts can never arrive now
    let mut new = b.bridge(FakeNotifier::default());
    new.pass().unwrap();
    assert_eq!(new.notifier().sent.len(), 1, "re-sent once by the new instance");
    new.pass().unwrap();
    assert_eq!(new.notifier().sent.len(), 1, "and only once");

    b.register("codex:t-new", "bob");
    new.pass().unwrap();
    assert_eq!(new.notifier().sent.len(), 2);
    assert_eq!(new.notifier().sent[1].0, "codex:t-new@box");
    assert_eq!(b.status_of(&mid, "bob"), "notified");
}

#[test]
fn confirmed_delivery_and_read_messages_are_not_resent_after_restart() {
    let b = setup();
    let delivered = b.send(ALICE, "bob", "x");
    let read = b.send(ALICE, "carol", "y");
    let mut old = b.bridge(FakeNotifier::default());
    old.pass().unwrap();
    let attempt = old.notifier().sent_to("codex:t-bob")[0].2.clone();
    old.notifier().receipt(&attempt, conde::notify::ReceiptStatus::Delivered);
    old.pass().unwrap();
    b.ok(CAROL, "message.read", json!({"id": read}));
    drop(old);
    let mut new = b.bridge(FakeNotifier::default());
    new.pass().unwrap();
    assert!(new.notifier().sent.is_empty(), "{:?}", new.notifier().sent);
    assert_eq!(b.status_of(&delivered, "bob"), "delivered");
    assert_eq!(b.status_of(&read, "carol"), "read");
}

#[test]
fn only_the_claiming_bridge_may_report() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    let attempt = br.notifier().last_attempt();
    let body = json!({"attempt_id": attempt, "outcome": "denied"});

    let foreign = b.d.bridge_principal(&b.other("foreign", "foreign-host"));
    let err = bridge_call(&b, &foreign, "bridge.report", body.clone()).unwrap_err();
    assert!(err.1.contains("belongs to bridge"), "{err:?}");
    // Same host, another instance (a restarted or rogue bridge): refused too.
    let sibling = b.d.bridge_principal(&b.p);
    let err = bridge_call(&b, &sibling, "bridge.report", body.clone()).unwrap_err();
    assert!(err.1.contains("belongs to bridge"), "{err:?}");
    // The right instance on a foreign host (an id is only a claim): refused.
    let mut spoof = foreign.clone();
    spoof.bridge_instance = Some(br.instance().to_string());
    assert!(bridge_call(&b, &spoof, "bridge.report", body).is_err());
    assert_eq!(b.status_of(&mid, "bob"), "notified");
}

#[test]
fn event_seq_makes_a_resent_outcome_a_no_op() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let p = b.d.bridge_principal(&b.p);
    let jobs = bridge_call(&b, &p, "bridge.claim", json!({})).unwrap();
    let attempt = jobs["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    let report = |seq: i64, outcome: &str| {
        bridge_call(&b, &p, "bridge.report", json!({"attempt_id": attempt, "outcome": outcome, "event_seq": seq})).unwrap()
    };
    assert!(report(1, "held").get("duplicate").is_none());
    assert_eq!(b.status_of(&mid, "bob"), "held");
    assert_eq!(report(1, "held")["duplicate"], true);
    report(2, "delivered");
    assert_eq!(report(2, "delivered")["duplicate"], true);
    assert_eq!(b.status_of(&mid, "bob"), "delivered");
}

/// An outcome that reaches the board after the recipient read the message (the bridge resends
/// an unacknowledged outcome, and the server applies outcomes beside the connection's other
/// frames, so it can land last): the notification stays read and nothing is offered again,
/// whether the read is a DM's read marker or a channel cursor past the message.
#[test]
fn a_late_outcome_never_reopens_a_read_message() {
    for channel in [false, true] {
        for outcome in ["accepted", "held", "delivered", "expired"] {
            let case = format!("{outcome}{}", if channel { " (channel cursor)" } else { "" });
            let b = setup();
            let mid = if channel {
                b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
                b.ok(CAROL, "channel.join", json!({"channel": "#dev"}));
                b.send(ALICE, "#dev", "@carol look")
            } else {
                b.send(ALICE, "carol", "x")
            };
            let p = b.d.bridge_principal(&b.p);
            bridge_call(&b, &p, "bridge.fence", json!({})).unwrap();
            let claim = bridge_call(&b, &p, "bridge.claim", json!({})).unwrap();
            assert_eq!(claim["jobs"].as_array().unwrap().len(), 1, "{case}: {claim}");
            let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
            if channel {
                b.ok(CAROL, "message.read", json!({"channel": "#dev"}));
            } else {
                b.ok(CAROL, "message.read", json!({"id": mid}));
            }
            let r = bridge_call(&b, &p, "bridge.report", json!({"attempt_id": attempt, "outcome": outcome, "event_seq": 1})).unwrap();
            assert_eq!(r["status"], "read", "{case}: the reply says how the notification settled: {r}");
            assert_eq!(b.status_of(&mid, "carol"), "read", "{case}");
            let again = bridge_call(&b, &p, "bridge.claim", json!({"open": true})).unwrap();
            assert_eq!(again["jobs"], json!([]), "{case}: a read message is not offered again");
            assert_eq!(again["open"], json!([]), "{case}: nor reopened on reconnect");
            assert_eq!(again["held"], json!([]), "{case}: nor held");
        }
    }
}

/// The other order: a hold (or a provisional acceptance) lands, then the recipient reads the
/// message. The read settles the notification as read: it is no longer held, and it is not
/// reopened on a reconnect.
#[test]
fn a_read_after_a_hold_settles_the_notification() {
    for channel in [false, true] {
        for outcome in ["held", "accepted"] {
            let case = format!("{outcome}{}", if channel { " (channel cursor)" } else { "" });
            let b = setup();
            let mid = if channel {
                b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
                b.ok(CAROL, "channel.join", json!({"channel": "#dev"}));
                b.send(ALICE, "#dev", "@carol look")
            } else {
                b.send(ALICE, "carol", "x")
            };
            let p = b.d.bridge_principal(&b.p);
            bridge_call(&b, &p, "bridge.fence", json!({})).unwrap();
            let claim = bridge_call(&b, &p, "bridge.claim", json!({})).unwrap();
            let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
            bridge_call(&b, &p, "bridge.report", json!({"attempt_id": attempt, "outcome": outcome, "event_seq": 1})).unwrap();
            assert_ne!(b.status_of(&mid, "carol"), "read", "{case}: not read yet");
            if channel {
                b.ok(CAROL, "message.read", json!({"channel": "#dev"}));
            } else {
                b.ok(CAROL, "message.read", json!({"id": mid}));
            }
            assert_eq!(b.status_of(&mid, "carol"), "read", "{case}");
            let again = bridge_call(&b, &p, "bridge.claim", json!({"open": true})).unwrap();
            assert_eq!(again["jobs"], json!([]), "{case}");
            assert_eq!(again["open"], json!([]), "{case}: not reopened on reconnect");
            assert_eq!(again["held"], json!([]), "{case}: not held");
        }
    }
}

/// The reviewer's case: two channel messages, the older one held for Carol; Carol reads the newer
/// one by id. The cursor now covers both, so the older notification settles as read too, and the
/// change is announced. On the test store (Postgres with CONDE_TEST_PG) and on SQLite, where the
/// announcement is delivered in process (a Postgres command board has no LISTEN connection).
#[test]
fn reading_a_later_channel_message_settles_the_earlier_ones() {
    later_channel_read(setup(), false);
    let b = Board::sqlite();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    b.register(CAROL, "carol");
    later_channel_read(b, true);
}

fn later_channel_read(b: Board, announce: bool) {
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.ok(CAROL, "channel.join", json!({"channel": "#dev"}));
    let m1 = b.send(ALICE, "#dev", "@carol first");
    let p = b.d.bridge_principal(&b.p);
    bridge_call(&b, &p, "bridge.fence", json!({})).unwrap();
    let claim = bridge_call(&b, &p, "bridge.claim", json!({})).unwrap();
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    bridge_call(&b, &p, "bridge.report", json!({"attempt_id": attempt, "outcome": "held", "event_seq": 1})).unwrap();
    let m2 = b.send(ALICE, "#dev", "a later post");
    assert_eq!(b.status_of(&m1, "carol"), "held");
    let mut events = b.d.store().pending().events();
    b.ok(CAROL, "message.read", json!({"id": m2}));
    assert_eq!(b.status_of(&m1, "carol"), "read", "the cursor covers the earlier message");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(if announce { 2000 } else { 0 });
    let announced = loop {
        match events.try_recv() {
            Ok(conde::store::Wake::Board(t)) if t & conde::store::topics::DELIVERY != 0 => break true,
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) if std::time::Instant::now() > deadline => break false,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    assert!(announced || !announce, "the delivery change is announced to the web views");
    let again = bridge_call(&b, &p, "bridge.claim", json!({"open": true})).unwrap();
    assert_eq!((&again["jobs"], &again["open"], &again["held"]), (&json!([]), &json!([]), &json!([])), "{again}");
}

/// The cursor settle is bounded both ways: a notification for a message above the cursor stays
/// open, and one already terminal (delivered, denied) below it keeps its status. On the test
/// store and on SQLite.
#[test]
fn a_channel_cursor_settle_leaves_messages_above_it_open() {
    for b in [setup(), sqlite_setup()] {
        let ([_, _, below, above], settled) = cursor_settle(&b);
        assert_eq!(settled, 1);
        assert_eq!(b.status_of(&below, "carol"), "read", "an open one below the cursor settles");
        assert_eq!(b.status_of(&above, "carol"), "pending", "above the cursor: not read yet");
    }
}

#[test]
fn a_channel_cursor_settle_leaves_terminal_notifications_alone() {
    for b in [setup(), sqlite_setup()] {
        let ([delivered, denied, ..], _) = cursor_settle(&b);
        assert_eq!(b.status_of(&delivered, "carol"), "delivered");
        assert_eq!(b.status_of(&denied, "carol"), "denied");
    }
}

fn sqlite_setup() -> Board {
    let b = Board::sqlite();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    b.register(CAROL, "carol");
    b
}

/// Four posts mentioning Carol in #dev: the first delivered, the second denied, the third and
/// fourth open; settles at the third's seq. Returns the four ids and the number settled.
fn cursor_settle(b: &Board) -> ([String; 4], u64) {
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.ok(CAROL, "channel.join", json!({"channel": "#dev"}));
    let ids: [String; 4] = std::array::from_fn(|i| b.send(ALICE, "#dev", &format!("@carol post {i}")));
    let carol = b.agent_id("carol");
    let settled = b
        .d
        .with_tx(|tx| {
            for n in tx.notifications()?.iter().filter(|n| n.recipient_id == carol) {
                match n.message_id.as_str() {
                    m if m == ids[0] => tx.set_notification_status(&n.id, "delivered")?,
                    m if m == ids[1] => tx.set_notification_status(&n.id, "denied")?,
                    _ => {}
                }
            }
            let cursor = tx.message_by_id(&ids[2])?.unwrap();
            Ok(tx.settle_read_in_channel(cursor.channel_id.as_deref().unwrap(), &carol, cursor.seq)?)
        })
        .unwrap();
    (ids, settled)
}

#[test]
fn fence_makes_one_instance_active_and_open_attempts_follow_it() {
    let b = setup();
    let mid = b.send(ALICE, "carol", "x");
    let old = b.d.bridge_principal(&b.p);
    assert_eq!(bridge_call(&b, &old, "bridge.fence", json!({})).unwrap()["active"], true);
    let claim = bridge_call(&b, &old, "bridge.claim", json!({"open": true})).unwrap();
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    assert_eq!(claim["jobs"][0]["recipient"], "krisz/carol");
    assert_eq!(claim["jobs"][0]["message_id"], mid.as_str());
    assert_eq!(claim["open"][0]["attempt_id"], attempt.as_str(), "in flight stays open");
    bridge_call(&b, &old, "bridge.report", json!({"attempt_id": attempt, "outcome": "held", "event_seq": 1})).unwrap();
    let again = bridge_call(&b, &old, "bridge.claim", json!({"open": true})).unwrap();
    assert_eq!(again["jobs"], json!([]));
    assert_eq!(again["open"][0]["attempt_id"], attempt.as_str(), "held stays open");
    assert!(again["open"][0]["text"].as_str().unwrap().contains(&mid));

    let new = b.d.bridge_principal(&b.p);
    let r = bridge_call(&b, &new, "bridge.fence", json!({})).unwrap();
    assert_eq!(r["fenced"], json!([old.bridge_instance.clone().unwrap()]));
    // The fenced instance can neither claim nor come back.
    assert!(bridge_call(&b, &old, "bridge.claim", json!({})).unwrap_err().1.contains("fenced"));
    assert_eq!(bridge_call(&b, &old, "bridge.fence", json!({})).unwrap()["active"], false);
    // The new instance abandons the old hold and re-attempts it exactly once.
    let claim = bridge_call(&b, &new, "bridge.claim", json!({"open": true})).unwrap();
    assert_eq!(claim["jobs"].as_array().unwrap().len(), 1);
    assert_ne!(claim["jobs"][0]["attempt_id"], attempt.as_str());
    let old_report = json!({"attempt_id": attempt, "outcome": "delivered", "event_seq": 2});
    let r = bridge_call(&b, &old, "bridge.report", old_report).unwrap();
    assert_eq!(r["ignored"], "attempt already settled as abandoned");
    assert_eq!(bridge_call(&b, &new, "bridge.claim", json!({}))
        .unwrap()["jobs"], json!([]));
}

#[test]
fn endpoints_abandon_holds_on_sessions_that_stopped() {
    let b = setup();
    let mid = b.send(ALICE, "carol", "x");
    let p = b.d.bridge_principal(&b.p);
    bridge_call(&b, &p, "bridge.fence", json!({})).unwrap();
    let claim = bridge_call(&b, &p, "bridge.claim", json!({})).unwrap();
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    bridge_call(&b, &p, "bridge.report", json!({"attempt_id": attempt, "outcome": "held"})).unwrap();
    let live = json!({"endpoints": [{"kind": "claude", "target": "s-carol"}]});
    let r = bridge_call(&b, &p, "bridge.endpoints", live).unwrap();
    assert_eq!(r["abandoned"], json!([]));
    assert_eq!(b.status_of(&mid, "carol"), "held");
    let r = bridge_call(&b, &p, "bridge.endpoints", json!({"endpoints": []})).unwrap();
    assert_eq!(r["abandoned"], json!([attempt]));
    assert_eq!(b.status_of(&mid, "carol"), "pending");
    let stored = b.d.with_conn(|tx| tx.count_endpoints()).unwrap();
    assert_eq!(stored, 0);
}

#[test]
fn endpoints_leave_codex_holds_alone() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let p = b.d.bridge_principal(&b.p);
    let claim = bridge_call(&b, &p, "bridge.claim", json!({})).unwrap();
    let attempt = claim["jobs"][0]["attempt_id"].as_str().unwrap().to_string();
    bridge_call(&b, &p, "bridge.report", json!({"attempt_id": attempt, "outcome": "held", "reason": "queued"})).unwrap();
    let r = bridge_call(&b, &p, "bridge.endpoints", json!({"endpoints": []})).unwrap();
    assert_eq!(r["abandoned"], json!([]));
    assert_eq!(b.status_of(&mid, "bob"), "held");
}

/// A board on disk whose connection gives up on a write lock after 10 ms.
fn board_on_disk(dir: &std::path::Path) -> Board {
    std::fs::create_dir_all(dir).unwrap();
    let d = std::sync::Arc::new(conde::dispatch::Dispatcher::open(&dir.join("board.db")).unwrap());
    d.sqlite().unwrap().with_connection(|c| c.busy_timeout(std::time::Duration::from_millis(10)).unwrap());
    let p = d.principal("krisz", "box", true).unwrap();
    Board { d, p }
}

fn write_lock(dir: &std::path::Path) -> rusqlite::Connection {
    let lock = rusqlite::Connection::open(dir.join("board.db")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    lock
}

#[test]
fn a_write_lock_delays_a_remote_outcome_but_never_rejects_it() {
    use conde_remote::{NotificationSource, Outcome, Recorded};
    let dir = std::env::temp_dir().join(format!("cde-lock-{}", &conde::util::new_ulid()[14..]));
    let b = board_on_disk(&dir);
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let mid = b.send(ALICE, "bob", "receipt");
    let mut bridge = b.bridge(FakeNotifier::default());
    bridge.pass().unwrap();
    let outcome = Outcome { attempt_id: bridge.notifier().last_attempt(), event_seq: 1, status: "delivered".into(), reason: None };

    // Released while the board retries: recorded, durable.
    let remote = conde::cloud::Board::new(b.d.clone());
    let lock = write_lock(&dir);
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        lock.execute_batch("ROLLBACK").unwrap();
    });
    assert_eq!(remote.record_outcome("box", bridge.instance(), &outcome), Recorded::Accepted);
    release.join().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "delivered");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_outcome_the_store_could_not_take_is_answered_retry_for_the_resend() {
    use conde_remote::{NotificationSource, Outcome, Recorded};
    let dir = std::env::temp_dir().join(format!("cde-lock-{}", &conde::util::new_ulid()[14..]));
    let b = board_on_disk(&dir);
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let mid = b.send(ALICE, "bob", "receipt");
    let mut bridge = b.bridge(FakeNotifier::default());
    bridge.pass().unwrap();
    let outcome = Outcome { attempt_id: bridge.notifier().last_attempt(), event_seq: 1, status: "delivered".into(), reason: None };
    let remote = conde::cloud::Board::new(b.d.clone()).with_transient_retry(std::time::Duration::from_millis(200));

    // Locked for longer than the retry: answered Retry (the bridge keeps the outcome
    // and resends it), never a final rejection.
    let lock = write_lock(&dir);
    let answer = remote.record_outcome("box", bridge.instance(), &outcome);
    assert!(matches!(answer, Recorded::Retry(_)), "a storage failure was answered {answer:?}");
    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "notified");

    // The bridge's resend lands.
    assert_eq!(remote.record_outcome("box", bridge.instance(), &outcome), Recorded::Accepted);
    assert_eq!(b.status_of(&mid, "bob"), "delivered");
    // A refusal stays final and answered.
    let foreign = Outcome { attempt_id: "nope".into(), ..outcome };
    assert!(matches!(remote.record_outcome("box", bridge.instance(), &foreign), Recorded::Rejected(_)));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Interrupt the `nth` SELECT the board prepares (through SQLite's progress
/// handler), once; returns the count of interruptions that happened.
fn interrupt_nth_select<T>(b: &Board, nth: usize, f: impl FnOnce() -> T) -> (T, usize) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let selects = Arc::new(AtomicUsize::new(0));
    let interrupted = Arc::new(AtomicUsize::new(0));
    b.d.sqlite().expect("a SQLite board").with_connection(|c| {
        let n = selects.clone();
        c.authorizer(Some(move |ctx: AuthContext<'_>| {
            if matches!(ctx.action, AuthAction::Select) {
                n.fetch_add(1, Ordering::SeqCst);
            }
            Authorization::Allow
        }))
        .unwrap();
        let (n, i) = (selects.clone(), interrupted.clone());
        c.progress_handler(1, Some(move || {
            // fire once, on the nth SELECT
            n.load(Ordering::SeqCst) == nth && i.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
        }))
        .unwrap();
    });
    let out = f();
    b.d.sqlite().unwrap().with_connection(|c| {
        c.progress_handler(0, None::<fn() -> bool>).unwrap();
        c.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
    });
    (out, interrupted.load(std::sync::atomic::Ordering::SeqCst))
}

#[test]
fn an_interrupted_owner_or_attempt_read_is_retried_not_rejected() {
    use conde_remote::{NotificationSource, Outcome, Recorded};
    let b = Board::sqlite();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    let remote = conde::cloud::Board::new(b.d.clone());
    // 1: the host owner read; 2: the first read inside bridge.report.
    for nth in [1, 2] {
        let mid = b.send(ALICE, "bob", &format!("receipt {nth}"));
        let mut bridge = b.bridge(FakeNotifier::default());
        bridge.pass().unwrap();
        let outcome = Outcome { attempt_id: bridge.notifier().last_attempt(), event_seq: 1, status: "delivered".into(), reason: None };
        let (result, interrupted) =
            interrupt_nth_select(&b, nth, || remote.record_outcome("box", bridge.instance(), &outcome));
        assert_eq!(interrupted, 1, "select {nth} was interrupted");
        assert_eq!(result, Recorded::Accepted, "select {nth}");
        assert_eq!(b.status_of(&mid, "bob"), "delivered", "select {nth}");
    }
}

/// Requests carried by a bridge connection are admitted only while its instance
/// is the host's active one: after a takeover the old connection's are refused.
#[test]
fn a_fenced_bridge_connection_takes_no_requests() {
    use conde_remote::Dispatcher as _;
    let b = setup();
    let (old, new) = (b.d.bridge_principal(&b.p), b.d.bridge_principal(&b.p));
    let board = conde::cloud::Board::new(b.d.clone());
    let principal = conde_remote::Principal { user_id: b.p.user_id.to_string(), host_id: b.p.host.clone() };
    let frame = |name: &str| {
        let mut req = conde::protocol::Request::new(name, "agent.register", json!({"name": name}));
        req.actor = Some(format!("codex:t-{name}"));
        req.idempotency_key = Some(name.into());
        serde_json::to_value(req).unwrap()
    };
    bridge_call(&b, &old, "bridge.fence", json!({})).unwrap();
    let reply = board.dispatch_bridge(&principal, old.bridge_instance.as_deref().unwrap(), frame("early"));
    assert_eq!(reply["ok"], true, "{reply}");
    bridge_call(&b, &new, "bridge.fence", json!({})).unwrap();
    let reply = board.dispatch_bridge(&principal, old.bridge_instance.as_deref().unwrap(), frame("late"));
    assert_eq!(reply["error"]["code"], "forbidden", "{reply}");
    let reply = board.dispatch_bridge(&principal, new.bridge_instance.as_deref().unwrap(), frame("current"));
    assert_eq!(reply["ok"], true, "{reply}");
    let r = b.call(HUMAN, "agent.list", json!({}));
    let names = r.map(|v| v.to_string()).unwrap_or_else(|e| format!("{e:?}"));
    assert!(names.contains("krisz/early") && names.contains("krisz/current") && !names.contains("krisz/late"), "{names}");
}
