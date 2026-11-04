//! Where the board and the socket live.

use std::path::{Path, PathBuf};

/// `$CONDE_DATA`, else `~/.local/share/conde` (Windows: `%USERPROFILE%\.conde\data`).
pub fn data_dir() -> PathBuf {
    if let Some(d) = env_var("CONDE_DATA") {
        return d;
    }
    #[cfg(windows)]
    {
        windows_in_use(WindowsDirs::resolve(&env_var, std::env::home_dir()).data)
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".local/share/conde")
    }
}

/// Board files, the default board and the v1 credential: `$XDG_CONFIG_HOME/conde`, else
/// `~/.config/conde` (Windows: `%USERPROFILE%\.conde\config`).
pub fn config_dir() -> anyhow::Result<PathBuf> {
    #[cfg(windows)]
    {
        let config = WindowsDirs::resolve(&env_var, std::env::home_dir()).config;
        Ok(match windows_move_of(&config) {
            Some((old, new)) => config_in_use(&old, &new),
            None => config,
        })
    }
    #[cfg(not(windows))]
    {
        use anyhow::Context as _;
        Ok(match env_var("XDG_CONFIG_HOME") {
            Some(d) => d.join("conde"),
            None => env_var("HOME")
                .context("HOME is not set")?
                .join(".config")
                .join("conde"),
        })
    }
}

/// An environment variable as a path; unset and empty alike are None.
pub fn env_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Where conde keeps its files on Windows. A packaged (MSIX) app, such as the Claude desktop app,
/// redirects files its shells create under `AppData` to a private per-package copy that the
/// bridge (a scheduled task, outside the package) never sees, so everything lives under
/// `%USERPROFILE%\.conde` instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsDirs {
    /// The binary: `$CONDE_INSTALL_DIR`, else `<profile>\.conde\bin`.
    pub bin: PathBuf,
    /// `$XDG_CONFIG_HOME\conde`, else `<profile>\.conde\config`.
    pub config: PathBuf,
    /// The local board and the bridge's log: `$CONDE_DATA`, else `<profile>\.conde\data`.
    pub data: PathBuf,
}

impl WindowsDirs {
    /// From `var` (an environment lookup, empty values None); the profile is `%USERPROFILE%`,
    /// else `home`, else the current directory.
    pub fn resolve(var: &dyn Fn(&str) -> Option<PathBuf>, home: Option<PathBuf>) -> WindowsDirs {
        let own = profile(var, home).join(".conde");
        WindowsDirs {
            bin: var("CONDE_INSTALL_DIR").unwrap_or_else(|| own.join("bin")),
            config: var("XDG_CONFIG_HOME").map_or_else(|| own.join("config"), |x| x.join("conde")),
            data: var("CONDE_DATA").unwrap_or_else(|| own.join("data")),
        }
    }
}

