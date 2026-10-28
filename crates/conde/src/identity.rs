//! Client-side identity: which harness address is this shell?
//!
//! From kond, made symmetric: `CODEX_THREAD_ID` is authoritative unless the
//! nearest harness process above us is Claude; `CLAUDE_CODE_SESSION_ID` unless
//! it is Codex (a Codex child never inherits its parent Claude session);
//! `human:<USER>` only when a successful walk of the process ancestry found no
//! harness at all (a marker-less shell under an agent, or one whose ancestry
//! cannot be read, must never act as the human).

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }

    fn from_program(name: &str) -> Option<Harness> {
        match name.rsplit('/').next().unwrap_or(name) {
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            _ => None,
        }
    }
}

/// What the walk up the process tree found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ancestry {
    /// The nearest harness process above us.
    Harness(Harness),
    /// The walk reached the root without meeting a harness.
    NoHarness,
    /// The walk failed, or this OS has no supported process table.
    Unknown,
}

/// For callers that only know an optional harness: no harness known means
/// the ancestry is unknown, never "no harness".
impl From<Option<Harness>> for Ancestry {
    fn from(h: Option<Harness>) -> Ancestry {
        h.map_or(Ancestry::Unknown, Ancestry::Harness)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityError {
    #[error(
        "cannot determine identity: no session markers, but this process runs under a {0} agent; \
         refusing to act as the human. Use conde from the agent's own shell"
    )]
    UnderHarness(&'static str),
    #[error(
        "cannot determine identity: CLAUDE_CODE_SESSION_ID is set, but the nearest agent harness is {0}, \
         so the marker is inherited; refusing to act as that Claude session"
    )]
    InheritedClaudeMarker(&'static str),
    #[error(
        "cannot determine identity: no session markers, and cannot determine the process ancestry; \
         refusing to act as the human"
    )]
    UnknownAncestry,
    #[error(
        "cannot determine identity: no CLAUDE_CODE_SESSION_ID / CODEX_THREAD_ID / USER in environment"
    )]
    NoMarkers,
}

/// The identity-relevant environment of a shell.
#[derive(Clone, Debug, Default)]
pub struct Markers {
    pub codex_thread: Option<String>,
    pub claude_session: Option<String>,
    pub user: Option<String>,
}

impl Markers {
    pub fn from_env() -> Markers {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Markers {
            codex_thread: var("CODEX_THREAD_ID"),
            claude_session: var("CLAUDE_CODE_SESSION_ID"),
            user: var(crate::util::USER_VAR),
        }
    }
}

/// Derive `kind:target` (no host; the server stamps it). `ancestry` is asked
/// at most once. Unknown ancestry never overrides a session marker; it only
/// blocks the human fallback.
pub fn derive_address<A: Into<Ancestry>>(
    m: &Markers,
    ancestry: impl FnOnce() -> A,
) -> Result<String, IdentityError> {
    let mut ancestry = Some(ancestry);
    let mut cached: Option<Ancestry> = None;
    let mut ancestor =
        || *cached.get_or_insert_with(|| ancestry.take().map_or(Ancestry::Unknown, |f| f().into()));
    if let Some(thread) = &m.codex_thread
        && ancestor() != Ancestry::Harness(Harness::Claude)
    {
        return Ok(format!("codex:{thread}"));
    }
    if let Some(session) = &m.claude_session {
        return match ancestor() {
            Ancestry::Harness(Harness::Codex) => Err(IdentityError::InheritedClaudeMarker(
                Harness::Codex.as_str(),
            )),
            _ => Ok(format!("claude:{session}")),
        };
    }
    if let Some(user) = &m.user {
        return match ancestor() {
            Ancestry::Harness(h) => Err(IdentityError::UnderHarness(h.as_str())),
            Ancestry::Unknown => Err(IdentityError::UnknownAncestry),
            Ancestry::NoHarness => Ok(format!("human:{}", user.to_lowercase())),
        };
    }
    Err(IdentityError::NoMarkers)
}

