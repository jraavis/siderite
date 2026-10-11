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
//! 3. A representation's TTL is clamped to `min(response ttl,
//!    generation_ttl)`. A representation may outlive its marker by at most
//!    `generation_ttl`, but it is unreachable: a recreated marker carries a
//!    fresh token (invariant 1), so no reader computes the old key.
//! 4. Marker creation goes through atomic expiring creation
//!    (`set_if_absent_with_ttl`); a backend without it makes the layer
//!    bypass storage rather than emulate it. The first `Unsupported` is
//!    remembered (with one warning, never reset) so later requests skip
//!    the backend entirely.
//! 5. Markers are finite: they carry `generation_ttl` (default one hour).
//! 6. Invalidation deletes the marker instead of writing a new one. A missing
//!    marker yields a fresh random token (invariant 1), so deletion rotates
//!    the generation and creates no metadata outside the admission budget.

use super::{NO_MARKERS, Policy, cache_key, push_part};
use crate::{Cache, CacheError};
use http::Method;
use siderite_core::Request;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

type CacheResult<T> = Result<T, CacheError>;
type Stamp = CacheResult<Uuid>;
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
    key_for(policy, generation, &cache_key(req, scheme))
}

/// Representation key from a precomputed [`cache_key`] suffix.
pub(super) fn key_for(policy: &Policy, generation: Uuid, suffix: &str) -> String {
    let mut key = String::from("route-v2-response:");
    push_part(&mut key, policy.namespace.as_bytes());
    push_part(&mut key, generation.as_bytes());
    push_part(&mut key, suffix.as_bytes());
    key
}

/// What a request knew about its target before the origin ran.
pub(super) struct Started {
    /// Invalidation epoch observed while the marker was absent.
    epoch: Option<Vec<u8>>,
    at: Instant,
}

/// Read the marker without creating it: the hit path is one `get`.
///
/// `Ok(None)` means no marker exists, so no representation of the target
/// is reachable. Markers are created later, only when a response is
/// actually stored (see [`admit`]).
pub(super) async fn peek<C>(cache: &C, key: &str) -> CacheResult<Option<Uuid>>
where
    C: Cache,
{
    match cache.get(key).await? {
        Some(value) => decode(&value).map(Some),
        None => Ok(None),
    }
}

/// Snapshot the invalidation epoch for a request that found no marker.
///
/// A write that completes while such a request is still in the origin
/// finds no marker to delete, so the epoch is how the late fill notices
/// it. This costs one `get` and happens only when the marker is absent.
pub(super) async fn begin<C: Cache>(cache: &C, policy: &Policy) -> CacheResult<Started> {
    let epoch = cache.get(&epoch_key(policy)).await?;
    Ok(Started {
        epoch,
        at: Instant::now(),
    })
}

fn epoch_key(policy: &Policy) -> String {
    let mut key = String::from("route-v2-epoch:");
    push_part(&mut key, policy.namespace.as_bytes());
    key
}

/// Window index `floor(now / ttl)`; the budget counter key embeds it.
pub(super) fn window(now: Duration, ttl: Duration) -> u64 {
    let ms = ttl.as_millis().max(1);
    u64::try_from(now.as_millis() / ms).unwrap_or(u64::MAX)
}

fn budget_key(policy: &Policy) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    let window = window(now.unwrap_or_default(), policy.generation_ttl);
    let mut key = String::from("route-v2-budget:");
    push_part(&mut key, policy.namespace.as_bytes());
    key.push_str(&window.to_string());
    key
}

/// Admit a storable response: find or create the marker and return its
/// token, or `None` when the response must bypass (budget exhausted, the
/// request outlived a generation, or a write intervened).
///
/// A new marker is counted against the namespace's per-window budget with
/// one atomic increment-with-expiry. The invalidation epoch is read only
/// after the marker exists: a write that lands later deletes that marker,
/// and one that landed earlier changed the epoch, so a fill that began
/// before a write can never become reachable.
///
/// # Errors
/// Backend failures. `Unsupported` from the increment also sets the shared
/// `NO_MARKERS` flag (one warning), so the layer stops storing.
pub(super) async fn admit<C: Cache>(
    cache: &C,
    target: &str,
    policy: &Policy,
    started: &Started,
) -> CacheResult<Option<Uuid>> {
    if started.at.elapsed() >= policy.generation_ttl {
        return Ok(None);
    }
    let token = match peek(cache, target).await? {
        Some(existing) => existing,
        None => match create(cache, target, policy).await? {
            Some(created) => created,
            None => return Ok(None),
        },
    };
    let epoch = cache.get(&epoch_key(policy)).await?;
    Ok((epoch == started.epoch).then_some(token))
}

async fn create<C: Cache>(cache: &C, target: &str, policy: &Policy) -> CacheResult<Option<Uuid>> {
    let (ttl, budget) = (policy.generation_ttl, policy.max_new_targets);
    let used = cache
        .increment_with_expiry(&budget_key(policy), 1, ttl)
        .await;
    let used = used.inspect_err(|error| note_error(error, &policy.flags))?;
    if u64::try_from(used).map_or(true, |used| used > budget) {
        return Ok(None);
    }
    let candidate = Uuid::new_v4();
    let seed = candidate.as_bytes().to_vec();
    let created = cache.set_if_absent_with_ttl(target, seed, ttl).await;
    match created.inspect_err(|error| note_error(error, &policy.flags))? {
        true => Ok(Some(candidate)),
        false => match cache.get(target).await? {
            Some(value) => decode(&value).map(Some),
            None => Err(CacheError::Backend("generation was evicted".into())),
        },
    }
}

