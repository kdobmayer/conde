//! The build id `conde --version` prints: the commit and the build date, from
//! CONDE_BUILD_COMMIT and CONDE_BUILD_DATE (the release workflow sets them) or the git
//! checkout, where uncommitted changes to tracked files add `-dirty` to the commit; `unknown`
//! when neither names a commit.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CONDE_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=CONDE_BUILD_DATE");
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let commit = var("CONDE_BUILD_COMMIT").or_else(|| git_commit(Path::new(&dir)));
    // `today` is evaluated on every run, so a rerun after a change also renews the date.
    let id = match commit {
        Some(c) => format!("{c} {}", var("CONDE_BUILD_DATE").unwrap_or_else(today)),
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=CONDE_BUILD_ID={id}");
}

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// The short HEAD commit, `-dirty` when tracked files differ from it; reruns the build script
/// when HEAD or its branch moves, the index changes (add, commit) or a file of the workspace
/// changes (an edit makes the build dirty).
fn git_commit(dir: &Path) -> Option<String> {
    let commit = git(dir, &["rev-parse", "--short=7", "HEAD"])?;
    // Every tracked file, the only ones that can make the checkout dirty (no build output, no
    // node_modules to walk).
    if let Some(top) = git(dir, &["rev-parse", "--show-toplevel"]) {
        // listed from the top: from the crate, ls-files names only its files, relative to it
        if let Some(files) = git(Path::new(&top), &["ls-files", "-z"]) {
            for f in files.split('\0').filter(|f| !f.is_empty()) {
                println!(
                    "cargo:rerun-if-changed={}",
                    Path::new(&top).join(f).display()
                );
            }
        }
    }
    let dirty = git(
        dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
    )
    .is_some();
    let commit = if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    };
    let mut watched = vec![
        "HEAD".to_string(),
        "packed-refs".to_string(),
        "index".to_string(),
    ];
    if let Some(r) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        watched.push(r);
    }
    for w in watched {
        // a missing path would rerun the script on every build
        if let Some(p) = git(
            dir,
            &["rev-parse", "--path-format=absolute", "--git-path", &w],
        )
        .filter(|p| Path::new(p).exists())
        {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    Some(commit)
}

/// Today's UTC date, YYYY-MM-DD (days to civil date, proleptic Gregorian).
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
