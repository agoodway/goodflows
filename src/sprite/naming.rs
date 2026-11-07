//! Deterministic, project-scoped Sprite names and ownership records.
//!
//! A caller identity is opaque, case-sensitive data. It is never used as a
//! Sprite name or shell fragment: the name is `<prefix>-<32 hex>` from a
//! versioned, length-delimited SHA-256 over the prefix and identity, and the
//! exact values live in a private remote ownership record that every
//! operation verifies.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Ownership record schema and kind marker.
pub const OWNER_SCHEMA: u32 = 1;
pub const OWNER_KIND: &str = "goodflows-sprite-owner";
const NAME_DOMAIN: &[u8] = b"goodflows-sprite-name/v1";
const SCOPE_DOMAIN: &[u8] = b"goodflows-sprite-scope/v1";
pub const MAX_IDENTITY_BYTES: usize = 256;

/// Validate a caller identity: 1-256 UTF-8 bytes without control characters.
pub fn validate_identity(identity: &str) -> Result<()> {
    if identity.is_empty() {
        bail!("Sprite identity must not be empty");
    }
    if identity.len() > MAX_IDENTITY_BYTES {
        bail!("Sprite identity must be at most {MAX_IDENTITY_BYTES} UTF-8 bytes");
    }
    if identity.chars().any(char::is_control) {
        bail!("Sprite identity must not contain control characters");
    }
    Ok(())
}

/// clap value parser for `--sprite` and the recovery commands' identity.
pub fn parse_identity(value: &str) -> std::result::Result<String, String> {
    validate_identity(value)
        .map(|()| value.to_owned())
        .map_err(|e| e.to_string())
}

fn digest(domain: &[u8], parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u32).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// The exact selection of one owned environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub organization: String,
    pub prefix: String,
    pub identity: String,
}

impl Scope {
    /// Full name digest over the prefix and identity.
    pub fn name_digest(&self) -> String {
        digest(NAME_DOMAIN, &[&self.prefix, &self.identity])
    }

    /// The Sprite name: at most 20 + 1 + 32 = 53 bytes, inside the platform's
    /// 3-63 byte lowercase/digit/hyphen rule, starting and ending alphanumeric.
    pub fn sprite_name(&self) -> String {
        format!("{}-{}", self.prefix, &self.name_digest()[..32])
    }

    /// Digest over organization, prefix and identity. Keys the remote caller
    /// lock and the local result bundle directory.
    pub fn digest(&self) -> String {
        digest(
            SCOPE_DOMAIN,
            &[&self.organization, &self.prefix, &self.identity],
        )
    }

    pub fn owner(&self, sprite_id: Option<String>, created_at: String) -> Owner {
        Owner {
            schema: OWNER_SCHEMA,
            kind: OWNER_KIND.into(),
            organization: self.organization.clone(),
            prefix: self.prefix.clone(),
            identity: self.identity.clone(),
            name_digest: self.name_digest(),
            scope_digest: self.digest(),
            name: self.sprite_name(),
            sprite_id,
            created_at,
        }
    }
}

/// `~/.goodflows/owner.json` on the Sprite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub schema: u32,
    pub kind: String,
    pub organization: String,
    pub prefix: String,
    pub identity: String,
    pub name_digest: String,
    pub scope_digest: String,
    pub name: String,
    /// Stable platform resource identifier captured at creation, when known.
    pub sprite_id: Option<String>,
    pub created_at: String,
}

impl Owner {
    /// Every field except the creation time and resource id must match the
    /// requested scope exactly; a truncated-name collision fails here.
    pub fn verify(&self, scope: &Scope) -> Result<()> {
        if self.schema != OWNER_SCHEMA || self.kind != OWNER_KIND {
            bail!(
                "unsupported GoodFlows ownership record (schema {})",
                self.schema
            );
        }
        let expected = scope.owner(None, String::new());
        if self.organization != expected.organization
            || self.prefix != expected.prefix
            || self.identity != expected.identity
            || self.name_digest != expected.name_digest
            || self.scope_digest != expected.scope_digest
            || self.name != expected.name
        {
            bail!(
                "Sprite {} is owned by a different GoodFlows scope; refusing to use or modify it",
                expected.name
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(prefix: &str, identity: &str) -> Scope {
        Scope {
            organization: "org".into(),
            prefix: prefix.into(),
            identity: identity.into(),
        }
    }

    #[test]
    fn names_are_deterministic_bounded_and_platform_valid() {
        let a = scope("goodflows", "T-Example.1");
        assert_eq!(a.sprite_name(), a.sprite_name());
        for s in [
            a,
            scope("a", "x"),
            scope("abcdefghij0123456789", &"x".repeat(256)),
        ] {
            let name = s.sprite_name();
            assert!((3..=63).contains(&name.len()), "{name}");
            assert!(name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
            assert!(name.as_bytes()[name.len() - 1].is_ascii_alphanumeric());
        }
    }

    #[test]
    fn case_punctuation_and_prefix_never_merge() {
        let names: Vec<_> = [
            scope("gf", "T-abc"),
            scope("gf", "t-abc"),
            scope("gf", "T_abc"),
            scope("gf", "Tabc"),
            scope("other", "T-abc"),
        ]
        .iter()
        .map(Scope::sprite_name)
        .collect();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        // Length-delimited: moving bytes between prefix and identity changes the name.
        assert_ne!(
            scope("ab", "c").name_digest(),
            scope("a", "bc").name_digest()
        );
    }

    #[test]
    fn ownership_verification_is_exact() {
        let s = scope("gf", "T-1");
        let owner = s.owner(Some("id".into()), "now".into());
        owner.verify(&s).unwrap();
        assert!(owner.verify(&scope("gf", "T-2")).is_err());
        let mut other_org = s.clone();
        other_org.organization = "elsewhere".into();
        assert!(owner.verify(&other_org).is_err());
    }

    #[test]
    fn identities_are_validated() {
        assert!(validate_identity("").is_err());
        assert!(validate_identity("a\nb").is_err());
        assert!(validate_identity(&"é".repeat(129)).is_err());
        validate_identity("T-019a Thread/ID: ok").unwrap();
    }
}
