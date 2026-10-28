//! Secrets from the environment: `<NAME>_FILE` names a file whose content (a trailing newline
//! trimmed) wins over `<NAME>`, so a mounted secret never has to sit in the process environment.

use thiserror::Error;

/// A `<NAME>_FILE` that names no readable file.
#[derive(Debug, Error)]
#[error("{var}: cannot read {path}: {cause}")]
pub struct SecretError {
    pub var: String,
    pub path: String,
    pub cause: String,
}

/// The secret `name` from the process environment; see [`secret_from`].
pub fn secret_env(name: &str) -> Result<Option<String>, SecretError> {
    secret_from(name, |k| std::env::var(k).ok())
}

/// The secret `name` as `var` reads variables: the content of the file `<name>_FILE` names when
/// that is set, else `<name>`. An empty value, or a file empty after trimming (a workspace without
/// the secret mounts an empty file), counts as unset; a set `<name>_FILE` is never bypassed for
/// `<name>`.
pub fn secret_from(
    name: &str,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, SecretError> {
    let file_var = format!("{name}_FILE");
    if let Some(path) = var(&file_var).filter(|p| !p.is_empty()) {
        let content = std::fs::read_to_string(&path).map_err(|e| SecretError {
            var: file_var,
            path: path.clone(),
            cause: e.to_string(),
        })?;
        let value = content.trim_end_matches(['\n', '\r']);
        return Ok((!value.trim().is_empty()).then(|| value.to_string()));
    }
    Ok(var(name).filter(|v| !v.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fresh directory for one case's secret files.
    fn scratch() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("conde-secret-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn the_file_wins_over_the_plain_variable_and_loses_its_trailing_newline() {
        let dir = scratch();
        let path = dir.join("admin-key");
        std::fs::write(&path, "from-the-file\n").unwrap();
        let p = path.to_str().unwrap();
        let both = lookup(&[
            ("CONDE_ADMIN_KEY", "from-the-env"),
            ("CONDE_ADMIN_KEY_FILE", p),
        ]);
        assert_eq!(
            secret_from("CONDE_ADMIN_KEY", both).unwrap().as_deref(),
            Some("from-the-file")
        );
        std::fs::write(&path, "two lines\r\nkept inside\r\n\n").unwrap();
        let file = lookup(&[("CONDE_ADMIN_KEY_FILE", p)]);
        assert_eq!(
            secret_from("CONDE_ADMIN_KEY", file).unwrap().as_deref(),
            Some("two lines\r\nkept inside")
        );
        let plain = lookup(&[
            ("CONDE_ADMIN_KEY", "from-the-env"),
            ("CONDE_ADMIN_KEY_FILE", ""),
        ]);
        assert_eq!(
            secret_from("CONDE_ADMIN_KEY", plain).unwrap().as_deref(),
            Some("from-the-env")
        );
        assert_eq!(
            secret_from("CONDE_ADMIN_KEY", lookup(&[("CONDE_ADMIN_KEY", "")])).unwrap(),
            None
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_empty_file_is_unset_and_a_missing_one_is_a_named_error() {
        let dir = scratch();
        let path = dir.join("empty");
        std::fs::write(&path, " \t\n").unwrap();
        let empty = lookup(&[
            ("OIDC_CLIENT_SECRET", "plain"),
            ("OIDC_CLIENT_SECRET_FILE", path.to_str().unwrap()),
        ]);
        assert_eq!(
            secret_from("OIDC_CLIENT_SECRET", empty).unwrap(),
            None,
            "the file is not bypassed"
        );
        let missing = dir.join("missing");
        let gone = lookup(&[
            ("OIDC_CLIENT_SECRET", "plain"),
            ("OIDC_CLIENT_SECRET_FILE", missing.to_str().unwrap()),
        ]);
        let e = secret_from("OIDC_CLIENT_SECRET", gone)
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with(&format!(
                "OIDC_CLIENT_SECRET_FILE: cannot read {}",
                missing.display()
            )),
            "{e}"
        );
        assert!(!e.contains("plain"), "{e}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
