//! Configuration checks: database aliases and URLs.

use super::{CheckIssue, has_managed_models};
use crate::connect::{backend_kind, url_scheme};
use crate::settings::{CliSettings, DEFAULT_DATABASE};
use siderite_orm::ModelMeta;
use url::Url;

pub(super) fn check(models: &[&'static ModelMeta], settings: &CliSettings) -> Vec<CheckIssue> {
    let mut issues = Vec::new();
    if has_managed_models(models) && settings.database_url(DEFAULT_DATABASE).is_none() {
        issues.push(CheckIssue::error(
            "config.E001",
            format!(
                "models are registered but no `{DEFAULT_DATABASE}` database is configured \
                 (set databases.{DEFAULT_DATABASE}.url or DATABASE_URL)"
            ),
        ));
    }
    for (alias, url) in settings.database_urls() {
        issues.extend(check_url(alias, url));
    }
    issues
}

/// Check one URL. Messages name the alias and scheme, never the URL.
pub(crate) fn check_url(alias: &str, url: &str) -> Option<CheckIssue> {
    let Some(scheme) = url_scheme(url) else {
        return Some(CheckIssue::error(
            "config.E002",
            format!("database `{alias}` has a URL without a scheme"),
        ));
    };
    if backend_kind(url).is_none() {
        return Some(CheckIssue::error(
            "config.E003",
            format!(
                "database `{alias}` uses the unsupported URL scheme `{scheme}` \
                 (expected sqlite, postgres, mysql, mongodb or redis)"
            ),
        ));
    }
    // SQLite URLs are file paths, not always valid URLs (`sqlite::memory:`).
    if scheme != "sqlite"
        && let Err(err) = Url::parse(url)
    {
        return Some(CheckIssue::error(
            "config.E002",
            format!("database `{alias}` has an invalid URL: {err}"),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(issues: &[CheckIssue]) -> Vec<&'static str> {
        issues.iter().map(|i| i.id).collect()
    }

    #[test]
    fn valid_urls_pass() {
        let settings = CliSettings::new()
            .database("default", "sqlite::memory:")
            .database("pg", "postgres://u:p@h:5432/db")
            .database("my", "mysql://u@h/db")
            .database("cache", "redis://h:6379")
            .database("mongo", "mongodb+srv://h/db");
        assert!(check(&[], &settings).is_empty());
    }

    #[test]
    fn missing_and_unknown_schemes_are_errors() {
        let settings = CliSettings::new()
            .database("a", "no-scheme-secret")
            .database("b", "ftp://u:hunter2@h/x");
        let issues = check(&[], &settings);
        assert_eq!(ids(&issues), ["config.E002", "config.E003"]);
        let text: String = issues.iter().map(|i| i.to_string()).collect();
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn unparsable_urls_are_errors_without_the_url() {
        let settings = CliSettings::new().database("pg", "postgres://u:hunter2@[bad/db");
        let issues = check(&[], &settings);
        assert_eq!(ids(&issues), ["config.E002"]);
        assert!(!issues[0].message.contains("hunter2"), "{}", issues[0]);
    }
}
