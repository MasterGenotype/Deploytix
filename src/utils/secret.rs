//! A string newtype that never prints its contents.
//!
//! Config secrets — LUKS passphrases, account passwords, Wi-Fi PSKs — are
//! legitimately *serialised* (a config file has to be able to carry them),
//! but they must never reach a log line, a `{:?}` dump, an
//! [`OperationRecord`](crate::utils::command::OperationRecord), or a
//! rehearsal report.  `DeploymentConfig` derives `Debug`, so a single
//! `debug!("{:?}", config)` anywhere would leak every credential at once.
//!
//! Wrapping the field type rather than hand-writing `Debug` for each
//! containing struct means a newly added secret field is redacted by
//! construction: you cannot forget to update a `Debug` impl, because there
//! isn't one to update.
//!
//! Serialisation is `#[serde(transparent)]`, so a `Secret` round-trips
//! through TOML as a plain string and existing config files keep working.
//!
//! There is deliberately **no** `Display` impl: `format!("{}", secret)`
//! does not compile, so reaching the plaintext is always an explicit
//! `.as_str()` (or an explicit deref).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A string whose `Debug` representation is redacted.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the plaintext.  Every call site that needs the real value
    /// goes through here, which makes secret use greppable.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and yield the plaintext.
    #[allow(dead_code)]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Distinguishing empty from set is safe and makes "you forgot to
        // set a passphrase" debuggable without revealing anything.
        if self.0.is_empty() {
            f.write_str("<empty>")
        } else {
            f.write_str("<redacted>")
        }
    }
}

impl std::ops::Deref for Secret {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_contents() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{:?}", s), "<redacted>");
        assert!(!format!("{:?}", s).contains("hunter2"));
    }

    #[test]
    fn debug_distinguishes_empty() {
        assert_eq!(format!("{:?}", Secret::new("")), "<empty>");
    }

    #[test]
    fn debug_of_containing_struct_redacts() {
        #[derive(Debug)]
        struct Holder {
            name: String,
            password: Secret,
        }
        let h = Holder {
            name: "tester".to_string(),
            password: Secret::new("s3cret"),
        };
        let rendered = format!("{:?}", h);
        assert!(rendered.contains("tester"));
        assert!(!rendered.contains("s3cret"));
    }

    #[test]
    fn debug_of_option_redacts() {
        let s = Some(Secret::new("s3cret"));
        assert!(!format!("{:?}", s).contains("s3cret"));
    }

    #[test]
    fn as_str_yields_plaintext() {
        assert_eq!(Secret::new("hunter2").as_str(), "hunter2");
    }

    #[test]
    fn serialises_transparently_as_a_plain_string() {
        #[derive(Serialize, Deserialize)]
        struct Holder {
            password: Secret,
        }
        let toml_text = toml::to_string(&Holder {
            password: Secret::new("hunter2"),
        })
        .unwrap();
        assert_eq!(toml_text.trim(), r#"password = "hunter2""#);

        // Existing config files carry a bare string; it must still parse.
        let back: Holder = toml::from_str(r#"password = "hunter2""#).unwrap();
        assert_eq!(back.password.as_str(), "hunter2");
    }

    #[test]
    fn deref_gives_str_methods() {
        let s = Secret::new("hunter2");
        assert_eq!(s.len(), 7);
        assert!(!s.is_empty());
        assert!(Secret::new("").is_empty());
    }
}
