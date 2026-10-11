//! Names that generated code and paths may use.

use crate::error::CliError;

/// Rust keywords (strict and reserved, 2024 edition).
const KEYWORDS: &str = "abstract as async await become box break const \
    continue crate do dyn else enum extern false final fn for gen if impl \
    in let loop macro match mod move mut override priv pub ref return self \
    Self static struct super trait true try type typeof unsafe unsized use \
    virtual where while";

/// Whether `name` is a Rust keyword.
pub(crate) fn is_keyword(name: &str) -> bool {
    KEYWORDS.split_whitespace().any(|k| k == name)
}

/// A package name for `siderite new`: a lowercase letter, then
/// lowercase letters, digits, `-` and `_`.
///
/// Errors:
///   A usage error saying what is wrong.
pub(crate) fn package_name(name: &str) -> Result<(), CliError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(CliError::usage("project name is empty"));
    };
    if !first.is_ascii_lowercase() {
        return Err(CliError::usage(
            "project name must start with a lowercase letter",
        ));
    }
    let allowed = |c: char| {
        let symbol = c == '-' || c == '_';
        c.is_ascii_lowercase() || c.is_ascii_digit() || symbol
    };
    if !name.chars().all(allowed) {
        return Err(CliError::usage(
            "project name may contain only lowercase letters, digits, `-` \
             and `_`",
        ));
    }
    let ident = name.replace('-', "_");
    if is_keyword(&ident) {
        let msg = format!("`{name}` is a Rust keyword");
        return Err(CliError::usage(msg));
    }
    Ok(())
}

/// A module or field name for generated code: a lowercase letter, then
/// lowercase letters, digits and `_`, not a keyword.
///
/// Errors:
///   A usage error naming `what` and the problem.
pub(crate) fn snake_name(what: &str, name: &str) -> Result<(), CliError> {
    let first = name.chars().next();
    let lower = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let valid = first.is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| lower(c) || c == '_')
        && !name.ends_with('_')
        && !name.contains("__");
    if !valid {
        return Err(CliError::usage(format!(
            "{what} `{name}` must be snake_case: a lowercase letter, then \
             lowercase letters, digits and single `_`"
        )));
    }
    if is_keyword(name) {
        return Err(CliError::usage(format!("`{name}` is a Rust keyword")));
    }
    Ok(())
}

/// `blog_posts` as `BlogPosts`.
pub(crate) fn pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for part in name.split('_').filter(|p| !p.is_empty()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_identifiers() {
        assert!(package_name("demo-app_2").is_ok());
        for bad in ["", "Demo", "1demo", "de mo", "fn", "self", "a/b"] {
            assert!(package_name(bad).is_err(), "{bad}");
        }
        assert!(snake_name("route", "blog_posts2").is_ok());
        let bad = ["", "Items", "1x", "a-b", "x_", "a__b", "type", "é"];
        for name in bad {
            assert!(snake_name("route", name).is_err(), "{name}");
        }
        assert_eq!(pascal("blog_posts"), "BlogPosts");
        assert_eq!(pascal("items"), "Items");
    }
}