fn profile(var: &dyn Fn(&str) -> Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    var("USERPROFILE")
        .or(home)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `%APPDATA%` and `%LOCALAPPDATA%`, else the profile's own.
fn appdata(var: &dyn Fn(&str) -> Option<PathBuf>, profile: &Path) -> (PathBuf, PathBuf) {
    (
        var("APPDATA").unwrap_or_else(|| profile.join("AppData").join("Roaming")),
        var("LOCALAPPDATA").unwrap_or_else(|| profile.join("AppData").join("Local")),
    )
}

/// The binary's directory up to 0.5.17: `%LOCALAPPDATA%\conde\bin`.
pub fn windows_legacy_bin(var: &dyn Fn(&str) -> Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    let (_, local) = appdata(var, &profile(var, home));
    local.join("conde").join("bin")
}

/// What moves out of `AppData` on the first run of a release after 0.5.17, (before, now) for each
/// location no override places: the config directory, the data directory and the task file.
pub fn windows_moves(
    var: &dyn Fn(&str) -> Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<(PathBuf, PathBuf)> {
    let profile = profile(var, home.clone());
    let (roaming, local) = appdata(var, &profile);
    let now = WindowsDirs::resolve(var, home);
    let mut moves = Vec::new();
    if var("XDG_CONFIG_HOME").is_none() {
        moves.push((roaming.join("conde"), now.config));
    }
    if var("CONDE_DATA").is_none() {
        let task = now.data.with_file_name("conde-task.xml");
        moves.push((local.join("conde").join("data"), now.data));
        moves.push((local.join("conde").join("conde-task.xml"), task));
    }
    moves
}

/// Whether `p` exists; Err for anything other than found or not found (no access, a file where a
/// directory belongs), which must not pass for absent.
fn present(p: &Path) -> std::io::Result<bool> {
    match p.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The location to use of one that moves from `old` to `new`: `old` while the move is pending
/// (`new` absent, `old` not known absent), so nothing is created at `new` beside it; else `new`.
pub fn in_use(old: &Path, new: &Path) -> PathBuf {
    match (present(new), present(old)) {
        (Ok(false), Ok(false)) => new.to_path_buf(),
        (Ok(false), _) => old.to_path_buf(),
        _ => new.to_path_buf(),
    }
}

/// The config directory to use while it moves from `old` to `new`: `old` while any file is left
/// in it (a merge not yet done, or a save that recreated it, is never hidden behind a partly
/// filled `new`); `new` once `old` is gone or empty.
pub fn config_in_use(old: &Path, new: &Path) -> PathBuf {
    match has_files(old) {
        Ok(false) => new.to_path_buf(),
        _ => old.to_path_buf(),
    }
}

fn has_files(dir: &Path) -> std::io::Result<bool> {
    if !present(dir)? {
        return Ok(false);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || has_files(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The move from AppData of one of [`WindowsDirs`]'s locations, (before, now).
#[cfg(windows)]
fn windows_move_of(target: &Path) -> Option<(PathBuf, PathBuf)> {
    windows_moves(&env_var, std::env::home_dir())
        .into_iter()
        .find(|(_, new)| new == target)
}

/// [`in_use`] for one of [`WindowsDirs`]'s locations.
#[cfg(windows)]
fn windows_in_use(target: PathBuf) -> PathBuf {
    match windows_move_of(&target) {
        Some((old, new)) => in_use(&old, &new),
        None => target,
    }
}

/// Rename `old` to `new` when `old` exists and `new` does not. Ok(true) when this call moved it.
/// A rename only: it moves the whole directory at once or nothing (a file in use, or `new` on
/// another volume, is an error that changed nothing), so a database is never copied while open.
pub fn move_if_absent(old: &Path, new: &Path) -> anyhow::Result<bool> {
    use anyhow::Context as _;
    let looked = |p: &Path| present(p).with_context(|| format!("looking at {}", p.display()));
    if looked(new)? || !looked(old)? {
        return Ok(false);
    }
    if let Some(parent) = new.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    match std::fs::rename(old, new) {
        Ok(()) => Ok(true),
        Err(e) if crosses_volumes(&e) => anyhow::bail!(
            "{} is on another volume than {}; conde keeps using it there",
            old.display(),
            new.display()
        ),
        // another conde process moved it first
        Err(_) if !looked(old)? && looked(new)? => Ok(false),
        Err(e) => Err(e).with_context(|| format!("moving {} to {}", old.display(), new.display())),
    }
}

/// Move the config directory `old` to `new` file by file where it cannot go at once: a rename of
/// the whole directory when `new` does not exist; else (`new` exists, from a save that raced an
/// earlier move, or `new` is on another volume) every file under `old` moves in, over the copy at
/// `new` when it is newer (an older or equal one is removed), and the directories of `old` left
/// empty are removed. Ok(true) when anything moved.
pub fn merge_dir(old: &Path, new: &Path) -> anyhow::Result<bool> {
    use anyhow::Context as _;
    let looked = |p: &Path| present(p).with_context(|| format!("looking at {}", p.display()));
    if !looked(old)? {
        return Ok(false);
    }
    if !looked(new)? {
        if let Some(parent) = new.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        match std::fs::rename(old, new) {
            Ok(()) => return Ok(true),
            Err(e) if crosses_volumes(&e) => {}
            Err(_) if !looked(old)? && looked(new)? => return Ok(false),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("moving {} to {}", old.display(), new.display()));
            }
        }
    }
    let moved = merge_files(old, new)?;
    remove_empty_dirs(old);
    Ok(moved)
}

fn merge_files(old: &Path, new: &Path) -> anyhow::Result<bool> {
    use anyhow::Context as _;
    crate::private::create_dir(new).with_context(|| format!("creating {}", new.display()))?;
    let mut moved = false;
    for entry in std::fs::read_dir(old).with_context(|| format!("reading {}", old.display()))? {
        let entry = entry?;
        let (from, to) = (entry.path(), new.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            moved |= merge_files(&from, &to)?;
            continue;
        }
        // both have it: the newer copy wins (a save that recreated the old place lands last)
        if present(&to).with_context(|| format!("looking at {}", to.display()))? {
            let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified());
            if modified(&from)? <= modified(&to)? {
                std::fs::remove_file(&from)
                    .with_context(|| format!("removing {}", from.display()))?;
                continue;
            }
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => {}
            Err(e) if crosses_volumes(&e) => copy_file_into(&from, &to)?,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("moving {} to {}", from.display(), to.display()));
            }
        }
        moved = true;
    }
    Ok(moved)
}

/// One file across volumes: copied beside `to`, renamed into place once whole, then removed
/// from `from` (a removal that fails leaves it behind, unused: `to` wins).
fn copy_file_into(from: &Path, to: &Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let staging = to.with_file_name(format!(".{name}.moving-{}", std::process::id()));
    let copied = std::fs::copy(from, &staging).and_then(|_| std::fs::rename(&staging, to));
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&staging);
        return Err(e).with_context(|| format!("copying {} to {}", from.display(), to.display()));
    }
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// Remove `dir` and the directories under it that are empty; anything with a file stays.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_dirs(&entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

