//! CSRF tokens: HMAC-SHA256 of the session id hash under a server key.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn mac(key: &[u8], id_hash: &str) -> Option<HmacSha256> {
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    mac.update(id_hash.as_bytes());
    Some(mac)
}

/// The token for the session whose id hash is `id_hash`, base64url.
pub(crate) fn issue(key: &[u8], id_hash: &str) -> Option<String> {
    let tag = mac(key, id_hash)?.finalize().into_bytes();
    Some(URL_SAFE_NO_PAD.encode(tag))
}

/// Check `token` for `id_hash` in constant time.
pub(crate) fn verify(key: &[u8], id_hash: &str, token: &str) -> bool {
    let Ok(tag) = URL_SAFE_NO_PAD.decode(token) else {
        return false;
    };
    mac(key, id_hash).is_some_and(|mac| mac.verify_slice(&tag).is_ok())
}
