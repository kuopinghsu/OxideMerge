//! The kinds of outcome every feature reports when it cannot simply succeed, so the folder
//! sync, the semantic and Tree views and the CLI say the same thing for the same situation.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// Another reading of the input is as likely, such as a pairing of repeated items; the
    /// result stands but is to be checked by hand. The semantic and Tree views mark such
    /// changes and rows with their own `ambiguous` flag.
    #[allow(dead_code)]
    Ambiguous,
    /// The input is no longer as it was when the result or plan was made.
    Stale,
    /// The input or request is of a kind this feature refuses: a format, version, file type
    /// or place it does not handle.
    Unsupported,
    ReadFailure,
    /// Writing failed, or would: no permission, or no room.
    WriteFailure,
    /// Stopped by the user.
    Canceled,
    /// A change was cut short and could not be wholly undone; a recovery journal is kept.
    RecoveryNeeded,
}

impl Problem {
    /// The problem an I/O error is, when reading (`writing` false) or writing.
    pub fn of_io(error: &std::io::Error, writing: bool) -> Problem {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::Interrupted => Problem::Canceled,
            ErrorKind::Unsupported | ErrorKind::InvalidInput | ErrorKind::InvalidData => {
                Problem::Unsupported
            }
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded | ErrorKind::ReadOnlyFilesystem => {
                Problem::WriteFailure
            }
            _ if writing => Problem::WriteFailure,
            _ => Problem::ReadFailure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn io_errors_map_by_kind_and_direction() {
        let e = |k: ErrorKind| Error::from(k);
        assert_eq!(
            Problem::of_io(&e(ErrorKind::StorageFull), false),
            Problem::WriteFailure
        );
        assert_eq!(
            Problem::of_io(&e(ErrorKind::PermissionDenied), false),
            Problem::ReadFailure
        );
        assert_eq!(
            Problem::of_io(&e(ErrorKind::PermissionDenied), true),
            Problem::WriteFailure
        );
        assert_eq!(
            Problem::of_io(&e(ErrorKind::Interrupted), true),
            Problem::Canceled
        );
        assert_eq!(
            serde_json::to_string(&Problem::RecoveryNeeded).unwrap(),
            "\"recovery_needed\""
        );
    }
}
