use std::time::Duration;

use crate::error::{ensure, UpdateError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("kladde-update/", env!("CARGO_PKG_VERSION"));

/// Accepts https, and plain http only to the local machine (tests, local mirrors).
pub(crate) fn check_url(url: &str) -> Result<(), UpdateError> {
    let lower = url.to_ascii_lowercase();
    ensure(lower.starts_with("https://") || is_loopback_http(&lower), || UpdateError::InsecureUrl(url.to_string()))
}

fn is_loopback_http(lower: &str) -> bool {
    ["http://127.0.0.1", "http://localhost"]
        .iter()
        .filter_map(|prefix| lower.strip_prefix(prefix))
        .any(|rest| rest.is_empty() || rest.starts_with([':', '/']))
}

/// Agent for `url`; https requests refuse redirects to http, loopback requests bypass proxies.
/// `total` bounds the whole exchange; downloads pass `None` because their duration depends on the line.
pub(crate) fn agent(url: &str, total: Option<Duration>) -> ureq::Agent {
    let lower = url.to_ascii_lowercase();
    let builder = ureq::Agent::config_builder()
        .https_only(lower.starts_with("https://"))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_global(total)
        .user_agent(USER_AGENT);
    let builder = if is_loopback_http(&lower) { builder.proxy(None) } else { builder };
    builder.build().into()
}

pub(crate) fn net_error(e: ureq::Error) -> UpdateError {
    match e {
        ureq::Error::StatusCode(code) => UpdateError::Http(code),
        other => UpdateError::Network(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::check_url;

    #[test]
    fn only_https_or_loopback() {
        for ok in ["https://github.com/x", "HTTPS://example.org", "http://127.0.0.1:8080/a", "http://localhost/a"] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in ["http://example.org/a", "http://localhost.evil.org/a", "http://127.0.0.1.evil.org", "ftp://x", "x"]
        {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }
}
