//! `/readme` (rendered) and `/readme.md` (raw): the setup runbook for teammates
//! without repository access, public and never cached.

use std::sync::Arc;

use conde_remote::{
    Authenticator, Dispatcher, FenceResult, NotificationSource, Notify, Outcome, Principal,
    Recorded, ServerConfig,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Nobody;

impl Authenticator for Nobody {
    fn authenticate(&self, _token: &str) -> Option<Principal> {
        None
    }
}

impl Dispatcher for Nobody {
    fn dispatch(&self, _p: &Principal, _r: serde_json::Value) -> serde_json::Value {
        serde_json::Value::Null
    }
}

impl NotificationSource for Nobody {
    fn pending_for_host(&self, _host: &str, _instance: &str) -> Vec<Notify> {
        Vec::new()
    }

    fn record_outcome(&self, _host: &str, _instance: &str, _outcome: &Outcome) -> Recorded {
        Recorded::Accepted
    }

    fn fence(&self, _host: &str, _instance: &str) -> FenceResult {
        FenceResult::Active
    }
}

const README: &str = include_str!("../../../README.md");

/// (status line + headers lowercased, body as sent).
async fn get(addr: std::net::SocketAddr, path: &str) -> (String, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: conde.test\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    (head.to_lowercase(), body.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn readme_is_served_raw_and_rendered() {
    let n = Arc::new(Nobody);
    let h = conde_remote::start(
        "127.0.0.1:0".parse().unwrap(),
        ServerConfig::default(),
        n.clone(),
        n.clone(),
        n,
    )
    .await
    .unwrap();
    let addr = h.local_addr();

    let (head, body) = get(addr, "/readme.md").await;
    assert!(head.starts_with("http/1.1 200"), "{head}");
    assert!(
        head.contains("\r\ncontent-type: text/markdown; charset=utf-8"),
        "{head}"
    );
    assert!(head.contains("\r\ncache-control: no-store"), "{head}");
    assert_eq!(body, README, "the raw file, byte for byte");
    assert!(
        body.contains("https://conde.nativeai.space/readme.md"),
        "the README points agents at this copy"
    );

    let (head, body) = get(addr, "/readme").await;
    assert!(head.starts_with("http/1.1 200"), "{head}");
    assert!(
        head.contains("\r\ncontent-type: text/html; charset=utf-8"),
        "{head}"
    );
    assert!(head.contains("\r\ncache-control: no-store"), "{head}");
    assert!(
        head.contains("script-src 'none'"),
        "no script on a public page: {head}"
    );
    assert!(
        body.contains("<title>conde setup · conde</title>"),
        "{body}"
    );
    assert!(
        body.contains("<h1>conde</h1>") && body.contains("<h2>Setup</h2>"),
        "headings render"
    );
    assert!(
        body.contains("<table>") && body.contains("<th>Message</th>"),
        "the doctor table renders"
    );
    assert!(body.contains("<pre><code class=\"language-sh\">curl -fsSL https://conde.nativeai.space/install.sh | sh"), "code blocks render");
    assert!(
        body.contains("<!-- You found the comment."),
        "the hidden comment stays a comment"
    );
    assert!(!body.contains("```"), "no raw fences left");

    h.shutdown().await.unwrap();
}
