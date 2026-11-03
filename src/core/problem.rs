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
    /// What this kind of outcome is called, for the short line a view or a report leads with
    /// before the detail of what went wrong. Every feature names the same situation alike.
    pub fn label(self) -> &'static str {
        crate::i18n::tr(match self {
            Problem::Ambiguous => "problem.ambiguous",
            Problem::Stale => "problem.stale",
            Problem::Unsupported => "problem.unsupported",
            Problem::ReadFailure => "problem.read_failure",
            Problem::WriteFailure => "problem.write_failure",
            Problem::Canceled => "problem.canceled",
            Problem::RecoveryNeeded => "problem.recovery_needed",
        })
    }

    /// The problem a table or key that could not be used as asked is. A key column that is
    /// missing or repeated is a request this comparison cannot carry out, not a reading of the
    /// data that might be wrong, so it is `Unsupported` rather than `Ambiguous`; duplicate keys
    /// among the *records* are a warning on a match that was still made, and the match marks
    /// those rows ambiguous itself.
    pub fn of_table(problem: &super::table_match::Problem) -> Problem {
        use super::table_match::Problem as Table;
        match problem {
            Table::Cancelled => Problem::Canceled,
            Table::Failed => Problem::ReadFailure,
            Table::MissingKey { .. }
            | Table::DuplicateKeyColumn { .. }
            | Table::NoHeader
            | Table::TooLarge => Problem::Unsupported,
        }
    }

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

    #[test]
    fn a_table_that_cannot_be_matched_as_asked_is_named_in_the_same_taxonomy() {
        use crate::core::table_match::Problem as Table;
        assert_eq!(Problem::of_table(&Table::Cancelled), Problem::Canceled);
        assert_eq!(Problem::of_table(&Table::NoHeader), Problem::Unsupported);
        assert_eq!(Problem::of_table(&Table::TooLarge), Problem::Unsupported);
        assert_eq!(Problem::of_table(&Table::Failed), Problem::ReadFailure);
        assert_eq!(
            Problem::of_table(&Table::MissingKey {
                side: 0,
                column: "id".into()
            }),
            Problem::Unsupported
        );
    }

    #[test]
    fn every_kind_of_outcome_has_a_name_of_its_own() {
        let all = [
            Problem::Ambiguous,
            Problem::Stale,
            Problem::Unsupported,
            Problem::ReadFailure,
            Problem::WriteFailure,
            Problem::Canceled,
            Problem::RecoveryNeeded,
        ];
        let mut names: Vec<&str> = all.iter().map(|p| p.label()).collect();
        for (p, name) in all.iter().zip(&names) {
            assert!(
                name.chars().any(char::is_alphanumeric) && !name.starts_with("problem."),
                "{p:?} has no name: {name:?}"
            );
        }
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two kinds share a name: {names:?}");
    }
}
