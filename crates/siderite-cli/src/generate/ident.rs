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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_identifiers() {
        assert!(package_name("demo-app_2").is_ok());
        for bad in ["", "Demo", "1demo", "de mo", "fn", "self", "a/b"] {
            assert!(package_name(bad).is_err(), "{bad}");
        }
    }
}
