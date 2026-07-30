use serde::Deserialize;
use std::fmt;

/// Bearer token used to authenticate API requests. Redacts itself in `Debug`
/// so structures like `AppConfig` and `ApiState` are safe to log.
///
/// Compare via `expose_bytes()` with a constant-time function — never expose
/// the inner string directly to a non-comparison code path.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct ApiToken(String);

impl ApiToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw bytes — intended only for constant-time comparison.
    pub fn expose_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

impl From<&str> for ApiToken {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ApiToken {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let t = ApiToken::new("supersecret");
        let s = format!("{:?}", t);
        assert!(!s.contains("supersecret"));
        assert!(s.contains("redacted"));
    }

    #[test]
    fn expose_bytes_round_trips() {
        let t = ApiToken::new("abc");
        assert_eq!(t.expose_bytes(), b"abc");
    }

    #[test]
    fn deserializes_from_plain_string() {
        let t: ApiToken = serde_json::from_str("\"plain\"").unwrap();
        assert_eq!(t.expose_bytes(), b"plain");
    }

    #[test]
    fn detects_blank_tokens() {
        assert!(ApiToken::new("").is_blank());
        assert!(ApiToken::new(" \t").is_blank());
        assert!(!ApiToken::new("secret").is_blank());
    }
}
