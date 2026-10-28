//! Addresses (`kind:target@host`) and agent / channel names.

use std::fmt;

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Claude,
    Codex,
    Human,
    /// A published mailbox on a shared board; its target is the board's own agent id.
    Mailbox,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Human => "human",
            Kind::Mailbox => "mailbox",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "claude" => Some(Kind::Claude),
            "codex" => Some(Kind::Codex),
            "human" => Some(Kind::Human),
            "mailbox" => Some(Kind::Mailbox),
            _ => None,
        }
    }
}

/// The current incarnation behind a name: a Claude session, a Codex thread or
/// a human login, on one host.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    pub kind: Kind,
    pub target: String,
    pub host: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("bad address '{0}', expected codex:<id> | claude:<id> | human:<user> (optionally @<host>)")]
pub struct AddressError(pub String);

fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '@' | '/' | ':'))
}

impl Address {
    pub fn new(kind: Kind, target: &str, host: &str) -> Address {
        Address {
            kind,
            target: target.to_string(),
            host: host.to_lowercase(),
        }
    }

    /// Parse `kind:target[@host]`; a missing host means `default_host`.
    pub fn parse(s: &str, default_host: &str) -> Result<Address, AddressError> {
        let bad = || AddressError(s.to_string());
        let (kind, rest) = s.trim().split_once(':').ok_or_else(bad)?;
        let kind = Kind::parse(kind).ok_or_else(bad)?;
        let (target, host) = rest.split_once('@').unwrap_or((rest, default_host));
        if !valid_token(target) || !valid_token(host) {
            return Err(bad());
        }
        Ok(Address::new(kind, target, host))
    }

    /// [`Address::parse`] limited to the v1 kinds (claude, codex, human): the address a
    /// session registers or pins, and a v1 actor. A mailbox is refused with the same error.
    pub fn parse_v1(s: &str, default_host: &str) -> Result<Address, AddressError> {
        Address::parse(s, default_host).and_then(|a| {
            if a.kind == Kind::Mailbox {
                Err(AddressError(s.to_string()))
            } else {
                Ok(a)
            }
        })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.kind.as_str(), self.target, self.host)
    }
}

/// A dotted agent name part: letters, digits, `_` and `-` (so every name is
/// also a valid @mention).
fn valid_part(p: &str) -> bool {
    !p.is_empty()
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Lowercased dotted name (`project.task.role`), or None when invalid.
pub fn normalize_name(name: &str) -> Option<String> {
    let lower = name.trim().to_lowercase();
    (lower.len() <= 200 && lower.split('.').all(valid_part)).then_some(lower)
}

/// `owner/name` or `name` -> (owner, name), both lowercased and validated.
pub fn split_qualified(input: &str) -> Option<(Option<String>, String)> {
    match input.trim().split_once('/') {
        Some((owner, name)) => {
            let owner = owner.to_lowercase();
            if !valid_part(&owner) {
                return None;
            }
            Some((Some(owner), normalize_name(name)?))
        }
        None => Some((None, normalize_name(input)?)),
    }
}

/// `#name` -> `name` lowercased, or None when invalid.
pub fn normalize_channel(input: &str) -> Option<String> {
    normalize_name(input.trim().strip_prefix('#')?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_normalize() {
        let a = Address::parse("codex:t-1", "Box").unwrap();
        assert_eq!(a.to_string(), "codex:t-1@box");
        let b = Address::parse("claude:4c06@other", "box").unwrap();
        assert_eq!(b.host, "other");
        assert!(Address::parse("nats:x", "box").is_err());
        assert!(Address::parse("claude:", "box").is_err());
        assert!(Address::parse("claude:a b", "box").is_err());
        let m = Address::parse("mailbox:01AGENT@Box", "x").unwrap();
        assert_eq!(
            (m.kind, m.to_string().as_str()),
            (Kind::Mailbox, "mailbox:01AGENT@box")
        );
        assert_eq!(Kind::parse(Kind::Mailbox.as_str()), Some(Kind::Mailbox));
        assert!(Address::parse_v1("mailbox:01AGENT@box", "box").is_err());
        assert_eq!(
            Address::parse_v1("codex:t-1", "box"),
            Address::parse("codex:t-1", "box")
        );
    }

    #[test]
    fn names() {
        assert_eq!(
            normalize_name("Kond.Dev.Claude").as_deref(),
            Some("kond.dev.claude")
        );
        assert_eq!(normalize_name("a..b"), None);
        assert_eq!(normalize_name("../x"), None);
        assert_eq!(
            split_qualified("Bob/x.y"),
            Some((Some("bob".into()), "x.y".into()))
        );
        assert_eq!(split_qualified("x"), Some((None, "x".into())));
        assert_eq!(
            normalize_channel("#Team.Review").as_deref(),
            Some("team.review")
        );
        assert_eq!(normalize_channel("team"), None);
    }
}
