//! Session ids: 256 random bits, and the SHA-256 hash stores see.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Random bytes in an id (256 bits).
const ID_BYTES: usize = 32;
/// Length of the base64url text of an id.
const ID_LEN: usize = 43;

/// A new id from the operating system's CSPRNG, as base64url text.
pub(crate) fn generate() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; ID_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Whether `id` has the shape of a generated id. Anything else is not
/// worth a store lookup.
pub(crate) fn well_formed(id: &str) -> bool {
    id.len() == ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The SHA-256 of a session id as lowercase hex: the only form of an id
/// a [`SessionStore`](crate::SessionStore) ever sees.
#[must_use]
pub fn id_hash(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}
