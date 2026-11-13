//! Foreign-key bookkeeping around SQLite schema changes, shared by the SQLx
//! and native backends.
//!
//! SQLite ignores `PRAGMA foreign_keys` inside a transaction, so a schema
//! connection turns the pragma off *before* `BEGIN` and restores it after
//! commit or rollback.
//!
//! The pre-commit `foreign_key_check` is scoped to violations the schema
//! change introduced: the rows at open are the baseline, and commit fails
//! only on rows that are not in it. Pre-existing violations in unrelated
//! tables must not block every future migration, and comparing rows (not
//! counts) means a change that fixes one violation while adding another
//! still fails. When foreign keys were already off the check is skipped
//! entirely.

use siderite_orm::{BackendError, OrmError};
use std::collections::HashSet;

/// Statement listing every current foreign-key violation.
pub(super) const FOREIGN_KEY_CHECK: &str = "PRAGMA foreign_key_check";

/// Statement reading whether foreign keys are enforced (`0` or `1`).
pub(super) const FOREIGN_KEYS: &str = "PRAGMA foreign_keys";

/// One `PRAGMA foreign_key_check` row: the offending child row and the parent
/// it points at. Identifies a violation independently of when it appeared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct FkViolation {
    table: String,
    rowid: i64,
    parent: String,
}

impl FkViolation {
    /// A missing or wrong-typed column becomes a placeholder, so an unreadable
    /// row still counts as a violation instead of being silently dropped.
    ///
    /// Args:
    ///     table: The `table` column, if readable as text.
    ///     rowid: The `rowid` column, if readable as an integer (it is
    ///         `NULL` for `WITHOUT ROWID` tables).
    ///     parent: The `parent` column, if readable as text.
    pub(super) fn new(table: Option<String>, rowid: Option<i64>, parent: Option<String>) -> Self {
        Self {
            table: table.unwrap_or_else(|| "<unreadable table>".to_owned()),
            rowid: rowid.unwrap_or(-1),
            parent: parent.unwrap_or_else(|| "<unreadable parent>".to_owned()),
        }
    }

    fn location(&self) -> String {
        format!(
            "{} row {} (missing parent {})",
            self.table, self.rowid, self.parent
        )
    }
}

/// The SQL that sets `PRAGMA foreign_keys` back to `on` (`0` is off).
pub(super) fn set_foreign_keys_sql(on: i64) -> &'static str {
    if on == 0 {
        "PRAGMA foreign_keys = OFF"
    } else {
        "PRAGMA foreign_keys = ON"
    }
}

/// Violations in `current` that are not in `baseline`, each once.
///
/// A row-level diff, not a count: a change that repairs one old violation
/// while adding another keeps the count equal, and would wrongly pass.
pub(super) fn new_violations(
    baseline: &HashSet<FkViolation>,
    current: impl IntoIterator<Item = FkViolation>,
) -> Vec<FkViolation> {
    let mut seen = HashSet::new();
    current
        .into_iter()
        .filter(|violation| !baseline.contains(violation) && seen.insert(violation.clone()))
        .collect()
}

/// The commit failure for `fresh` violations. Without a transaction
/// (`in_txn == false`) the statements are already committed; the message
/// says so rather than implying nothing happened.
pub(super) fn violation_error(fresh: &[FkViolation], in_txn: bool) -> OrmError {
    let outcome = if in_txn {
        "rolled back"
    } else {
        "already committed; fix the rows by hand"
    };
    BackendError::Constraint(format!(
        "SQLite foreign_key_check found {} new violation(s) after schema change ({outcome}): {}",
        fresh.len(),
        violation_summary(fresh),
    ))
    .into()
}

/// One-line summary of new `PRAGMA foreign_key_check` violations.
fn violation_summary(violations: &[FkViolation]) -> String {
    let mut locations: Vec<String> = violations.iter().map(FkViolation::location).collect();
    locations.sort();
    locations.truncate(5);
    if locations.is_empty() {
        "see PRAGMA foreign_key_check".to_owned()
    } else {
        locations.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(table: &str, rowid: i64) -> FkViolation {
        FkViolation::new(Some(table.into()), Some(rowid), Some("parent".into()))
    }

    #[test]
    fn only_new_rows_count_and_each_once() {
        let baseline: HashSet<_> = [violation("a", 1)].into();
        let current = [violation("a", 1), violation("b", 2), violation("b", 2)];
        assert_eq!(new_violations(&baseline, current), [violation("b", 2)]);
    }

    #[test]
    fn unreadable_columns_become_placeholders() {
        let unreadable = FkViolation::new(None, None, None);
        assert_eq!(unreadable.rowid, -1);
        let message = violation_error(&[unreadable], true).to_string();
        assert!(message.contains("<unreadable table> row -1"), "{message}");
        assert!(message.contains("rolled back"), "{message}");
    }
}
