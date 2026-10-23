//! A single structured error type shared by every Caffeine crate.
//!
//! Errors carry a [`ErrorKind`] that maps to a documented process exit code,
//! a human-readable message, an optional remediation hint and, when the
//! failure came from the operating system, the raw OS error code. Nothing in
//! Caffeine should ever turn an OS failure into a success; this type is how
//! failures travel up to the CLI unchanged.

use std::fmt;

use serde::Serialize;

/// Broad classification of a failure. Each kind maps to a stable exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    /// Generic failure (exit code 1).
    Failure,
    /// Invalid command-line usage or invalid argument value (exit code 2).
    Usage,
    /// The requested operation is not supported on this platform or
    /// hardware (exit code 4).
    Unsupported,
    /// The operating system refused the operation (exit code 5).
    PermissionDenied,
    /// The request conflicts with current state, e.g. a session with
    /// different parameters is already running (exit code 6).
    Conflict,
    /// The configuration file is missing, malformed or invalid (exit code 7).
    Config,
    /// A plugin failed validation, trust checks or execution (exit code 8).
    Plugin,
    /// A named object (plugin, setting, file) does not exist (exit code 9).
    NotFound,
}

impl ErrorKind {
    /// The process exit code associated with this kind of error.
    ///
    /// Exit code 3 is intentionally absent: it is reserved for
    /// `caffeine status` reporting that no session is active, mirroring
    /// `systemctl is-active`.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::Failure => 1,
            ErrorKind::Usage => 2,
            ErrorKind::Unsupported => 4,
            ErrorKind::PermissionDenied => 5,
            ErrorKind::Conflict => 6,
            ErrorKind::Config => 7,
            ErrorKind::Plugin => 8,
            ErrorKind::NotFound => 9,
        }
    }

    /// Stable, kebab-case identifier used in JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Failure => "failure",
            ErrorKind::Usage => "usage",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::PermissionDenied => "permission-denied",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Config => "config",
            ErrorKind::Plugin => "plugin",
            ErrorKind::NotFound => "not-found",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The error type used throughout Caffeine.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    hint: Option<String>,
    os_code: Option<i64>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

/// Convenience alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Create an error of the given kind.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
            hint: None,
            os_code: None,
            source: None,
        }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Failure, message)
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    pub fn permission(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PermissionDenied, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, message)
    }

    pub fn plugin(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Plugin, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    /// Wrap an I/O error, classifying permission errors correctly.
    pub fn io(context: impl Into<String>, err: std::io::Error) -> Self {
        let kind = match err.kind() {
            std::io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound => ErrorKind::NotFound,
            _ => ErrorKind::Failure,
        };
        let os_code = err.raw_os_error().map(i64::from);
        let mut e = Error::new(kind, format!("{}: {}", context.into(), err));
        e.os_code = os_code;
        e.source = Some(Box::new(err));
        e
    }

    /// Attach a remediation hint shown below the error message.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attach the raw operating-system error code.
    pub fn with_os_code(mut self, code: i64) -> Self {
        self.os_code = Some(code);
        self
    }

    /// Attach an underlying error.
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Prefix the message with additional context.
    pub fn context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }

    pub fn os_code(&self) -> Option<i64> {
        self.os_code
    }

    pub fn exit_code(&self) -> i32 {
        self.kind.exit_code()
    }

    /// A serializable view of this error for JSON output.
    pub fn to_report(&self) -> ErrorReport {
        ErrorReport {
            kind: self.kind,
            message: self.message.clone(),
            hint: self.hint.clone(),
            os_code: self.os_code,
            exit_code: self.exit_code(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(code) = self.os_code {
            // Only append the code if the message does not already mention it.
            if !self.message.contains(&code.to_string()) {
                write!(f, " (os error {code})")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|s| s.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// Serializable error representation used by `--json` output.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorReport {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_code: Option<i64>,
    pub exit_code: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinct_and_skip_three() {
        let kinds = [
            ErrorKind::Failure,
            ErrorKind::Usage,
            ErrorKind::Unsupported,
            ErrorKind::PermissionDenied,
            ErrorKind::Conflict,
            ErrorKind::Config,
            ErrorKind::Plugin,
            ErrorKind::NotFound,
        ];
        let mut codes: Vec<i32> = kinds.iter().map(|k| k.exit_code()).collect();
        assert!(!codes.contains(&0));
        assert!(!codes.contains(&3));
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), kinds.len());
    }

    #[test]
    fn io_permission_errors_are_classified() {
        let err = Error::io(
            "writing config",
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
        assert!(err.message().starts_with("writing config: "));
    }

    #[test]
    fn display_appends_os_code_once() {
        let err = Error::failure("PowerSetRequest failed").with_os_code(5);
        assert_eq!(err.to_string(), "PowerSetRequest failed (os error 5)");
        let err = Error::failure("failed with error 5").with_os_code(5);
        assert_eq!(err.to_string(), "failed with error 5");
    }
}
