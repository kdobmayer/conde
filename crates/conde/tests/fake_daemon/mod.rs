#![allow(dead_code)]
//! A fake `conde daemon` on the CLI's transport (a unix socket, or on Windows the daemon's own
//! named pipe): records every request frame and answers with a scripted result per op, so the
//! CLI and MCP surfaces are tested against the daemon contract before the daemon implements it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// `Ok(result)` or `Err((code, message))` for one request frame.
pub type Answer = Result<Value, (&'static str, String)>;
type Responder = dyn Fn(&Value) -> Answer + Send + Sync;

pub struct FakeDaemon {
    pub socket: PathBuf,
    log: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeDaemon {
    pub fn start(
        socket: &Path,
        respond: impl Fn(&Value) -> Answer + Send + Sync + 'static,
    ) -> FakeDaemon {
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let respond: Arc<Responder> = Arc::new(respond);
        let thread = listen(socket, log.clone(), stop.clone(), respond);
        FakeDaemon {
            socket: socket.to_path_buf(),
            log,
            stop,
            thread: Some(thread),
        }
    }

    /// Every request frame received so far, in order.
    pub fn requests(&self) -> Vec<Value> {
        self.log.lock().unwrap().clone()
    }

    /// The request frames received so far, then forget them.
    pub fn take(&self) -> Vec<Value> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

#[cfg(unix)]
fn listen(
    socket: &Path,
    log: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    respond: Arc<Responder>,
) -> std::thread::JoinHandle<()> {
    use std::os::unix::net::UnixListener;
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).unwrap();
    // polled, so a test that removes the socket's directory first still stops it
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((conn, _)) => {
                    conn.set_nonblocking(false).unwrap();
                    let (log, respond) = (log.clone(), respond.clone());
                    std::thread::spawn(move || {
                        let writer = conn.try_clone().unwrap();
                        serve(std::io::BufReader::new(conn), writer, &log, &*respond)
                    });
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
    })
}

/// The pipe is the daemon's own (`conde::ipc`, so the CLI's peer check passes); each
/// connection is bridged to a blocking [`serve`] through in-process pipes.
#[cfg(windows)]
fn listen(
    socket: &Path,
    log: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    respond: Arc<Responder>,
) -> std::thread::JoinHandle<()> {
    let socket = socket.to_path_buf();
    let (bound, ready) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut listener = conde::ipc::bind(&socket).unwrap();
            bound.send(()).unwrap();
            while !stop.load(Ordering::SeqCst) {
                let accepted = tokio::time::timeout(
                    std::time::Duration::from_millis(20),
                    conde::ipc::accept(&mut listener),
                )
                .await;
                let Ok(Ok((conn, _peer))) = accepted else {
                    continue;
                };
                let (log, respond) = (log.clone(), respond.clone());
                tokio::spawn(bridge(conn, log, respond));
            }
        });
    });
    ready.recv().unwrap();
    thread
}

#[cfg(windows)]
async fn bridge(conn: conde::ipc::Conn, log: Arc<Mutex<Vec<Value>>>, respond: Arc<Responder>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (read, mut write) = tokio::io::split(conn);
    let mut lines = tokio::io::BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut out = Vec::new();
        serve(line.as_bytes(), &mut out, &log, &*respond);
        if write.write_all(&out).await.is_err() {
            return;
        }
    }
}

fn serve(
    input: impl std::io::BufRead,
    mut writer: impl std::io::Write,
    log: &Mutex<Vec<Value>>,
    respond: &Responder,
) {
    for line in input.lines() {
        let Ok(line) = line else { return };
        let req: Value = serde_json::from_str(&line).unwrap();
        log.lock().unwrap().push(req.clone());
        let frame = match respond(&req) {
            Ok(result) => {
                json!({"v": 1, "type": "response", "request_id": req["request_id"], "ok": true, "result": result})
            }
            Err((code, message)) => {
                json!({"v": 1, "type": "response", "request_id": req["request_id"], "ok": false,
                                           "error": {"code": code, "message": message}})
            }
        };
        if writeln!(writer, "{frame}").is_err() {
            return;
        }
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket);
    }
}
