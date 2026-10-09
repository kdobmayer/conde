//! `/install.sh` and `/dist/<file>` on the cloud listener: a no-store 302 into
//! the release container for one plain file name, 404 for anything else.

use std::sync::Arc;

use conde::dispatch::Dispatcher;
use conde_remote::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn get(addr: std::net::SocketAddr, method: &str, path: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: conde.test\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.to_lowercase()
}

async fn server(base: Option<&str>) -> conde_remote::ServerHandle {
    let d = Arc::new(Dispatcher::open_in_memory().unwrap());
    let config = ServerConfig { dist_base_url: base.map(Into::into), ..ServerConfig::default() };
    conde::cloud::start(d, "127.0.0.1:0".parse().unwrap(), config).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn release_files_redirect_to_the_container() {
    let h = server(Some("https://stconde.blob.core.windows.net/dist/")).await;
    let addr = h.local_addr();
    for (path, file) in [
        ("/install.sh", "install.sh"),
        ("/dist/conde-darwin-arm64", "conde-darwin-arm64"),
        ("/dist/conde-linux-x86_64.sha256", "conde-linux-x86_64.sha256"),
        ("/dist/latest", "latest"),
    ] {
        for method in ["GET", "HEAD"] {
            let r = get(addr, method, path).await;
            assert!(r.starts_with("http/1.1 302"), "{method} {path}: {r}");
            assert!(r.contains(&format!("\r\nlocation: https://stconde.blob.core.windows.net/dist/{file}\r\n")), "{r}");
            assert!(r.contains("\r\ncache-control: no-store\r\n"), "{r}");
        }
    }
    for path in ["/dist/..", "/dist/.env", "/dist/a%2Fb", "/dist/a%2F..%2Fb", "/dist/x%00", "/dist/", "/dist/a/b"] {
        let r = get(addr, "GET", path).await;
        assert!(r.starts_with("http/1.1 404"), "{path}: {r}");
        assert!(!r.contains("location:"), "{path}: {r}");
    }
    h.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_dist_base_url_there_is_nothing_to_serve() {
    let h = server(None).await;
    for path in ["/install.sh", "/dist/latest"] {
        let r = get(h.local_addr(), "GET", path).await;
        assert!(r.starts_with("http/1.1 404"), "{path}: {r}");
    }
    h.shutdown().await.unwrap();
}
