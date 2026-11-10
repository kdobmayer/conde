mod common;

use common::*;
use conde::dispatch::NOTIFY_RETRY_MS;
use conde::notify::{NotifyOutcome, ReceiptStatus};
use serde_json::json;

fn setup() -> Board {
    let b = Board::new();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    b.register(CAROL, "carol");
    b.register(HUMAN, "krisz");
    b
}

// --- who gets pushed ---------------------------------------------------------

#[test]
fn dms_push_with_a_pointer_text() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "hello");
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    let sent = &br.notifier().sent;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "codex:t-bob@box");
    assert_eq!(
        sent[0].1,
        format!("conde: new message {mid} from krisz/alice. Read it with: conde read {mid}")
    );
    assert_eq!(b.status_of(&mid, "bob"), "notified");
    // nothing more on the next pass
    br.pass().unwrap();
    assert_eq!(br.notifier().sent.len(), 1);
}

#[test]
fn plain_channel_posts_never_push_but_badge() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.ok(BOB, "channel.join", json!({"channel": "#dev"}));
    b.send(ALICE, "#dev", "plain post");
    b.send(BOB, "#dev", "another plain post");
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    assert!(br.notifier().sent.is_empty(), "{:?}", br.notifier().sent);
    // the badge (bob's join post + his post) rides on the next push
    let mid = b.send(CAROL, "alice", "dm");
    br.pass().unwrap();
    assert_eq!(
        br.notifier().sent[0].1,
        format!(
            "conde: new message {mid} from krisz/carol. Read it with: conde read {mid} | Also unread: #dev 2"
        )
    );
}

#[test]
fn mentions_push_members_and_non_members() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.ok(
        ALICE,
        "channel.invite",
        json!({"name": "bob", "channel": "#dev"}),
    );
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    // the invite post mentions the invitee: exactly one push
    assert_eq!(br.notifier().sent.len(), 1);
    assert!(
        br.notifier().sent[0]
            .1
            .contains("you were mentioned in #dev by krisz/alice")
    );
    br.notifier().sent.clear();

    let mid = b.send(
        ALICE,
        "#dev",
        "@Bob and @carol please look; @alice is me; @nobody is nobody",
    );
    br.pass().unwrap();
    let sent = &br.notifier().sent;
    assert_eq!(sent.len(), 2, "{sent:?}");
    let bob = &br.notifier().sent_to("codex:t-bob")[0].1;
    let expected = format!(
        "conde: you were mentioned in #dev by krisz/alice (message {mid}). Read it with: conde read '#dev'"
    );
    assert_eq!(bob, &format!("{expected} | Also unread: #dev 2"));
    let carol = &br.notifier().sent_to("claude:s-carol")[0].1;
    assert!(
        carol.ends_with("after joining: conde join '#dev'"),
        "{carol}"
    );
}

#[test]
fn everybody_mentions_all_members() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.ok(BOB, "channel.join", json!({"channel": "#dev"}));
    b.ok(CAROL, "channel.join", json!({"channel": "#dev"}));
    let mid = b.send(ALICE, "#dev", "@everybody standup");
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    let mut to: Vec<_> = br.notifier().sent.iter().map(|s| s.0.clone()).collect();
    to.sort();
    assert_eq!(to, ["claude:s-carol@box", "codex:t-bob@box"]);
    assert_eq!(b.status_of(&mid, "alice"), "none");
}

#[test]
fn a_humans_channel_post_pushes_every_member() {
    let b = setup();
    b.ok(HUMAN, "channel.create", json!({"channel": "#ops"}));
    b.ok(
        HUMAN,
        "channel.invite",
        json!({"name": "alice", "channel": "#ops"}),
    );
    b.ok(
        HUMAN,
        "channel.invite",
        json!({"name": "bob", "channel": "#ops"}),
    );
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    br.notifier().sent.clear();
    let mid = b.send(HUMAN, "#ops", "everyone: stop deploying");
    br.pass().unwrap();
    let sent = &br.notifier().sent;
    assert_eq!(sent.len(), 2);
    let expected = format!(
        "conde: krisz/krisz posted in #ops (message {mid}). Read it with: conde read '#ops' | Also unread: #ops"
    );
    assert!(sent.iter().all(|s| s.1.starts_with(&expected)), "{sent:?}");
    // humans themselves are badge-only
    let dm = b.send(ALICE, "krisz", "for the human");
    br.pass().unwrap();
    assert_eq!(br.notifier().sent.len(), 2);
    assert_eq!(b.status_of(&dm, "krisz"), "badge");
}