fn crosses_volumes(e: &std::io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE (17) on Windows
    e.kind() == std::io::ErrorKind::CrossesDevices
        || (cfg!(windows) && e.raw_os_error() == Some(17))
}

/// The bridge, at its start while no other bridge holds the pipe: move what an earlier conde kept
/// under `AppData` (`moves`, [`windows_moves`]), the config directory `config` file by file
/// ([`merge_dir`]), the rest whole ([`move_if_absent`]), and tell the human each move once
/// (`say`). Only the bridge moves: a running bridge keeps the directory it started with, so a
/// command that moved it would strand that bridge, and [`in_use`] keeps every process on the old
/// place until the move. A move that fails changes nothing and is said when `report`; its old
/// location stays in use. true when nothing is left to move.
pub fn first_run_moves(
    moves: &[(PathBuf, PathBuf)],
    config: &Path,
    say: &mut dyn FnMut(String),
    report: bool,
) -> bool {
    let mut done = true;
    for (old, new) in moves {
        let moved = match new == config {
            true => merge_dir(old, new),
            false => move_if_absent(old, new),
        };
        match moved {
            Ok(true) => {
                say(format!(
                    "conde: moved {} to {} (conde keeps its files out of AppData now)",
                    old.display(),
                    new.display()
                ));
                #[cfg(windows)]
                {
                    let private = match new.is_dir() {
                        true => conde_transport::win::Security::owner_only(true)
                            .and_then(|s| s.apply(new)),
                        false => Ok(()),
                    };
                    if let Err(e) = private {
                        say(format!(
                            "conde: could not make {} owner-only: {e}",
                            new.display()
                        ));
                    }
                }
            }
            Ok(false) => {}
            Err(e) => {
                done = false;
                if report {
                    say(format!(
                        "conde: {} stays in use where it is: {e:#}",
                        old.display()
                    ));
                }
            }
        }
    }
    done
}

/// [`first_run_moves`] for this process's environment.
#[cfg(windows)]
pub fn windows_first_run(say: &mut dyn FnMut(String), report: bool) -> bool {
    let config = WindowsDirs::resolve(&env_var, std::env::home_dir()).config;
    first_run_moves(
        &windows_moves(&env_var, std::env::home_dir()),
        &config,
        say,
        report,
    )
}

/// `$CONDE_SOCK`, else `$XDG_RUNTIME_DIR/conde/conde.sock` (Windows: the pipe
/// `\\.\pipe\conde-<user>`).
pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("CONDE_SOCK").filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    default_socket_path()
}

/// The socket without `$CONDE_SOCK`: `$XDG_RUNTIME_DIR/conde/conde.sock`.
#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("conde-{}", crate::util::own_uid())));
    runtime.join("conde").join("conde.sock")
}

/// The socket without `$CONDE_SOCK`: the named pipe `\\.\pipe\conde-<user>`.
#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    PathBuf::from(pipe_for_user(&crate::util::local_user()))
}

