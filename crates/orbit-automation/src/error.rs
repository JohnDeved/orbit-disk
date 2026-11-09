use std::fmt;

/// An application-level failure reported as `{"ok":false,"error":{code,message,outcomeUnknown?}}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
    /// The command may have executed; clients must check state instead of retrying.
    pub outcome_unknown: bool,
}

impl Failure {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), outcome_unknown: false }
    }

    pub fn uncertain(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { outcome_unknown: true, ..Self::new(code, message) }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// Local transport failures (Swift `LocalAutomationError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// No app is listening.
    Unavailable,
    /// Another server holds the endpoint lock or a live listener owns the socket.
    AlreadyRunning,
    InvalidEndpoint(&'static str),
    /// The peer process belongs to another user.
    PeerMismatch,
    OversizedMessage,
    Timeout,
    Disconnected,
    /// An OS call failed; the second field is the raw OS error.
    System(&'static str, i32),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Orbit Disk is not running. Open the app and try again."),
            Self::AlreadyRunning => f.write_str("Orbit Disk automation is already running."),
            Self::InvalidEndpoint(reason) => write!(f, "The automation endpoint is unavailable: {reason}"),
            Self::PeerMismatch => f.write_str("The automation connection belongs to another user."),
            Self::OversizedMessage => f.write_str("Automation messages must not exceed 1 MiB."),
            Self::Timeout => f.write_str("The automation request timed out."),
            Self::Disconnected => f.write_str("The automation connection closed before the response was complete."),
            Self::System(operation, code) => write!(f, "{operation}: {}", os_message(*code)),
        }
    }
}

impl std::error::Error for TransportError {}

/// `strerror` text without Rust's ` (os error N)` suffix.
pub(crate) fn os_message(code: i32) -> String {
    let text = std::io::Error::from_raw_os_error(code).to_string();
    match text.rfind(" (os error ") {
        Some(index) => text[..index].to_owned(),
        None => text,
    }
}

impl From<TransportError> for Failure {
    fn from(error: TransportError) -> Self {
        const UNCERTAIN: &str = " The command may already have been accepted. Check status and collection; do not automatically retry a mutation.";
        let message = error.to_string();
        let code = match error {
            TransportError::Timeout => "request_timeout",
            TransportError::Disconnected => "connection_closed",
            TransportError::System(..) => "connection_failed",
            TransportError::OversizedMessage => "message_too_large",
            TransportError::InvalidEndpoint(_) => return Failure::new("invalid_endpoint", message),
            TransportError::PeerMismatch => return Failure::new("peer_mismatch", message),
            TransportError::AlreadyRunning => return Failure::new("already_running", message),
            TransportError::Unavailable => return Failure::new("unavailable", message),
        };
        Failure::uncertain(code, message + UNCERTAIN)
    }
}
