#![allow(dead_code)]
//! Running the binary the same way on Unix and Windows: a clean environment, the login user
//! and home, a stand-in harness process, and a daemon address.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Clear the child's environment, keeping on Windows only what a process there needs to start
/// and to open sockets.
pub fn clear_env(c: &mut Command) -> &mut Command {
    c.env_clear();
    #[cfg(windows)]
    for k in ["SystemRoot", "SystemDrive", "windir", "TEMP", "TMP"] {
        if let Some(v) = std::env::var_os(k) {
            c.env(k, v);
        }
    }
    c
}

/// The login user and home: `USER` and `HOME`, on Windows also `USERNAME`, `USERPROFILE` and
/// the profile's `AppData` directories.
pub fn user_home<'c>(c: &'c mut Command, user: &str, home: &Path) -> &'c mut Command {
    c.env("USER", user).env("HOME", home);
    #[cfg(windows)]
    c.env("USERNAME", user)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join(r"AppData\Roaming"))
        .env("LOCALAPPDATA", home.join(r"AppData\Local"));
    c
}

/// A process named `name` in `dir` that runs a program as its child, as Claude Code or Codex
/// runs a tool: a shell under that name (Unix), or `examples/fake_harness.rs` copied to
/// `<name>.exe` (Windows, where cmd.exe would mangle arguments).
pub fn harness(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(unix)]
    {
        let path = dir.join(name);
        std::os::unix::fs::symlink("/bin/sh", &path).unwrap();
        path
    }
    #[cfg(windows)]
    {
        // tests run from target/<profile>/deps; `cargo test` builds the examples beside them
        let exe = std::env::current_exe().unwrap();
        let built = exe
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(r"examples\fake_harness.exe");
        assert!(
            built.exists(),
            "{} is missing: build it with cargo build -p conde --example fake_harness",
            built.display()
        );
        let path = dir.join(format!("{name}.exe"));
        std::fs::copy(&built, &path).unwrap();
        path
    }
}

/// `harness` running `program` and the arguments added after this call, which stays its child
/// (the shell does not exec it away).
pub fn under(harness: &Path, program: &str) -> Command {
    let mut c = Command::new(harness);
    #[cfg(unix)]
    c.args(["-c", "\"$0\" \"$@\"; rc=$?; :; exit $rc", program]);
    #[cfg(windows)]
    c.arg(program);
    c
}

/// A daemon address for a test root: `root/run/conde.sock`, or on Windows a pipe of its own,
/// since a pipe is not a file.
pub fn socket_in(root: &Path) -> PathBuf {
    #[cfg(unix)]
    return root.join("run/conde.sock");
    #[cfg(windows)]
    return PathBuf::from(format!(
        r"\\.\pipe\conde-test-{}",
        &conde::util::sha256_hex(root.to_string_lossy().as_bytes())[..16]
    ));
}

/// Whether a daemon answers at `socket` (connects; on Windows the pipe also passes the peer
/// check).
pub fn answers(socket: &Path) -> bool {
    conde::ipc::connect(socket).is_ok()
}
