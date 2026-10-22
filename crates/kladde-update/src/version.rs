use crate::error::{ensure, UpdateError};

/// Parses `MAJOR.MINOR.PATCH` with an optional leading `v`; ordering of the tuple is version ordering.
pub(crate) fn parse_version(text: &str) -> Result<(u64, u64, u64), UpdateError> {
    let invalid = || UpdateError::InvalidVersion(text.to_string());
    let trimmed = text.trim();
    let bare = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let parts = bare
        .split('.')
        .map(|p| {
            ensure(!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()), invalid)?;
            p.parse::<u64>().map_err(|_| invalid())
        })
        .collect::<Result<Vec<u64>, UpdateError>>()?;
    match parts[..] {
        [major, minor, patch] => Ok((major, minor, patch)),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn parses_and_orders_numerically() {
        assert_eq!(parse_version("1.2.3").unwrap(), (1, 2, 3));
        assert_eq!(parse_version("v0.10.0").unwrap(), (0, 10, 0));
        assert!(parse_version("0.10.0").unwrap() > parse_version("0.9.9").unwrap());
    }

    #[test]
    fn rejects_malformed() {
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "1.2.3-beta", "V1.2.3", "1..3", "+1.2.3", "1.2.99999999999999999999"]
        {
            assert!(parse_version(bad).is_err(), "{bad}");
        }
    }
}