#[test]
fn reactions_and_removals_never_push() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    br.notifier().sent.clear();
    b.ok(
        BOB,
        "reaction.set",
        json!({"message_id": mid, "emoji": "👍", "present": true}),
    );
    b.ok(
        BOB,
        "reaction.set",
        json!({"message_id": mid, "emoji": "👍", "present": false}),
    );
    br.pass().unwrap();
    assert!(br.notifier().sent.is_empty());
    assert_eq!(b.notifications().len(), 1);
}

#[test]
fn read_before_push_is_not_pushed() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    b.ok(BOB, "message.read", json!({"id": mid}));
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    assert!(br.notifier().sent.is_empty());
    assert_eq!(b.status_of(&mid, "bob"), "read");
}

#[test]
fn unbound_recipient_waits_for_registration() {
    let b = setup();
    b.ok(BOB, "agent.unregister", json!({"name": "bob"}));
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    assert!(br.notifier().sent.is_empty());
    b.register("codex:t-bob2", "bob");
    br.pass().unwrap();
    assert_eq!(br.notifier().sent[0].0, "codex:t-bob2@box");
    assert_eq!(b.status_of(&mid, "bob"), "notified");
}

// --- receipts state machine ------------------------------------------------

#[test]
fn held_then_delivered() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Held].into(),
        ..Default::default()
    });
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "held");
    br.pass().unwrap();
    assert_eq!(br.notifier().sent.len(), 1, "no resend while held");
    let attempt = br.notifier().last_attempt();
    br.notifier().receipt(&attempt, ReceiptStatus::Delivered);
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "delivered");
}

#[test]
fn late_held_revises_provisional_acceptance_then_expired_retries_after_backoff() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "notified", "provisional");
    let first = br.notifier().last_attempt();
    br.notifier().receipt(&first, ReceiptStatus::Held);
    br.pass().unwrap();
    assert_eq!(
        b.status_of(&mid, "bob"),
        "held",
        "provisional acceptance withdrawn"
    );
    br.notifier().receipt(&first, ReceiptStatus::Expired);
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "pending");
    assert_eq!(br.notifier().sent.len(), 1, "no resend inside the backoff");
    b.d.advance_clock(NOTIFY_RETRY_MS + 1);
    br.pass().unwrap();
    assert_eq!(br.notifier().sent.len(), 2);
    assert_ne!(br.notifier().last_attempt(), first, "a fresh attempt id");
    assert_eq!(b.status_of(&mid, "bob"), "notified");
}

#[test]
fn failed_and_dropped_retry_but_duplicate_drop_is_ignored() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Failed("socket gone".into())].into(),
        ..Default::default()
    });
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "pending");
    b.d.advance_clock(NOTIFY_RETRY_MS + 1);
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "notified");
    let attempt = br.notifier().last_attempt();
    br.notifier().receipt(
        &attempt,
        ReceiptStatus::Dropped {
            reason: "duplicate".into(),
        },
    );
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "notified");
    br.notifier().receipt(
        &attempt,
        ReceiptStatus::Dropped {
            reason: "queue full".into(),
        },
    );
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "pending");
}

#[test]
fn denied_and_refused_are_terminal() {
    for status in [ReceiptStatus::Denied, ReceiptStatus::Refused] {
        let b = setup();
        let mid = b.send(ALICE, "bob", "x");
        let mut br = b.bridge(FakeNotifier {
            outcomes: [NotifyOutcome::Held].into(),
            ..Default::default()
        });
        br.pass().unwrap();
        let attempt = br.notifier().last_attempt();
        br.notifier().receipt(&attempt, status.clone());
        br.pass().unwrap();
        let expected = if status == ReceiptStatus::Denied {
            "denied"
        } else {
            "refused"
        };
        assert_eq!(b.status_of(&mid, "bob"), expected);
        // an older held update never reopens it
        br.notifier().receipt(&attempt, ReceiptStatus::Held);
        b.d.advance_clock(10 * NOTIFY_RETRY_MS);
        br.pass().unwrap();
        assert_eq!(b.status_of(&mid, "bob"), expected);
        assert_eq!(br.notifier().sent.len(), 1, "never resent");
    }
}

