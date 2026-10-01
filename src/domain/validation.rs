use std::{net::IpAddr, time::Duration};

/// Bounded to keep scheduling and date arithmetic representable.
pub const MAX_DURATION: Duration = Duration::from_secs(10 * 366 * 24 * 60 * 60);

pub fn validate_duration(value: Duration) -> Result<(), &'static str> {
    if value < Duration::from_millis(1) || value > MAX_DURATION {
        Err("duration must be between 1ms and 10 years (3660 days)")
    } else {
        Ok(())
    }
}

/// Inventory destinations are IP literals or DNS/SSH host aliases, never command options.
pub fn validate_address(value: &str) -> Result<(), &'static str> {
    if value.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > 253
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    {
        Err("address must be an IP literal or DNS/SSH host alias")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    #[rstest]
    #[case("-oProxyCommand=example")]
    #[case("host.example argument")]
    #[case("host\nexample")]
    #[case("")]
    fn rejects_unsafe_destinations(#[case] address: &str) {
        assert!(validate_address(address).is_err());
    }
    #[rstest]
    #[case("192.0.2.1")]
    #[case("2001:db8::1")]
    #[case("host.example")]
    fn accepts_destinations(#[case] address: &str) {
        assert!(validate_address(address).is_ok());
    }
}
