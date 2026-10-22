use std::fmt;

/// Errors of workspace, settings and session operations.
#[derive(Debug)]
pub enum WsError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(String),
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsError::Io(e) => write!(f, "I/O error: {e}"),
            WsError::Json(e) => write!(f, "JSON error: {e}"),
            WsError::Invalid(msg) => write!(f, "invalid: {msg}"),
        }
    }
}

impl std::error::Error for WsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WsError::Io(e) => Some(e),
            WsError::Json(e) => Some(e),
            WsError::Invalid(_) => None,
        }
    }
}

impl From<std::io::Error> for WsError {
    fn from(e: std::io::Error) -> Self {
        WsError::Io(e)
    }
}

impl From<serde_json::Error> for WsError {
    fn from(e: serde_json::Error) -> Self {
        WsError::Json(e)
    }
}

/// `Err(Invalid(msg))` unless `cond` holds; keeps call sites free of early returns.
pub(crate) fn ensure(cond: bool, msg: &str) -> Result<(), WsError> {
    if cond {
        Ok(())
    } else {
        Err(WsError::Invalid(msg.to_string()))
    }
}