#[test]
fn hold_from_a_dead_bridge_instance_is_abandoned_and_resent() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut old = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Held].into(),
        ..Default::default()
    });
    old.pass().unwrap();
    let old_attempt = old.notifier().last_attempt();
    assert_eq!(b.status_of(&mid, "bob"), "held");
    drop(old); // daemon restart: a new bridge instance
    let mut new = b.bridge(FakeNotifier::default());
    new.pass().unwrap();
    assert_eq!(
        new.notifier().sent.len(),
        1,
        "stale hold must not block the resend"
    );
    let new_attempt = new.notifier().last_attempt();
    assert_ne!(new_attempt, old_attempt);
    assert_eq!(b.status_of(&mid, "bob"), "notified");
    // late receipts for the old attempt settle nothing
    new.notifier()
        .receipt(&old_attempt, ReceiptStatus::Delivered);
    new.notifier().receipt(&old_attempt, ReceiptStatus::Denied);
    new.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "notified");
}

#[test]
fn hold_on_a_gone_session_is_abandoned_and_resent() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Held].into(),
        ..Default::default()
    });
    br.pass().unwrap();
    br.notifier().dead.push("codex:t-bob@box".into());
    br.pass().unwrap(); // abandons; the re-attempt is due now
    br.notifier().dead.clear();
    br.pass().unwrap();
    assert_eq!(br.notifier().sent.len(), 2);
    assert_eq!(b.status_of(&mid, "bob"), "notified");
}

#[test]
fn hold_on_a_rebound_name_is_abandoned_and_sent_to_the_new_session() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Held].into(),
        ..Default::default()
    });
    br.pass().unwrap();
    b.register("codex:t-bob-new", "bob");
    br.pass().unwrap();
    assert_eq!(br.notifier().sent.len(), 2);
    assert_eq!(br.notifier().sent[1].0, "codex:t-bob-new@box");
    assert_eq!(b.status_of(&mid, "bob"), "notified");
}

#[test]
fn out_of_order_late_receipts_for_two_attempts() {
    let b = setup();
    let mid = b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Held].into(),
        ..Default::default()
    });
    br.pass().unwrap();
    let a1 = br.notifier().last_attempt();
    br.notifier().receipt(&a1, ReceiptStatus::Expired);
    br.pass().unwrap();
    b.d.advance_clock(NOTIFY_RETRY_MS + 1);
    br.pass().unwrap();
    let a2 = br.notifier().last_attempt();
    assert_ne!(a1, a2);
    // a2's receipts arrive out of order: delivered first, then its earlier held
    br.notifier().receipt(&a2, ReceiptStatus::Delivered);
    br.notifier().receipt(&a2, ReceiptStatus::Held);
    // and a1's stale receipts arrive late too
    br.notifier().receipt(&a1, ReceiptStatus::Held);
    br.notifier().receipt(&a1, ReceiptStatus::Delivered);
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "delivered");
    let statuses: Vec<(String, String)> =
        b.d.with_conn(|tx| tx.attempts())
            .unwrap()
            .into_iter()
            .map(|a| (a.id, a.status))
            .collect();
    assert_eq!(
        statuses,
        [(a1, "expired".to_string()), (a2, "delivered".to_string())]
    );
}

#[test]
fn channel_fanout_attempts_settle_only_their_recipient() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    let mid = b.send(ALICE, "#dev", "@bob @carol review please");
    let mut br = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Held, NotifyOutcome::Held].into(),
        ..Default::default()
    });
    br.pass().unwrap();
    let ids: Vec<String> = br.notifier().sent.iter().map(|s| s.2.clone()).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    let bob_attempt = br.notifier().sent_to("codex:t-bob")[0].2.clone();
    br.notifier()
        .receipt(&bob_attempt, ReceiptStatus::Delivered);
    br.pass().unwrap();
    assert_eq!(b.status_of(&mid, "bob"), "delivered");
    assert_eq!(b.status_of(&mid, "carol"), "held");
}

// --- the per-membership notify setting ----------------------------------------

/// (recipient name, kind) of every notification of `mid`.
fn kinds(b: &Board, mid: &str) -> Vec<(String, String)> {
    b.notifications()
        .into_iter()
        .filter_map(|(k, kind, _)| {
            k.strip_prefix(&format!("{mid}."))
                .map(|name| (name.to_string(), kind))
        })
        .collect()
}

