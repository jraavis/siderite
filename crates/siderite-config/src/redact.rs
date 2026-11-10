//! The single redaction helper: configuration errors, the configuration
//! explanation and every CLI report use it.
//!
//! Secret-looking keys use [`is_secret_key`], the
//! same rule the configuration errors apply. Redaction errs toward hiding
//! too much: a false positive only hides part of a message, a false
//! negative leaks a credential.

use crate::error::is_secret_key;

/// What a redacted value is replaced with.
pub const REDACTED: &str = "***";

/// Redact the value of a field called `key`: all of it when the key looks
/// secret, otherwise any credentials inside it (see [`redact_text`]).
///
/// Args:
///   key: The field name.
///   value: The field value.
///
/// Returns:
///   The value safe to print.
pub fn redact_field(key: &str, value: &str) -> String {
    if is_secret_key(key) {
        REDACTED.to_owned()
    } else {
        redact_text(value)
    }
}

/// Redact every credential found in `text`.
///
/// Two passes run in order:
///
/// 1. **URLs.** In each `scheme://` token the password of the userinfo
///    (or the whole userinfo when it has no `user:` part) becomes
///    [`REDACTED`]. The userinfo ends at the *last* `@` before the end
///    of the line or the next `scheme://`, so a password holding an
///    unencoded `/ ? # @`, space or quote is still hidden. A `scheme://`
///    glued on with `:` (`user:https://secret@host`) is part of that
///    password, not another URL. A comma separated list of URLs is
///    redacted URL by URL.
/// 2. **`key=value` pairs** anywhere in the text: URL query parameters,
///    libpq DSNs (`password=x`), ADO strings (`;Pwd=x;`) and the like.
///    When `is_secret_key(key)` holds, the value becomes [`REDACTED`].
///    A quoted value is redacted up to its closing quote; an unquoted
///    one up to whitespace, `;` or `&`.
///
/// Args:
///   text: Free text that may hold connection strings.
///
/// Returns:
///   The text with every recognised secret replaced.
pub fn redact_text(text: &str) -> String {
    redact_pairs(&redact_urls(text))
}

fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("://") {
        let (head, tail) = rest.split_at(pos);
        out.push_str(head);
        out.push_str("://");
        let after = &tail[3..];
        let region = &after[..region_end(after)];
        let consumed = push_userinfo(&mut out, region);
        rest = &after[consumed..];
    }
    out.push_str(rest);
    out
}

/// Where the text belonging to one URL ends: the end of the line, the
/// start of the next `scheme://`, or just after this URL's userinfo `@`.
///
/// A marker whose scheme is glued on with `:` sits inside the password
/// (`user:https://secret@host`, or more than one `://`). Cutting at that
/// marker would leave the password prefix in the clear. The password runs
/// to the last `@` before a comma, quote or space that starts another URL.
fn region_end(after: &str) -> usize {
    let line = after.find(['\n', '\r']).unwrap_or(after.len());
    let Some(rel) = after[..line].find("://") else {
        return line;
    };
    let start = scheme_start(&after[..rel]);
    let prev = start.checked_sub(1).and_then(|i| after.as_bytes().get(i));
    if prev != Some(&b':') {
        return start;
    }
    let rest_at = rel + 3;
    let rest = &after[rest_at..line];
    let sep = rest.find([' ', '\t', ',', '"', '\'', '`', '<', '>']);
    let window = sep.unwrap_or(rest.len());
    match rest[..window].rfind('@') {
        Some(at) => rest_at + at + 1,
        None => sep.map_or(line, |i| rest_at + i),
    }
}

