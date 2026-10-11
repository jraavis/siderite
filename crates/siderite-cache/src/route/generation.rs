//! Random generations keep stale fills unreachable across target writes.
//!
//! Invariants (full contract in `docs/CACHE.md`):
//!
//! Enforced today:
//!
//! 1. A generation is an unguessable random token. Whenever a marker is
//!    created after expiry, eviction, deletion or rotation, the token is
//!    freshly random; it is never derived from the previous one or from a
//!    counter, so an older token cannot be reproduced.
//! 2. A representation key embeds its generation, so a delayed fill that
//!    finishes after rotation lands under a key no reader will compute.
//!
//! Target state (roadmap R04; not yet enforced by this module, which still
//! creates non-expiring markers through `set_if_absent`):
//!
//! 3. A representation's TTL is clamped to `min(response ttl,
//!    generation_ttl)`. A representation may outlive its marker by at most
//!    `generation_ttl`, but it is unreachable: a recreated marker carries a
//!    fresh token (invariant 1), so no reader computes the old key.
//! 4. Marker creation goes through atomic expiring creation
//!    (`set_if_absent_with_ttl`); a backend without it makes the layer
//!    bypass storage rather than emulate it.
//! 5. Invalidation deletes the marker instead of writing a new one. A missing
//!    marker yields a fresh random token (invariant 1), so deletion rotates
//!    the generation and creates no metadata outside the admission budget.

use super::{Policy, cache_key, push_part};
use crate::{Cache, CacheError};
use http::Method;
use siderite_core::Request;
use uuid::Uuid;

type Stamp = Result<Uuid, CacheError>;
type Outcome = Result<(), CacheError>;

pub(super) fn target(req: &Request, policy: &Policy, scheme: &str) -> String {
    let mut target = String::from("route-v2-target:");
    push_part(&mut target, policy.namespace.as_bytes());
    push_part(&mut target, scheme.as_bytes());
    let host = req
        .headers()
        .get(http::header::HOST)
        .map(http::HeaderValue::as_bytes)
        .or_else(|| req.uri().authority().map(|a| a.as_str().as_bytes()))
        .unwrap_or_default();
    push_part(&mut target, &host.to_ascii_lowercase());
    let path = req.uri().path_and_query().map_or("/", |p| p.as_str());
    push_part(&mut target, path.as_bytes());
    target
}

pub(super) fn representation_key(
    req: &Request,
    policy: &Policy,
    scheme: &str,
    generation: Uuid,
) -> String {
    let mut key = String::from("route-v2-response:");
    push_part(&mut key, policy.namespace.as_bytes());
    push_part(&mut key, generation.as_bytes());
    push_part(&mut key, cache_key(req, scheme).as_bytes());
    key
}

pub(super) async fn current<C: Cache>(cache: &C, key: &str) -> Stamp {
    if let Some(value) = cache.get(key).await? {
        return decode(&value);
    }
    let candidate = Uuid::new_v4();
    if cache
        .set_if_absent(key, candidate.as_bytes().to_vec())
        .await?
    {
        return Ok(candidate);
    }
    match cache.get(key).await? {
        Some(value) => decode(&value),
        None => Err(CacheError::Backend("generation was evicted".into())),
    }
}

fn decode(value: &[u8]) -> Stamp {
    let reason = "invalid cache generation";
    Uuid::from_slice(value).map_err(|_| CacheError::Backend(reason.into()))
}

pub(super) async fn invalidate<C: Cache>(cache: &C, key: &str) -> Outcome {
    cache
        .set(key, Uuid::new_v4().as_bytes().to_vec(), None)
        .await
}

pub(super) fn unsafe_method(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryCache, RouteCache};
    use siderite_core::Body;
    use std::sync::Arc;
    use std::time::Duration;

    type TestResult = Result<(), CacheError>;
    const MARKER: &str = "route-v2-target:test";

    #[tokio::test]
    async fn expired_marker_yields_a_fresh_token() -> TestResult {
        let cache = MemoryCache::new(8);
        let ttl = Some(Duration::from_millis(5));
        let first = Uuid::new_v4();
        cache.set(MARKER, first.as_bytes().to_vec(), ttl).await?;
        assert_eq!(current(&cache, MARKER).await?, first);
        tokio::time::sleep(Duration::from_millis(30)).await;
        let second = current(&cache, MARKER).await?;
        assert_ne!(second, first);
        assert_eq!(current(&cache, MARKER).await?, second);
        Ok(())
    }

    #[tokio::test]
    async fn evicted_marker_yields_a_fresh_token() -> TestResult {
        let cache = MemoryCache::new(1);
        let first = current(&cache, MARKER).await?;
        cache.set("other", vec![1], None).await?;
        let second = current(&cache, MARKER).await?;
        assert_ne!(second, first);
        cache.delete(MARKER).await?;
        let third = current(&cache, MARKER).await?;
        assert_ne!(third, second);
        assert_ne!(third, first);
        Ok(())
    }

    #[tokio::test]
    async fn invalidation_rotates_to_a_fresh_token() -> TestResult {
        let cache = MemoryCache::new(8);
        let first = current(&cache, MARKER).await?;
        invalidate(&cache, MARKER).await?;
        let second = current(&cache, MARKER).await?;
        assert_ne!(first, second);
        Ok(())
    }

    fn request() -> Result<Request, http::Error> {
        let uri = "/item";
        http::Request::builder().uri(uri).body(Body::empty())
    }

    #[tokio::test]
    async fn delayed_fill_cannot_revive_old_data() -> TestResult {
        let cache = Arc::new(MemoryCache::new(16));
        let policy = RouteCache::new(Arc::clone(&cache)).policy();
        let req = request().map_err(|e| CacheError::Backend(e.to_string()))?;
        let target = target(&req, &policy, "http");
        for rotate in [Rotation::Invalidate, Rotation::Evict] {
            let old = current(cache.as_ref(), &target).await?;
            let old_key = representation_key(&req, &policy, "http", old);
            // The rotation happens while the origin read is still pending.
            match rotate {
                Rotation::Invalidate => {
                    invalidate(cache.as_ref(), &target).await?;
                }
                Rotation::Evict => {
                    cache.delete(&target).await?;
                }
            }
            // The delayed fill now lands under the old generation key.
            cache.set(&old_key, b"old".to_vec(), None).await?;
            let new = current(cache.as_ref(), &target).await?;
            assert_ne!(new, old, "rotation must not reuse the old token");
            let new_key = representation_key(&req, &policy, "http", new);
            assert_ne!(new_key, old_key);
            assert_eq!(cache.get(&new_key).await?, None);
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum Rotation {
        Invalidate,
        Evict,
    }
}
