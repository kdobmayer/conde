#![allow(dead_code)]
//! A fake `conde daemon` on a unix socket: records every request frame and answers with a
//! scripted result per op, so the CLI and MCP surfaces are tested against the daemon
//! contract before the daemon implements it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
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
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let respond: Arc<Responder> = Arc::new(respond);
        let (log2, stop2) = (log.clone(), stop.clone());
        // polled, so a test that removes the socket's directory first still stops it
        listener.set_nonblocking(true).unwrap();
        let thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        conn.set_nonblocking(false).unwrap();
                        let (log, respond) = (log2.clone(), respond.clone());
                        std::thread::spawn(move || serve(conn, &log, &*respond));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        });
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

fn serve(conn: UnixStream, log: &Mutex<Vec<Value>>, respond: &Responder) {
    let mut writer = conn.try_clone().unwrap();
    for line in BufReader::new(conn).lines() {
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
        let _ = std::fs::remove_file(&self.socket);
    }
}
