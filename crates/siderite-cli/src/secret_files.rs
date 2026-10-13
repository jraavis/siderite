//! Names of files that usually hold credentials. Every command that
//! reads or ships project files (`ai context --include`, `package`)
//! refuses them by this one rule.

/// Private key files OpenSSH writes by default (public halves included,
/// erring toward refusing too much).
const SSH_KEYS: [&str; 4] = ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
/// Extensions of keys, certificate bundles and key stores.
const KEY_SUFFIXES: [&str; 5] = [".pem", ".key", ".p12", ".pfx", ".keystore"];

/// Whether a file called `name` likely holds credentials: `.env` files,
/// SSH private keys, key and certificate stores, and local
/// configuration overrides (`*.local.toml`). Case is ignored.
///
/// Args:
///   name: The file name, without directories.
///
/// Returns:
///   `true` when the file must not be read or shipped.
pub(crate) fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".local.toml")
        || SSH_KEYS.iter().any(|key| lower.starts_with(key))
        || KEY_SUFFIXES.iter().any(|ext| lower.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::is_secret_name;

    #[test]
    fn secret_names() {
        for name in [
            ".env",
            ".ENV.prod",
            "server.pem",
            "tls.KEY",
            "store.p12",
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            "id_ed25519.pub",
            "app.local.toml",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in [
            "main.rs",
            "Cargo.toml",
            "environment.rs",
            "keys.rs",
            ".envrc",
            "identity.rs",
        ] {
            assert!(!is_secret_name(name), "{name}");
        }
    }
}