#[test]
fn an_opted_in_member_is_pushed_every_post_and_a_default_member_is_not() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    let joined = b.ok(
        BOB,
        "channel.join",
        json!({"channel": "#dev", "notify": "all"}),
    );
    assert_eq!(joined["notify"], "all");
    b.ok(CAROL, "channel.join", json!({"channel": "#dev"}));
    let post = b.send(ALICE, "#dev", "standup: shipped the parser");
    assert_eq!(kinds(&b, &post), [("bob".to_string(), "post".to_string())]);
    // the opted-in member's own post never comes back to it
    let own = b.send(BOB, "#dev", "standup: reviewing");
    assert!(kinds(&b, &own).is_empty(), "{:?}", kinds(&b, &own));
    // a mention stays a mention, for the opted-in member too
    let mention = b.send(ALICE, "#dev", "@bob @carol please review");
    assert_eq!(
        kinds(&b, &mention),
        [
            ("bob".to_string(), "mention".to_string()),
            ("carol".to_string(), "mention".to_string())
        ]
    );
    let mut br = b.bridge(FakeNotifier::default());
    br.pass().unwrap();
    let bob = br.notifier().sent_to("codex:t-bob");
    let text = format!(
        "conde: krisz/alice posted in #dev (message {post}). Read it with: conde read '#dev'"
    );
    assert!(bob.iter().any(|s| s.1.starts_with(&text)), "{bob:?}");
}

#[test]
fn switching_back_to_mentions_stops_the_posts() {
    let b = setup();
    b.ok(ALICE, "channel.create", json!({"channel": "#dev"}));
    b.ok(BOB, "channel.join", json!({"channel": "#dev"}));
    let r = b.ok(
        BOB,
        "channel.notify",
        json!({"channel": "#dev", "notify": "all"}),
    );
    assert_eq!(r, json!({"channel": "#dev", "notify": "all"}));
    let on = b.send(ALICE, "#dev", "first");
    assert_eq!(kinds(&b, &on), [("bob".to_string(), "post".to_string())]);
    b.ok(
        BOB,
        "channel.notify",
        json!({"channel": "#dev", "notify": "mentions"}),
    );
    let off = b.send(ALICE, "#dev", "second");
    assert!(kinds(&b, &off).is_empty(), "{:?}", kinds(&b, &off));
}

#[test]
fn a_humans_post_is_one_post_push_for_an_opted_in_member() {
    let b = setup();
    b.ok(HUMAN, "channel.create", json!({"channel": "#ops"}));
    b.ok(
        ALICE,
        "channel.join",
        json!({"channel": "#ops", "notify": "all"}),
    );
    b.ok(BOB, "channel.join", json!({"channel": "#ops"}));
    let mid = b.send(HUMAN, "#ops", "stop deploying, @bob");
    assert_eq!(
        kinds(&b, &mid),
        [
            ("alice".to_string(), "post".to_string()),
            ("bob".to_string(), "mention".to_string())
        ]
    );
}

/// The fleet's delivery health, read on every board event of an open browser, never sorts a notification's retries.
#[test]
fn fleet_health_reads_no_retry_history() {
    let Some(url) = pg::fresh_database() else {
        return pg::skip("the retry history on Postgres");
    };
    let open = || {
        let d = std::sync::Arc::new(
            conde::dispatch::Dispatcher::connect(&url, conde::store::postgres::PgOpen::COMMAND)
                .unwrap(),
        );
        let p = d.principal("krisz", "box", true).unwrap();
        Board { d, p }
    };
    let b = open();
    b.register(ALICE, "alice");
    b.register(BOB, "bob");
    b.send(ALICE, "bob", "x");
    let mut br = b.bridge(FakeNotifier {
        outcomes: [NotifyOutcome::Failed("socket gone".into())].into(),
        ..Default::default()
    });
    br.pass().unwrap();
    drop((br, b));
    // days of retries every 30 s; a sort of them all spills past work_mem, and the limit makes that an error
    let db = url.rsplit("dbname=").next().unwrap();
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    client
        .batch_execute(&format!(
            "INSERT INTO attempts(attempt_id, notification_id, bridge_instance, target_binding_version, status)
                 SELECT 'retry' || g, notification_id, bridge_instance, target_binding_version, 'failed'
                 FROM attempts, generate_series(1, 50000) g;
             ALTER DATABASE {db} SET work_mem = '64kB';
             ALTER DATABASE {db} SET temp_file_limit = '1MB';"
        ))
        .unwrap();
    let b = open();
    let browser = conde::dispatch::Principal {
        host: "browser".into(),
        local: false,
        browser: true,
        ..b.p.clone()
    };
    let resp = b.d.dispatch(
        &browser,
        &conde::protocol::Request::new(conde::util::new_ulid(), "fleet.list", json!({})),
    );
    assert!(resp.ok, "fleet.list: {:?}", resp.error);
    let fleet = resp.result.unwrap();
    let bob = fleet["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "krisz/bob")
        .unwrap();
    assert_eq!(bob["ever_reached"], false, "{bob}");
}
