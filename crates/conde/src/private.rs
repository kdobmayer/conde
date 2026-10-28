//! Files that hold a secret (board keys, the v1 credential, a pending login): created
//! readable by the owner only, and refused when other accounts can read them. Unix:
//! mode 0600 in 0700 directories. Windows: a protected DACL granting only the
//! current user's SID; SYSTEM and Administrators, which can read every profile
//! anyway, are not counted as exposure.

use std::fs::File;
use std::io;
use std::path::Path;

/// Create `dir` and its missing parents; a directory created here is owner-only.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(windows)]
    {
        if dir.is_dir() {
            return Ok(());
        }
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        match crate::win::create_private_dir(dir) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
            other => other,
        }
    }
}

/// Create a new file (it must not exist) readable by the owner only.
pub fn create_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        crate::win::create_private_file(path)
    }
}

#[cfg(windows)]
const TRUSTED: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];

/// The accounts besides the owner (and SYSTEM, Administrators) that may read `path`.
#[cfg(windows)]
fn foreign(path: &Path) -> io::Result<Vec<String>> {
    let own = conde_transport::win::own_user_sid()?;
    let mut allowed = vec![own.as_str()];
    allowed.extend(TRUSTED);
    conde_transport::win::foreign_grants(path, &allowed)
}

/// Refuse a secret file other users could read.
pub fn check(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    if let Some(m) = crate::credentials::mode(path)
        && m & 0o077 != 0
    {
        anyhow::bail!(
            "{} has mode {m:03o}; it holds a secret, make it 0600 (chmod 600 {})",
            path.display(),
            path.display()
        );
    }
    #[cfg(windows)]
    match foreign(path) {
        Ok(others) if others.is_empty() => {}
        Ok(others) => anyhow::bail!(
            "{} is readable by {}; it holds a secret, make it yours alone (icacls \"{}\" /inheritance:r /grant:r \"%USERNAME%:F\")",
            path.display(),
            others.join(", "),
            path.display()
        ),
        Err(e) => anyhow::bail!("cannot read the permissions of {}: {e}", path.display()),
    }
    Ok(())
}

/// `conde doctor`'s line on a secret file: (ok, text).
pub fn doctor(path: &Path) -> (bool, String) {
    #[cfg(unix)]
    {
        match crate::credentials::mode(path) {
            Some(0o600) => (true, format!("credentials: {} mode 600", path.display())),
            Some(m) => (
                false,
                format!("credentials: {} mode {m:03o}, expected 600", path.display()),
            ),
            None => (false, format!("credentials: {} missing", path.display())),
        }
    }
    #[cfg(windows)]
    {
        if !path.exists() {
            return (false, format!("credentials: {} missing", path.display()));
        }
        match foreign(path) {
            Ok(others) if others.is_empty() => (
                true,
                format!("credentials: {} readable by this user only", path.display()),
            ),
            Ok(others) => (
                false,
                format!(
                    "credentials: {} also readable by {}",
                    path.display(),
                    others.join(", ")
                ),
            ),
            Err(e) => (
                false,
                format!(
                    "credentials: {} permissions unreadable: {e}",
                    path.display()
                ),
            ),
        }
    }
}
