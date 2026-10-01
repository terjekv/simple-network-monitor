use serde::{Deserialize, Deserializer, de};
use std::fmt;

/// Validated HTTP bearer credential. Debug and validation errors never contain its value.
#[derive(Clone)]
pub struct ApiToken(String);

#[derive(Debug, thiserror::Error)]
#[error("api_token must contain 1..4096 visible ASCII characters without whitespace")]
pub struct InvalidApiToken;

impl ApiToken {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidApiToken> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 4096
            || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(InvalidApiToken);
        }
        Ok(Self(value))
    }

    pub fn expose_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for ApiToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn debug_is_redacted() {
        let token = ApiToken::new("fake-example-token").unwrap();
        assert_eq!(format!("{token:?}"), "ApiToken(<redacted>)");
        assert_eq!(token.expose_bytes(), b"fake-example-token");
    }

    #[rstest]
    #[case("")]
    #[case(" ")]
    #[case("fake\nvalue")]
    #[case("nonascii-æ")]
    fn rejects_invalid_tokens(#[case] value: &str) {
        assert!(ApiToken::new(value).is_err());
    }

    #[test]
    fn validates_deserialization() {
        assert!(serde_json::from_str::<ApiToken>("\"\"").is_err());
        assert!(serde_json::from_str::<ApiToken>("\"fake-token\"").is_ok());
    }
}