/// One process as the ancestry walk needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcEntry {
    pub ppid: u32,
    /// Program names to match (argv0, executable path, kernel comm).
    pub names: Vec<String>,
}

/// A source of process facts; injectable so every platform's walk is
/// testable anywhere.
pub trait ProcessTable {
    /// None when the process cannot be read.
    fn entry(&self, pid: u32) -> Option<ProcEntry>;
}

/// Walk from `start` up the parent chain to the nearest harness.
pub fn walk(table: &impl ProcessTable, start: u32) -> Ancestry {
    let mut pid = start;
    for _ in 0..64 {
        let Some(entry) = table.entry(pid) else {
            return Ancestry::Unknown;
        };
        if let Some(h) = entry.names.iter().find_map(|n| Harness::from_program(n)) {
            return Ancestry::Harness(h);
        }
        if entry.ppid <= 1 {
            return Ancestry::NoHarness;
        }
        pid = entry.ppid;
    }
    Ancestry::Unknown
}

/// This process's ancestry, read from the OS process table.
pub fn harness_ancestor() -> Ancestry {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        walk(&SystemTable, std::process::id())
    }
    #[cfg(windows)]
    {
        match crate::win::process_snapshot() {
            Ok(rows) => walk(
                &WindowsTable {
                    rows,
                    created: conde_transport::win::process_created,
                    cmdline: crate::win::command_line,
                },
                std::process::id(),
            ),
            Err(_) => Ancestry::Unknown,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Ancestry::Unknown
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct SystemTable;

// --- Linux: /proc ------------------------------------------------------------

#[cfg(target_os = "linux")]
impl ProcessTable for SystemTable {
    fn entry(&self, pid: u32) -> Option<ProcEntry> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
        linux_entry(&stat, cmdline.as_deref())
    }
}

/// `/proc/<pid>/stat` (comm, ppid) plus argv0 from `/proc/<pid>/cmdline`.
#[cfg(any(target_os = "linux", test))]
fn linux_entry(stat: &str, cmdline: Option<&[u8]>) -> Option<ProcEntry> {
    let (open, close) = (stat.find('(')?, stat.rfind(')')?);
    let comm = stat.get(open + 1..close)?.to_string();
    let ppid = stat
        .get(close + 2..)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let mut names = Vec::new();
    if let Some(argv0) = cmdline
        .and_then(|raw| raw.split(|b| *b == 0).next())
        .filter(|a| !a.is_empty())
    {
        names.push(String::from_utf8_lossy(argv0).into_owned());
    }
    names.push(comm);
    Some(ProcEntry { ppid, names })
}

// --- Windows: a Toolhelp snapshot --------------------------------------------

/// A Windows process table: one snapshot (pid -> parent pid, image file name), with
/// creation times and command lines read for the processes the walk visits.
#[cfg(any(windows, test))]
pub struct WindowsTable<C, L> {
    pub rows: std::collections::HashMap<u32, (u32, String)>,
    /// Creation time (FILETIME) of a live process.
    pub created: C,
    pub cmdline: L,
}

#[cfg(any(windows, test))]
impl<C: Fn(u32) -> Option<u64>, L: Fn(u32) -> Option<String>> ProcessTable for WindowsTable<C, L> {
    fn entry(&self, pid: u32) -> Option<ProcEntry> {
        let (ppid, image) = self.rows.get(&pid)?;
        // Windows keeps the pid of a parent that exited, and may reuse it: a parent that is
        // gone, or younger than its child, makes the child a root of the walk.
        let parent = self.rows.contains_key(ppid)
            && match ((self.created)(*ppid), (self.created)(pid)) {
                (Some(p), Some(c)) => p <= c,
                _ => true,
            };
        Some(windows_entry(
            if parent { *ppid } else { 0 },
            image,
            (self.cmdline)(pid).as_deref(),
        ))
    }
}

/// A file name without directories or a `.exe` suffix, lowercased (`claude`).
#[cfg(any(windows, test))]
fn windows_program(path: &str) -> String {
    let base = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    base.strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(base)
}

/// The first word of a command line, quoted or not.
#[cfg(any(windows, test))]
fn windows_argv0(cmdline: &str) -> Option<&str> {
    let line = cmdline.trim_start();
    let word = match line.strip_prefix('"') {
        Some(rest) => rest.split('"').next(),
        None => line.split_whitespace().next(),
    };
    word.filter(|w| !w.is_empty())
}

/// A Windows process: its image name, argv0 from the command line, and for a script
/// host (`node`, `bun`) the harness whose npm package the command line runs, since an
/// npm-installed Claude Code is `node.exe ...\@anthropic-ai\claude-code\cli.js`.
#[cfg(any(windows, test))]
fn windows_entry(ppid: u32, image: &str, cmdline: Option<&str>) -> ProcEntry {
    let mut names = vec![windows_program(image)];
    if let Some(line) = cmdline {
        if let Some(argv0) = windows_argv0(line)
            .map(windows_program)
            .filter(|a| !names.contains(a))
        {
            names.push(argv0);
        }
        if names.iter().any(|n| n == "node" || n == "bun") {
            let line = line.to_lowercase().replace('\\', "/");
            for (package, harness) in [
                ("/@anthropic-ai/claude-code/", "claude"),
                ("/@openai/codex/", "codex"),
            ] {
                if line.contains(package) {
                    names.push(harness.to_string());
                }
            }
        }
    }
    ProcEntry { ppid, names }
}

// --- macOS: libproc ----------------------------------------------------------

#[cfg(target_os = "macos")]
impl ProcessTable for SystemTable {
    fn entry(&self, pid: u32) -> Option<ProcEntry> {
        use std::ffi::c_void;
        let pid = libc::c_int::try_from(pid).ok()?;
        // PROC_PIDT_SHORTBSDINFO needs no privilege over the target, unlike
        // PROC_PIDTBSDINFO, so root-owned ancestors (login) stay readable.
        let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                (&raw mut info).cast::<c_void>(),
                size,
            )
        };
        if got != size {
            return None;
        }
        let comm: Vec<u8> = info.pbsi_comm.iter().map(|c| *c as u8).collect();

        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe {
            libc::proc_pidpath(pid, path.as_mut_ptr().cast::<c_void>(), path.len() as u32)
        };
        let path = (n > 0).then(|| String::from_utf8_lossy(&path[..n as usize]).into_owned());

        // argv0 (KERN_PROCARGS2) names a harness started through a symlink
        // to a versioned binary; it is readable for our own user's processes,
        // which is where a harness lives.
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut len: libc::size_t = 0;
        let mut args = Vec::new();
        let sized = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if sized == 0 && len > 0 {
            args.resize(len, 0);
            let ok = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    3,
                    args.as_mut_ptr().cast::<c_void>(),
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            args.truncate(if ok == 0 { len } else { 0 });
        }
        Some(darwin_entry(info.pbsi_ppid, &comm, path.as_deref(), &args))
    }
}