/// Byte index where the URL scheme ending `head` starts.
fn scheme_start(head: &str) -> usize {
    head.char_indices()
        .rev()
        .find(|(_, c)| !is_scheme_char(*c))
        .map_or(0, |(i, c)| i + c.len_utf8())
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// Write the redacted userinfo of `region` (the text after `://`) and
/// return how many bytes of it were consumed; `0` when there is no
/// userinfo.
fn push_userinfo(out: &mut String, region: &str) -> usize {
    let Some(stop) = region.find(|c: char| {
        let quote = matches!(c, '"' | '\'' | '`' | '<' | '>');
        quote || matches!(c, ':' | '@' | '/') || c.is_whitespace()
    }) else {
        return 0;
    };
    match region.as_bytes()[stop] {
        b'@' => {
            out.push_str(REDACTED);
            out.push('@');
            stop + 1
        }
        b':' => match region.rfind('@') {
            Some(at) if at > stop => {
                out.push_str(&region[..=stop]);
                out.push_str(REDACTED);
                out.push('@');
                at + 1
            }
            _ => 0,
        },
        _ => 0,
    }
}

fn redact_pairs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(offset) = text[search..].find('=') {
        let eq = search + offset;
        search = eq + 1;
        let key_start = text[..eq]
            .char_indices()
            .rev()
            .find(|(_, c)| !is_key_char(*c))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let key = &text[key_start..eq];
        if key.is_empty() || !is_secret_key(key) {
            continue;
        }
        let (start, end) = value_span(&text[eq + 1..]);
        out.push_str(&text[copied..=eq + start]);
        out.push_str(REDACTED);
        copied = eq + 1 + end;
        search = copied.max(search);
    }
    out.push_str(&text[copied..]);
    out
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// The `(start, end)` byte span of the secret in `value`, the text after
/// `=`. A quoted value keeps its quotes outside the span.
fn value_span(value: &str) -> (usize, usize) {
    let first = value.chars().next();
    let Some(quote) = first.filter(|c| matches!(c, '\'' | '"')) else {
        let end = value
            .find(|c: char| c.is_whitespace() || matches!(c, ';' | '&'))
            .unwrap_or(value.len());
        return (0, end);
    };
    let mut escaped = false;
    for (i, c) in value.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return (1, i);
        }
    }
    (1, value.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaks(text: &str, secret: &str) {
        let out = redact_text(text);
        assert!(!out.contains(secret), "{secret} leaked from {out}");
    }

    #[test]
    fn userinfo_ends_at_the_last_at_sign() {
        for (text, secret) in [
            ("postgres://u:p/ss@h/db", "p/ss"),
            ("postgres://u:p?ss@h/db", "p?ss"),
            ("postgres://u:p#ss@h/db", "p#ss"),
            ("postgres://u:pa ss@h/db", "pa ss"),
            ("postgres://u:pa\"ss@h/db", "pa\"ss"),
            ("postgres://u:pa'ss@h/db", "pa'ss"),
            ("postgres://u:p@ss@h/db", "p@ss"),
        ] {
            leaks(text, secret);
        }
        assert_eq!(
            redact_text("postgres://u:p/s s@h:5432/db x"),
            "postgres://u:***@h:5432/db x"
        );
        assert_eq!(redact_text("redis://tok@h"), "redis://***@h");
        assert_eq!(redact_text("redis://:pw@h"), "redis://:***@h");
    }

    #[test]
    fn a_scheme_inside_the_password_is_not_another_url() {
        for (text, secret) in [
            ("postgres://u:https://evil.com@h/db", "https://evil.com"),
            ("postgres://u:p://x@h/db", "p://x"),
            ("postgres://u:ab://cd://x@h/db", "ab://cd://x"),
            ("redis://:https://pw@h", "https://pw"),
            ("postgres://u:my://password@h/db", "my://password"),
        ] {
            leaks(text, secret);
        }
        assert_eq!(
            redact_text("postgres://u:https://evil.com@h/db"),
            "postgres://u:***@h/db"
        );
        assert_eq!(
            redact_text("postgres://u:https://evil.com@h/db,redis://:pw@h2"),
            "postgres://u:***@h/db,redis://:***@h2"
        );
        let query = "postgres://user:pass@h/db?next=https://user:secret@evil";
        let hidden = "postgres://user:***@h/db?next=https://user:***@evil";
        assert_eq!(redact_text(query), hidden);
    }

    #[test]
    fn url_lists_are_redacted_one_by_one() {
        let text = "redis://h1:6379,redis://:pw1@h2,redis://u:pw2@h3";
        assert_eq!(
            redact_text(text),
            "redis://h1:6379,redis://:***@h2,redis://u:***@h3"
        );
        assert_eq!(
            redact_text("a postgres://u:p1@h/x\nb mysql://u:p2@h/y"),
            "a postgres://u:***@h/x\nb mysql://u:***@h/y"
        );
    }

    #[test]
    fn urls_without_credentials_are_unchanged() {
        for text in [
            "no url here",
            "sqlite::memory:",
            "sqlite://app.db?mode=rwc",
            "postgres://h:5432/db",
            "https://example.com/a@b",
            "expected a == b",
        ] {
            assert_eq!(redact_text(text), text);
        }
    }

    #[test]
    fn key_value_secrets_are_redacted() {
        assert_eq!(
            redact_text("host=h password=hunter2 user=u"),
            "host=h password=*** user=u"
        );
        assert_eq!(
            redact_text("Server=h;Pwd=hunter2;Database=d"),
            "Server=h;Pwd=***;Database=d"
        );
        assert_eq!(
            redact_text("host=h password='a b\\'c' port=1"),
            "host=h password='***' port=1"
        );
        assert_eq!(
            redact_text("https://x/blob?sv=1&sig=abc%2B&se=2"),
            "https://x/blob?sv=1&sig=***&se=2"
        );
        assert_eq!(
            redact_text("postgres://h/x?sslmode=require&password=p"),
            "postgres://h/x?sslmode=require&password=***"
        );
        assert_eq!(redact_text("password=\"open"), "password=\"***");
        let pairs = "signature=s token=";
        assert_eq!(redact_text(pairs), "signature=*** token=***");
    }
}