fn note_error(error: &CacheError, flags: &AtomicU8) {
    if matches!(error, CacheError::Unsupported { .. }) {
        note_unsupported(flags);
    }
}

/// Set `NO_MARKERS`, warning once, on the transition only.
fn note_unsupported(flags: &AtomicU8) {
    let before = flags.fetch_or(NO_MARKERS, Ordering::AcqRel);
    if before & NO_MARKERS == 0 {
        tracing::warn!(
            "cache backend lacks atomic expiring insertion or \
             increment-with-expiry; route caching disabled for this layer"
        );
    }
}

fn decode(value: &[u8]) -> Stamp {
    let reason = "invalid cache generation";
    Uuid::from_slice(value).map_err(|_| CacheError::Backend(reason.into()))
}

/// Rotate by deleting the marker; a missing marker draws a fresh token.
///
/// The namespace epoch is replaced first (one bounded key, refreshed with
/// the marker lifetime) so fills that began without a marker are refused.
pub(super) async fn invalidate<C: Cache>(cache: &C, key: &str, policy: &Policy) -> Outcome {
    if !policy.generation_ttl.is_zero() {
        let token = Uuid::new_v4().as_bytes().to_vec();
        let ttl = Some(policy.generation_ttl);
        cache.set(&epoch_key(policy), token, ttl).await?;
    }
    cache.delete(key).await.map(drop)
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
    const TTL: Duration = Duration::from_secs(60);

    fn policy(ttl: Duration) -> Policy {
        RouteCache::new(MemoryCache::new(1))
            .generation_ttl(ttl)
            .policy()
    }

    /// Find or create the marker like a storing request would.
    async fn current<C: Cache>(cache: &C, key: &str, p: &Policy) -> Stamp {
        let started = begin(cache, p).await?;
        let admitted = admit(cache, key, p, &started).await?;
        admitted.ok_or_else(|| CacheError::Backend("bypassed".into()))
    }

    #[tokio::test]
    async fn expired_marker_yields_a_fresh_token() -> TestResult {
        let cache = MemoryCache::new(8);
        let ttl = Some(Duration::from_millis(5));
        let first = Uuid::new_v4();
        cache.set(MARKER, first.as_bytes().to_vec(), ttl).await?;
        assert_eq!(current(&cache, MARKER, &policy(TTL)).await?, first);
        tokio::time::sleep(Duration::from_millis(30)).await;
        let second = current(&cache, MARKER, &policy(TTL)).await?;
        assert_ne!(second, first);
        assert_eq!(current(&cache, MARKER, &policy(TTL)).await?, second);
        Ok(())
    }

    #[tokio::test]
    async fn evicted_marker_yields_a_fresh_token() -> TestResult {
        let cache = MemoryCache::new(1);
        let first = current(&cache, MARKER, &policy(TTL)).await?;
        cache.set("other", vec![1], None).await?;
        let second = current(&cache, MARKER, &policy(TTL)).await?;
        assert_ne!(second, first);
        cache.delete(MARKER).await?;
        let third = current(&cache, MARKER, &policy(TTL)).await?;
        assert_ne!(third, second);
        assert_ne!(third, first);
        Ok(())
    }

    #[tokio::test]
    async fn invalidation_rotates_to_a_fresh_token() -> TestResult {
        let cache = MemoryCache::new(8);
        let first = current(&cache, MARKER, &policy(TTL)).await?;
        invalidate(&cache, MARKER, &policy(TTL)).await?;
        let second = current(&cache, MARKER, &policy(TTL)).await?;
        assert_ne!(first, second);
        Ok(())
    }

    #[tokio::test]
    async fn markers_expire_and_invalidation_deletes() -> TestResult {
        let cache = MemoryCache::new(8);
        let short = Duration::from_millis(20);
        let first = current(&cache, MARKER, &policy(short)).await?;
        assert!(cache.get(MARKER).await?.is_some());
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(cache.get(MARKER).await?, None, "marker is finite");
        let second = current(&cache, MARKER, &policy(TTL)).await?;
        assert_ne!(first, second);
        invalidate(&cache, MARKER, &policy(TTL)).await?;
        assert_eq!(cache.get(MARKER).await?, None, "no marker is written");
        invalidate(&cache, MARKER, &policy(TTL)).await?;
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
            let old = current(cache.as_ref(), &target, &policy).await?;
            let old_key = representation_key(&req, &policy, "http", old);
            // The rotation happens while the origin read is still pending.
            match rotate {
                Rotation::Invalidate => {
                    invalidate(cache.as_ref(), &target, &policy).await?;
                }
                Rotation::Evict => {
                    cache.delete(&target).await?;
                }
            }
            // The delayed fill now lands under the old generation key.
            cache.set(&old_key, b"old".to_vec(), None).await?;
            let new = current(cache.as_ref(), &target, &policy).await?;
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