/// A macOS process from `proc_bsdshortinfo` (ppid, comm truncated to
/// MAXCOMLEN), `proc_pidpath` and the raw `KERN_PROCARGS2` buffer.
#[cfg(any(target_os = "macos", test))]
fn darwin_entry(ppid: u32, comm: &[u8], path: Option<&str>, procargs2: &[u8]) -> ProcEntry {
    let mut names = Vec::new();
    if let Some(argv0) = darwin_argv0(procargs2) {
        names.push(argv0);
    }
    if let Some(p) = path.filter(|p| !p.is_empty()) {
        names.push(p.to_string());
    }
    let comm = comm.split(|b| *b == 0).next().unwrap_or_default();
    if !comm.is_empty() {
        names.push(String::from_utf8_lossy(comm).into_owned());
    }
    ProcEntry { ppid, names }
}

/// argv0 from a `KERN_PROCARGS2` buffer: argc (native i32), the exec path,
/// NUL padding, then argv.
#[cfg(any(target_os = "macos", test))]
fn darwin_argv0(buf: &[u8]) -> Option<String> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if argc < 1 {
        return None;
    }
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|b| *b == 0)?;
    let after = &rest[exec_end..];
    let start = after.iter().position(|b| *b != 0)?;
    let argv0 = after[start..].split(|b| *b == 0).next()?;
    (!argv0.is_empty()).then(|| String::from_utf8_lossy(argv0).into_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn markers(codex: Option<&str>, claude: Option<&str>, user: Option<&str>) -> Markers {
        Markers {
            codex_thread: codex.map(Into::into),
            claude_session: claude.map(Into::into),
            user: user.map(Into::into),
        }
    }

    const CLAUDE: Ancestry = Ancestry::Harness(Harness::Claude);
    const CODEX: Ancestry = Ancestry::Harness(Harness::Codex);

    #[test]
    fn codex_is_authoritative_outside_claude() {
        let m = markers(Some("t1"), Some("s1"), Some("me"));
        assert_eq!(derive_address(&m, || CODEX).unwrap(), "codex:t1");
        assert_eq!(
            derive_address(&m, || Ancestry::NoHarness).unwrap(),
            "codex:t1"
        );
        assert_eq!(
            derive_address(&m, || Ancestry::Unknown).unwrap(),
            "codex:t1"
        );
    }

    #[test]
    fn nearest_claude_owns_mixed_markers() {
        let m = markers(Some("t1"), Some("s1"), Some("me"));
        assert_eq!(derive_address(&m, || CLAUDE).unwrap(), "claude:s1");
    }

    #[test]
    fn claude_with_only_inherited_codex_never_becomes_human() {
        let m = markers(Some("t1"), None, Some("me"));
        assert_eq!(
            derive_address(&m, || CLAUDE),
            Err(IdentityError::UnderHarness("claude"))
        );
    }

    #[test]
    fn markerless_shell_under_harness_refuses_human() {
        let m = markers(None, None, Some("me"));
        assert_eq!(
            derive_address(&m, || CODEX),
            Err(IdentityError::UnderHarness("codex"))
        );
        assert_eq!(
            derive_address(&m, || Ancestry::NoHarness).unwrap(),
            "human:me"
        );
        assert_eq!(
            derive_address(&markers(None, None, None), || Ancestry::NoHarness),
            Err(IdentityError::NoMarkers)
        );
    }

    #[test]
    fn unknown_ancestry_refuses_the_human() {
        let m = markers(None, None, Some("me"));
        let err = derive_address(&m, || Ancestry::Unknown).unwrap_err();
        assert_eq!(err, IdentityError::UnknownAncestry);
        assert!(
            err.to_string()
                .contains("cannot determine the process ancestry; refusing to act as the human")
        );
        // an Option-only caller with no harness known is Unknown, not "no harness"
        assert_eq!(
            derive_address(&m, || None::<Harness>),
            Err(IdentityError::UnknownAncestry)
        );
    }

    #[test]
    fn claude_marker_counts_unless_the_nearest_harness_is_codex() {
        let m = markers(None, Some("s1"), Some("me"));
        assert_eq!(derive_address(&m, || CLAUDE).unwrap(), "claude:s1");
        assert_eq!(
            derive_address(&m, || Ancestry::NoHarness).unwrap(),
            "claude:s1"
        );
        assert_eq!(
            derive_address(&m, || Ancestry::Unknown).unwrap(),
            "claude:s1"
        );
    }

    #[test]
    fn codex_child_never_inherits_the_parent_claude_identity() {
        let m = markers(None, Some("parent"), Some("me"));
        assert_eq!(
            derive_address(&m, || CODEX),
            Err(IdentityError::InheritedClaudeMarker("codex"))
        );
    }

    struct Fake(HashMap<u32, ProcEntry>);

    impl ProcessTable for Fake {
        fn entry(&self, pid: u32) -> Option<ProcEntry> {
            self.0.get(&pid).cloned()
        }
    }

    fn procargs2(exec: &str, argv: &[&str]) -> Vec<u8> {
        let mut buf = (argv.len() as i32).to_ne_bytes().to_vec();
        buf.extend_from_slice(exec.as_bytes());
        buf.extend_from_slice(&[0, 0, 0, 0]);
        for a in argv {
            buf.extend_from_slice(a.as_bytes());
            buf.push(0);
        }
        buf
    }

    /// A macOS-shaped table: comm bytes as the kernel keeps them (16, NUL
    /// padded), executable paths, raw KERN_PROCARGS2 buffers.
    /// (pid, ppid, comm, executable path, KERN_PROCARGS2 buffer)
    type DarwinProc<'a> = (u32, u32, &'a str, Option<&'a str>, Vec<u8>);

    fn darwin_table(procs: &[DarwinProc]) -> Fake {
        Fake(
            procs
                .iter()
                .map(|(pid, ppid, comm, path, args)| {
                    let mut c = [0u8; 16];
                    let n = comm.len().min(16);
                    c[..n].copy_from_slice(&comm.as_bytes()[..n]);
                    (*pid, darwin_entry(*ppid, &c, *path, args))
                })
                .collect(),
        )
    }

    #[test]
    fn darwin_walk_finds_harness_by_comm_path_or_argv0() {
        // conde <- zsh <- codex (comm) <- launchd
        let t = darwin_table(&[
            (500, 400, "conde", Some("/opt/homebrew/bin/conde"), vec![]),
            (400, 300, "zsh", Some("/bin/zsh"), vec![]),
            (300, 1, "codex", Some("/opt/homebrew/bin/codex"), vec![]),
        ]);
        assert_eq!(walk(&t, 500), CODEX);
        // claude installed as a versioned binary behind a symlink: comm and
        // path name the version, argv0 names the harness
        let t = darwin_table(&[
            (500, 300, "conde", None, vec![]),
            (
                300,
                1,
                "2.1.77",
                Some("/Users/k/.local/share/claude/versions/2.1.77"),
                procargs2(
                    "/Users/k/.local/share/claude/versions/2.1.77",
                    &["claude", "--resume"],
                ),
            ),
        ]);
        assert_eq!(walk(&t, 500), CLAUDE);
        // comm truncated to 16 chars: the full path still decides
        let t = darwin_table(&[(
            500,
            1,
            "a-very-long-name-xx",
            Some("/usr/local/bin/codex"),
            vec![],
        )]);
        assert_eq!(walk(&t, 500), CODEX);
    }

    #[test]
    fn darwin_walk_without_harness_and_on_failure() {
        // Terminal -> login (root, no argv readable) -> zsh -> conde
        let t = darwin_table(&[
            (500, 400, "conde", None, vec![]),
            (
                400,
                350,
                "zsh",
                Some("/bin/zsh"),
                procargs2("/bin/zsh", &["-zsh"]),
            ),
            (350, 1, "login", Some("/usr/bin/login"), vec![]),
        ]);
        assert_eq!(walk(&t, 500), Ancestry::NoHarness);
        // an unreadable ancestor makes the ancestry unknown, never "no harness"
        let t = darwin_table(&[(500, 400, "conde", None, vec![])]);
        assert_eq!(walk(&t, 500), Ancestry::Unknown);
    }

    #[test]
    fn darwin_argv0_parsing() {
        assert_eq!(
            darwin_argv0(&procargs2("/x/y", &["codex", "exec"])).as_deref(),
            Some("codex")
        );
        assert_eq!(darwin_argv0(&procargs2("/x/y", &[])), None);
        assert_eq!(darwin_argv0(&[1, 0]), None);
        assert_eq!(darwin_argv0(&[]), None);
    }

    #[test]
    fn linux_entries_from_proc_files() {
        let e = linux_entry(
            "123 (my (odd) comm) S 77 1 1 0",
            Some(b"/home/k/.local/bin/claude\0--x\0"),
        )
        .unwrap();
        assert_eq!(e.ppid, 77);
        assert_eq!(e.names, ["/home/k/.local/bin/claude", "my (odd) comm"]);
        assert_eq!(linux_entry("garbage", None), None);
        let t = Fake(HashMap::from([
            (9, linux_entry("9 (conde) S 8", None).unwrap()),
            (8, linux_entry("8 (bash) S 1", Some(b"-bash\0")).unwrap()),
        ]));
        assert_eq!(walk(&t, 9), Ancestry::NoHarness);
    }

    #[test]
    fn live_walk_reads_this_process_table() {
        // whatever runs the tests, the walk itself must succeed here
        assert_ne!(harness_ancestor(), Ancestry::Unknown);
    }

    fn windows_table(
        rows: &[(u32, u32, &str)],
        created: &'static [(u32, u64)],
        cmdlines: &'static [(u32, &'static str)],
    ) -> WindowsTable<impl Fn(u32) -> Option<u64>, impl Fn(u32) -> Option<String>> {
        WindowsTable {
            rows: rows
                .iter()
                .map(|(pid, ppid, image)| (*pid, (*ppid, image.to_string())))
                .collect(),
            created: move |pid| created.iter().find(|(p, _)| *p == pid).map(|(_, t)| *t),
            cmdline: move |pid| {
                cmdlines
                    .iter()
                    .find(|(p, _)| *p == pid)
                    .map(|(_, c)| c.to_string())
            },
        }
    }

    #[test]
    fn windows_walk_finds_the_harness_and_ends_at_an_exited_parent() {
        // conde <- pwsh <- claude <- explorer, whose parent (userinit) exited long ago
        let rows = [
            (40, 30, "conde.exe"),
            (30, 20, "pwsh.exe"),
            (20, 10, "claude.exe"),
            (10, 6, "explorer.exe"),
        ];
        let t = windows_table(&rows, &[(40, 400), (30, 300), (20, 200), (10, 100)], &[]);
        assert_eq!(walk(&t, 40), CLAUDE);
        // a shell straight under explorer: no harness, and the dead parent is the root, not unknown
        let rows = [
            (40, 30, "conde.exe"),
            (30, 10, "pwsh.exe"),
            (10, 6, "explorer.exe"),
        ];
        let t = windows_table(&rows, &[(40, 400), (30, 300), (10, 100)], &[]);
        assert_eq!(walk(&t, 40), Ancestry::NoHarness);
        assert_eq!(t.entry(10).unwrap().ppid, 0);
    }

    #[test]
    fn windows_walk_stops_at_a_reused_parent_pid() {
        // the shell's real parent exited and its pid now belongs to a younger codex
        let rows = [
            (40, 30, "conde.exe"),
            (30, 20, "cmd.exe"),
            (20, 1, "codex.exe"),
        ];
        let t = windows_table(&rows, &[(40, 400), (30, 300), (20, 350)], &[]);
        assert_eq!(walk(&t, 40), Ancestry::NoHarness);
        // unreadable creation times (a protected process) keep the link
        let t = windows_table(&rows, &[(40, 400)], &[]);
        assert_eq!(walk(&t, 40), CODEX);
        // a process missing from the snapshot is unknown
        assert_eq!(walk(&t, 99), Ancestry::Unknown);
    }

    #[test]
    fn windows_names_cover_npm_installs() {
        let e = windows_entry(
            5,
            "Codex.EXE",
            Some(r#""C:\Users\k\AppData\Local\codex\codex.exe" app-server"#),
        );
        assert_eq!(e.names, ["codex"]);
        let e = windows_entry(
            5,
            "node.exe",
            Some(
                r#""C:\Program Files\nodejs\node.exe" C:\Users\k\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js --resume"#,
            ),
        );
        assert_eq!(e.names, ["node", "claude"]);
        assert_eq!(Harness::from_program(&e.names[1]), Some(Harness::Claude));
        let e = windows_entry(
            5,
            "node.exe",
            Some(r#"node C:\npm\node_modules\@openai\codex\bin\codex.js"#),
        );
        assert_eq!(e.names, ["node", "codex"]);
        // a script host running something else is just itself
        let e = windows_entry(5, "node.exe", Some("node server.js"));
        assert_eq!(e.names, ["node"]);
        assert_eq!(windows_argv0("  plain.exe -x"), Some("plain.exe"));
        assert_eq!(windows_argv0(""), None);
    }
}
