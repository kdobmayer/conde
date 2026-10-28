#![allow(dead_code)]
//! A fake dist bucket over HTTP: `latest`, `v<version>/<binary>` and `v<version>/SHA256SUMS`
//! as the release workflow uploads them, served from a temp directory, for install.sh and
//! `conde update` alike.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sha2::Digest;

/// A conde binary at `to` that `conde update` may replace: a hard link to this build (a rename
/// over it replaces the link, never the build), or a copy across filesystems.
pub fn place_conde(to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    let build = env!("CARGO_BIN_EXE_conde");
    if std::fs::hard_link(build, to).is_err() {
        std::fs::copy(build, to).unwrap();
    }
}

/// A temp directory next to the build, so [`place_conde`] can link instead of copying it.
pub fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

/// The release file this test machine installs.
pub const NAME: &str = "conde-linux-x86_64";

/// A fake conde that answers `--version` like a release build.
pub fn fake_binary(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho 'conde {version} (fake000 2026-10-28)'\n").into_bytes()
}

pub fn sha256(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub struct Bucket {
    pub root: PathBuf,
    pub url: String,
    gates: Gates,
}

type Gates = Arc<Mutex<HashMap<String, Arc<Gate>>>>;

/// A held file: its request waits until [`Gate::open`].
#[derive(Default)]
pub struct Gate {
    arrived: std::sync::atomic::AtomicUsize,
    open: AtomicBool,
}

impl Gate {
    /// Waits until the held file was requested; panics after `limit`.
    pub fn wait_arrived(&self, limit: Duration) {
        self.wait_requests(1, limit);
    }

    /// Waits until `n` requests for the held file are waiting; panics after `limit`.
    pub fn wait_requests(&self, n: usize, limit: Duration) {
        let end = Instant::now() + limit;
        while self.arrived.load(Ordering::SeqCst) < n {
            assert!(Instant::now() < end, "the held file was never requested");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct Served {
    root: PathBuf,
    gates: Gates,
}

impl Bucket {
    /// Serves `root` on a loopback port until the test process ends.
    pub fn serve(root: &Path) -> Bucket {
        std::fs::create_dir_all(root).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let gates: Gates = Arc::default();
        let served = Served {
            root: root.to_path_buf(),
            gates: gates.clone(),
        };
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let app = axum::Router::new()
                    .route("/{*file}", axum::routing::get(file))
                    .with_state(served);
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        Bucket {
            root: root.to_path_buf(),
            url,
            gates,
        }
    }

    /// Holds `path` (e.g. `v99.0.0/conde-linux-x86_64`): its request waits for the gate.
    pub fn hold(&self, path: &str) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.gates
            .lock()
            .unwrap()
            .insert(path.to_string(), gate.clone());
        gate
    }

    /// `v<version>/<NAME>` with `bytes`, and a SHA256SUMS line for it: its true sum, or a wrong
    /// one when `good_sum` is false.
    pub fn release(&self, version: &str, bytes: &[u8], good_sum: bool) {
        let dir = self.root.join(format!("v{version}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(NAME), bytes).unwrap();
        let sum = if good_sum {
            sha256(bytes)
        } else {
            sha256(b"something else")
        };
        std::fs::write(dir.join("SHA256SUMS"), format!("{sum}  {NAME}\n")).unwrap();
    }

    pub fn latest(&self, version: &str) {
        std::fs::write(self.root.join("latest"), format!("v{version}\n")).unwrap();
    }
}

async fn file(State(served): State<Served>, UrlPath(path): UrlPath<String>) -> impl IntoResponse {
    if path.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return Err(StatusCode::NOT_FOUND);
    }
    let gate = served.gates.lock().unwrap().get(&path).cloned();
    if let Some(gate) = gate {
        gate.arrived.fetch_add(1, Ordering::SeqCst);
        while !gate.open.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    std::fs::read(served.root.join(path)).map_err(|_| StatusCode::NOT_FOUND)
}
