//! The single redaction helper for report fields.
//!
//! Secret-looking keys use `siderite_config::is_secret_key`, the same
//! rule the configuration errors apply.

use siderite_config::is_secret_key;

/// What a redacted value is replaced with.
pub const REDACTED: &str = "***";

/// Redact the value of a field called `key`: all of it when the key looks
/// secret, otherwise any connection-string credentials inside it.
pub fn redact_field(key: &str, value: &str) -> String {
    if is_secret_key(key) {
        REDACTED.to_owned()
    } else {
        redact_text(value)
    }
}

/// Redact every connection string in `text`.
///
/// For each `scheme://...` token the password of the userinfo (or the
/// whole userinfo when it has no `user:` part) and the value of every
/// secret-looking query parameter are replaced by [`REDACTED`]. Text
/// without a `://` is returned unchanged.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("://") {
        let (head, tail) = rest.split_at(pos);
        let scheme_start = head
            .char_indices()
            .rev()
            .find(|(_, c)| !is_scheme_char(*c))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let (before, scheme) = head.split_at(scheme_start);
        out.push_str(before);
        out.push_str(scheme);
        out.push_str("://");
        let after = &tail[3..];
        let end = after.find(is_url_end).unwrap_or(after.len());
        out.push_str(&redact_authority_and_query(&after[..end]));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

fn is_url_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>')
}

fn redact_authority_and_query(rest: &str) -> String {
    let (rest, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (before_query, query) = match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let authority_end = before_query.find('/').unwrap_or(before_query.len());
    let (authority, path) = before_query.split_at(authority_end);
    let mut out = String::new();
    if let Some(at) = authority.rfind('@') {
        let userinfo = &authority[..at];
        match userinfo.split_once(':') {
            Some((user, _)) => {
                out.push_str(user);
                out.push(':');
                out.push_str(REDACTED);
            }
            None => out.push_str(REDACTED),
        }
        out.push_str(&authority[at..]);
    } else {
        out.push_str(authority);
    }
    out.push_str(path);
    if let Some(query) = query {
        out.push('?');
        let pairs: Vec<String> = query.split('&').map(redact_pair).collect();
        out.push_str(&pairs.join("&"));
    }
    out.push_str(fragment);
    out
}

fn redact_pair(pair: &str) -> String {
    match pair.split_once('=') {
        Some((key, _)) if is_secret_key(key) => format!("{key}={REDACTED}"),
        _ => pair.to_owned(),
    }
}