/// `\\.\pipe\conde-<user>`, the user name reduced to `[a-z0-9._-]`.
pub fn pipe_for_user(user: &str) -> String {
    let name: String = user
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!(r"\\.\pipe\conde-{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_pipe_names() {
        assert_eq!(pipe_for_user("Krisz"), r"\\.\pipe\conde-krisz");
        assert_eq!(pipe_for_user("Kovács Béla"), r"\\.\pipe\conde-kov-cs-b-la");
        assert_eq!(pipe_for_user(r"DOMAIN\bob"), r"\\.\pipe\conde-domain-bob");
    }

    fn vars(set: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let set = set.to_vec();
        move |k: &str| {
            set.iter()
                .find(|(name, v)| *name == k && !v.is_empty())
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    const PROFILE: [(&str, &str); 3] = [
        ("USERPROFILE", "P"),
        ("APPDATA", "R"),
        ("LOCALAPPDATA", "L"),
    ];

    fn under(base: &str, parts: &[&str]) -> PathBuf {
        parts.iter().fold(PathBuf::from(base), |p, s| p.join(s))
    }

    /// Bin, config and data under `%USERPROFILE%\.conde`, never under AppData.
    #[test]
    fn windows_dirs_are_under_the_profile() {
        let dirs = WindowsDirs::resolve(&vars(&PROFILE), Some(PathBuf::from("H")));
        assert_eq!(
            dirs,
            WindowsDirs {
                bin: under("P", &[".conde", "bin"]),
                config: under("P", &[".conde", "config"]),
                data: under("P", &[".conde", "data"]),
            }
        );
        // no USERPROFILE (or an empty one): the home directory
        for set in [&[][..], &[("USERPROFILE", "")][..]] {
            let dirs = WindowsDirs::resolve(&vars(set), Some(PathBuf::from("H")));
            assert_eq!(dirs.bin, under("H", &[".conde", "bin"]));
            assert_eq!(dirs.config, under("H", &[".conde", "config"]));
            assert_eq!(dirs.data, under("H", &[".conde", "data"]));
        }
        assert_eq!(
            windows_legacy_bin(&vars(&PROFILE), None),
            under("L", &["conde", "bin"])
        );
    }

    /// Each override places its own directory and leaves the other two under the profile.
    #[test]
    fn windows_overrides_win() {
        let with = |k: &'static str, v: &'static str| {
            let mut set = PROFILE.to_vec();
            set.push((k, v));
            WindowsDirs::resolve(&vars(&set), None)
        };
        let (bin, config, data) = (
            under("P", &[".conde", "bin"]),
            under("P", &[".conde", "config"]),
            under("P", &[".conde", "data"]),
        );
        assert_eq!(
            with("CONDE_INSTALL_DIR", "I"),
            WindowsDirs {
                bin: PathBuf::from("I"),
                config: config.clone(),
                data: data.clone(),
            }
        );
        assert_eq!(
            with("XDG_CONFIG_HOME", "X"),
            WindowsDirs {
                bin: bin.clone(),
                config: under("X", &["conde"]),
                data: data.clone(),
            }
        );
        assert_eq!(
            with("CONDE_DATA", "D"),
            WindowsDirs {
                bin: bin.clone(),
                config: config.clone(),
                data: PathBuf::from("D"),
            }
        );
        // set but empty: no override
        assert_eq!(with("CONDE_DATA", ""), WindowsDirs { bin, config, data });
    }

    /// The config dir, the data dir and the task file move from AppData; an override keeps its
    /// location and moves nothing.
    #[test]
    fn windows_moves_out_of_appdata() {
        let config = (under("R", &["conde"]), under("P", &[".conde", "config"]));
        let data = (
            under("L", &["conde", "data"]),
            under("P", &[".conde", "data"]),
        );
        let task = (
            under("L", &["conde", "conde-task.xml"]),
            under("P", &[".conde", "conde-task.xml"]),
        );
        assert_eq!(
            windows_moves(&vars(&PROFILE), None),
            [config.clone(), data.clone(), task.clone()]
        );
        let mut set = PROFILE.to_vec();
        set.push(("XDG_CONFIG_HOME", "X"));
        assert_eq!(windows_moves(&vars(&set), None), [data, task]);
        set.push(("CONDE_DATA", "D"));
        assert_eq!(windows_moves(&vars(&set), None), []);
        // the profile's own AppData when the variables are missing
        assert_eq!(
            windows_moves(&vars(&[("USERPROFILE", "P")]), None)[0],
            (
                under("P", &["AppData", "Roaming", "conde"]),
                under("P", &[".conde", "config"])
            )
        );
    }

    fn tree(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if p.is_dir() {
                    stack.push(p);
                    out.push(format!("{rel}/"));
                } else {
                    out.push(format!("{rel}={}", std::fs::read_to_string(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    fn old_config(root: &Path) -> PathBuf {
        let old = root.join("AppData/Roaming/conde");
        std::fs::create_dir_all(old.join("boards")).unwrap();
        std::fs::write(old.join("boards/demo.json"), "{}").unwrap();
        std::fs::write(old.join("default-board"), "demo\n").unwrap();
        old
    }

    /// Old present, new absent: the contents move, the old location is gone.
    #[test]
    fn migration_moves_old_to_new() {
        let root = tempfile::tempdir().unwrap();
        let old = old_config(root.path());
        let new = root.path().join(".conde/config");
        let before = tree(&old);
        assert!(move_if_absent(&old, &new).unwrap());
        assert!(!old.exists());
        assert_eq!(tree(&new), before);
        // a second run finds nothing to do
        assert!(!move_if_absent(&old, &new).unwrap());
        assert_eq!(tree(&new), before);
        // a file (the task definition) moves the same way
        let (old_task, new_task) = (
            root.path().join("task.xml"),
            root.path().join(".conde/task.xml"),
        );
        std::fs::write(&old_task, "xml").unwrap();
        assert!(move_if_absent(&old_task, &new_task).unwrap());
        assert_eq!(std::fs::read_to_string(&new_task).unwrap(), "xml");
        assert!(!old_task.exists());
    }

    /// The data rule, both present: both stay as they are (a database is never merged).
    #[test]
    fn migration_leaves_both_alone() {
        let root = tempfile::tempdir().unwrap();
        let old = old_config(root.path());
        let new = root.path().join(".conde/config");
        std::fs::create_dir_all(new.join("boards")).unwrap();
        std::fs::write(new.join("boards/acme.json"), "{}").unwrap();
        let (old_before, new_before) = (tree(&old), tree(&new));
        assert!(!move_if_absent(&old, &new).unwrap());
        assert_eq!((tree(&old), tree(&new)), (old_before, new_before));
    }

    /// New present only, or neither: nothing happens.
    #[test]
    fn migration_without_an_old_dir_is_a_no_op() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (
            root.path().join("AppData/Roaming/conde"),
            root.path().join(".conde/config"),
        );
        assert!(!move_if_absent(&old, &new).unwrap());
        assert!(!old.exists() && !new.exists(), "nothing created");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("default-board"), "local\n").unwrap();
        assert!(!move_if_absent(&old, &new).unwrap());
        assert_eq!(tree(&new), ["default-board=local\n"]);
        assert!(!old.exists());
    }

    /// Only "not found" is absent: a location that cannot be looked at is an error, never a
    /// move skipped, so no fresh state appears at the new place beside old data.
    #[test]
    fn migration_refuses_a_location_it_cannot_look_at() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let new = root.path().join(".conde/config");
        assert!(move_if_absent(&file.join("conde"), &new).is_err());
        assert!(!new.exists());
        let old = old_config(root.path());
        assert!(move_if_absent(&old, &file.join("config")).is_err());
        assert!(old.join("boards/demo.json").exists());
    }

    /// While a move is pending (the old location there, the new one not) the old one stays in
    /// use, so a move that could not happen yet never strands it behind a fresh new location.
    #[test]
    fn a_pending_move_keeps_the_old_location_in_use() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join("old"), root.path().join("new"));
        assert_eq!(in_use(&old, &new), new, "neither");
        std::fs::create_dir(&old).unwrap();
        assert_eq!(in_use(&old, &new), old, "pending");
        std::fs::create_dir(&new).unwrap();
        assert_eq!(in_use(&old, &new), new, "both");
        std::fs::remove_dir(&old).unwrap();
        assert_eq!(in_use(&old, &new), new, "moved");
        let file = root.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let unknown = file.join("old");
        assert_eq!(
            in_use(&unknown, &root.path().join("none")),
            unknown,
            "old not known absent"
        );
    }

    fn aged(p: &Path, secs_ago: u64) {
        let at = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    /// The config directory where both exist (a save raced an earlier move and recreated the old
    /// one): each file moves in, the newer copy of one both have wins (the older is removed),
    /// and the old directory goes once empty.
    #[test]
    fn config_merges_file_by_file_when_both_exist() {
        let root = tempfile::tempdir().unwrap();
        let old = old_config(root.path());
        std::fs::write(old.join("credentials"), "v1").unwrap();
        let new = root.path().join(".conde/config");
        std::fs::create_dir_all(new.join("boards")).unwrap();
        std::fs::write(new.join("default-board"), "acme\n").unwrap();
        std::fs::write(new.join("boards/demo.json"), "{\"old\": true}").unwrap();
        // the old place's board file was saved last, its default-board before the new one's
        aged(&new.join("boards/demo.json"), 60);
        aged(&old.join("default-board"), 60);
        assert!(merge_dir(&old, &new).unwrap());
        assert_eq!(
            tree(&new),
            [
                "boards/",
                "boards/demo.json={}",
                "credentials=v1",
                "default-board=acme\n"
            ]
        );
        assert!(!old.exists(), "an empty old directory goes");
        // new absent: the whole directory, at once
        let old = old_config(root.path());
        let fresh = root.path().join(".conde/config2");
        let before = tree(&old);
        assert!(merge_dir(&old, &fresh).unwrap());
        assert_eq!(tree(&fresh), before);
        assert!(!old.exists());
    }

    /// The config directory stays at the old place while any file is left there, whatever the new
    /// place holds; the new place is in use once the merge emptied the old one.
    #[test]
    fn the_config_in_use_is_the_old_one_until_it_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("AppData/Roaming/conde");
        let new = root.path().join(".conde/config");
        assert_eq!(config_in_use(&old, &new), new, "neither");
        std::fs::create_dir_all(old.join("boards")).unwrap();
        assert_eq!(config_in_use(&old, &new), new, "old without a file");
        std::fs::write(old.join("boards/demo.json"), "{}").unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("default-board"), "local\n").unwrap();
        assert_eq!(config_in_use(&old, &new), old, "a file left in old");
        assert!(merge_dir(&old, &new).unwrap());
        assert_eq!(config_in_use(&old, &new), new, "merged");
    }

    /// Across volumes a config file is copied beside its place, renamed in, then removed.
    #[test]
    fn a_file_across_volumes_is_copied_then_removed() {
        let root = tempfile::tempdir().unwrap();
        let (from, to) = (
            root.path().join("demo.json"),
            root.path().join("new/demo.json"),
        );
        std::fs::create_dir(root.path().join("new")).unwrap();
        std::fs::write(&from, "{}").unwrap();
        copy_file_into(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "{}");
        assert!(!from.exists());
        assert_eq!(
            tree(&root.path().join("new")),
            ["demo.json={}"],
            "no staging file left"
        );
    }

    /// The bridge's first run: the config merges, the data directory and the task file move whole,
    /// each said once; a second run has nothing to do. Nothing else moves them.
    #[test]
    fn the_bridges_first_run_moves_everything_once() {
        let root = tempfile::tempdir().unwrap();
        let old = old_config(root.path());
        let old_data = root.path().join("AppData/Local/conde/data");
        std::fs::create_dir_all(&old_data).unwrap();
        std::fs::write(old_data.join("conde.db"), "db").unwrap();
        let old_task = root.path().join("AppData/Local/conde/conde-task.xml");
        std::fs::write(&old_task, "xml").unwrap();
        let home = root.path().join(".conde");
        let moves = [
            (old.clone(), home.join("config")),
            (old_data.clone(), home.join("data")),
            (old_task.clone(), home.join("conde-task.xml")),
        ];
        let mut said = Vec::new();
        assert!(first_run_moves(
            &moves,
            &home.join("config"),
            &mut |l| said.push(l),
            true
        ));
        assert_eq!(said.len(), 3, "{said:?}");
        assert!(!old.exists() && !old_data.exists() && !old_task.exists());
        assert_eq!(
            std::fs::read_to_string(home.join("data/conde.db")).unwrap(),
            "db"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config/boards/demo.json")).unwrap(),
            "{}"
        );
        said.clear();
        assert!(first_run_moves(
            &moves,
            &home.join("config"),
            &mut |l| said.push(l),
            true
        ));
        assert!(said.is_empty(), "{said:?}");
    }
}
