//! Random generations keep stale fills unreachable across target writes.
//!
//! Invariants (full contract in `docs/CACHE.md`):
//!
//! Enforced today:
//!
//! 1. A generation is a unique token: a random per-layer 64-bit seed plus a
//!    counter (see [`Local`]). Whenever a marker is created after expiry,
//!    eviction, deletion or rotation, its token differs from every earlier
//!    one, so an older token cannot be reproduced. Tokens are unique, not
//!    secret.
//! 2. A representation key embeds its generation, so a delayed fill that
//!    finishes after rotation lands under a key no reader will compute.
//!
//! 3. A representation's TTL is clamped to `min(response ttl,
//!    generation_ttl)`. A representation may outlive its marker by at most
//!    `generation_ttl`, but it is unreachable: a recreated marker carries a
//!    fresh token (invariant 1), so no reader computes the old key.
//! 4. Marker creation goes through atomic expiring creation
//!    (`Cache::admit_marker`, built on `set_if_absent_with_ttl` by default);
//!    a backend without it makes the layer bypass storage rather than
//!    emulate it. The first `Unsupported` is
//!    remembered (with one warning, never reset) so later requests skip
//!    the backend entirely.
//! 5. Markers are finite: they carry `generation_ttl` (default one hour).
//! 6. Invalidation deletes the marker instead of writing a new one. A missing
//!    marker yields a fresh random token (invariant 1), so deletion rotates
//!    the generation and creates no metadata outside the admission budget.

use super::{ADMIT_WARNED, NO_MARKERS, Policy, cache_key, push_part};
use crate::{Admission, AdmitMarker, Cache, CacheError};
use http::Method;
use siderite_core::Request;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use uuid::Uuid;

type CacheResult<T> = Result<T, CacheError>;
type Stamp = CacheResult<Uuid>;
type Outcome = Result<(), CacheError>;

/// Epoch keys per namespace. Targets hash onto one of these, so a write
/// refuses only first fills of targets that share its stripe.
pub(super) const STRIPES: usize = 64;

/// Key text fixed per namespace, built once per layer.
#[derive(Debug)]
pub(super) struct Keys {
    target_head: String,
    response_head: String,
    budget: String,
    epochs: Box<[String]>,
}

impl Keys {
    pub(super) fn new(namespace: &str) -> Self {
        let part = |role: &str| {
            let mut head = String::from(role);
            push_part(&mut head, namespace.as_bytes());
            head
        };
        let epoch = part("route-v2-epoch:");
        Self {
            target_head: part("route-v2-target:"),
            response_head: part("route-v2-response:"),
            budget: part("route-v2-budget:"),
            epochs: (0..STRIPES).map(|n| format!("{epoch}{n}")).collect(),
        }
    }
}

/// Per-layer state that never touches the backend: identifier source and the
/// local note that a window's budget is spent.
///
/// Identifiers are a random 64-bit seed drawn once per layer plus a counter.
/// They only have to differ from every identifier another request might still
/// hold (a token, or an epoch value a stale fill observed). Within a layer the
/// counter guarantees that; across layers and processes two identifiers
/// collide only if both draw the same seed (2^-64 per pair) and the same
/// counter. They are unique, not secret: nothing is learned from one.
///
/// The seed is drawn when the layer is built. Processes forked after the
/// build (prefork servers) copy the seed and the counter and would mint
/// identical identifiers: build the layer in each process after the fork.
#[derive(Debug)]
pub(super) struct Local {
    seed: [u8; 8],
    counter: AtomicU64,
    base: Instant,
    /// Milliseconds after `base` until which the budget is known spent.
    spent_until: AtomicU64,
}

impl Local {
    pub(super) fn new() -> Self {
        // Bytes 0..6 of a v4 UUID are fully random (the version and
        // variant bits sit in bytes 6 and 8); take four from each of two.
        let [a, b, c, d, ..] = *Uuid::new_v4().as_bytes();
        let [e, f, g, h, ..] = *Uuid::new_v4().as_bytes();
        Self {
            seed: [a, b, c, d, e, f, g, h],
            counter: AtomicU64::new(0),
            base: Instant::now(),
            spent_until: AtomicU64::new(0),
        }
    }

    /// A fresh 16-byte identifier: seed followed by a counter.
    pub(super) fn next_id(&self) -> [u8; 16] {
        let n = self.counter.fetch_add(1, Ordering::Relaxed).to_be_bytes();
        let [a, b, c, d, e, f, g, h] = self.seed;
        let [i, j, k, l, m, o, p, q] = n;
        [a, b, c, d, e, f, g, h, i, j, k, l, m, o, p, q]
    }

    /// True while a recent admission reported the window's budget spent.
    pub(super) fn spent(&self) -> bool {
        let until = self.spent_until.load(Ordering::Relaxed);
        until != 0 && millis(self.base.elapsed()) < until
    }

    fn spend(&self, resets_in: Duration) {
        let until = millis(self.base.elapsed()).saturating_add(millis(resets_in));
        self.spent_until.fetch_max(until.max(1), Ordering::Relaxed);
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// FNV-1a: stable across builds and processes, unlike `DefaultHasher`.
fn stripe(target: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in target.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    usize::try_from(hash % STRIPES as u64).unwrap_or(0)
}

pub(super) fn target(req: &Request, policy: &Policy, scheme: &str) -> String {
    let head = &policy.keys.target_head;
    let mut target = String::with_capacity(head.len() + 96);
    target.push_str(head);
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
    let head = &policy.keys.response_head;
    let mut key = String::with_capacity(head.len() + suffix.len() + 40);
    key.push_str(head);
    push_part(&mut key, generation.as_bytes());
    push_part(&mut key, suffix.as_bytes());
    key
}

/// What a request knew about its target before the origin ran.
pub(super) struct Started {
    /// The invalidation epoch observed before the origin read.
    epoch: Vec<u8>,
    stripe: usize,
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
/// it. The epoch always exists afterwards: a missing one (first use,
/// expiry, eviction) is created here with a fresh random value, about once
/// per `generation_ttl` per stripe, so a later disappearance of the epoch
/// is itself a change that [`admit`] refuses. The usual cost is one `get`.
///
/// # Errors
/// Backend failures; `Unsupported` also sets the shared `NO_MARKERS` flag.
pub(super) async fn begin<C: Cache>(
    cache: &C,
    policy: &Policy,
    target: &str,
) -> CacheResult<Started> {
    let stripe = stripe(target);
    let key = &policy.keys.epochs[stripe];
    if let Some(epoch) = cache.get(key).await? {
        return Ok(Started { epoch, stripe });
    }
    let fresh = policy.local.next_id().to_vec();
    let ttl = policy.generation_ttl;
    let made = cache.set_if_absent_with_ttl(key, fresh.clone(), ttl).await;
    if made.inspect_err(|error| note_error(error, &policy.flags))? {
        return Ok(Started {
            epoch: fresh,
            stripe,
        });
    }
    match cache.get(key).await? {
        Some(epoch) => Ok(Started { epoch, stripe }),
        None => Err(CacheError::Backend("epoch was evicted".into())),
    }
}

/// Admit a storable response: find or create the marker and return its
/// token, or `None` when the response must bypass (budget exhausted or a
/// write intervened).
///
/// One backend call ([`Cache::admit_marker`]) checks the epoch, finds or
/// creates the marker and counts a creation against the namespace's
/// window budget. A write that lands after the marker exists deletes it;
/// one that landed earlier replaced the epoch (or evicted it), which the
/// call reports as stale. A fill that began before a write therefore can
/// never become reachable.
///
/// # Errors
/// Backend failures. `Unsupported` also sets the shared `NO_MARKERS` flag
/// (one warning), so the layer stops storing.
pub(super) async fn admit<C: Cache>(
    cache: &C,
    target: &str,
    policy: &Policy,
    started: &Started,
) -> CacheResult<Option<Uuid>> {
    let candidate = policy.local.next_id();
    let request = AdmitMarker {
        marker: target,
        token: &candidate,
        epoch_key: &policy.keys.epochs[started.stripe],
        epoch: &started.epoch,
        budget_prefix: &policy.keys.budget,
        max_new: policy.max_new_targets,
        window: policy.generation_ttl,
    };
    let admission = cache.admit_marker(&request).await;
    match admission.inspect_err(|error| {
        note_error(error, &policy.flags);
        note_admit_failure(error, &policy.flags);
    })? {
        Admission::Marker(token) => decode(&token).map(Some),
        Admission::Exhausted { resets_in } => {
            policy.local.spend(resets_in);
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn note_error(error: &CacheError, flags: &AtomicU8) {
    if matches!(error, CacheError::Unsupported { .. }) {
        note_unsupported(flags);
    }
}

/// Warn once per layer when admission fails with a backend error, so a
/// persistent script or command error (for example a server that rejects
/// the admission script) is visible instead of an endless silent miss.
fn note_admit_failure(error: &CacheError, flags: &AtomicU8) {
    if matches!(error, CacheError::Backend(_)) {
        let before = flags.fetch_or(ADMIT_WARNED, Ordering::AcqRel);
        if before & ADMIT_WARNED == 0 {
            tracing::warn!(%error, "route cache marker admission failed; \
                 responses are served uncached while it keeps failing");
        }
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
/// The stripe's epoch is replaced in the same backend call (bounded keys,
/// refreshed with the marker lifetime) so fills that began without a marker
/// are refused.
pub(super) async fn invalidate<C: Cache>(cache: &C, key: &str, policy: &Policy) -> Outcome {
    if policy.generation_ttl.is_zero() {
        return cache.delete(key).await.map(drop);
    }
    let epoch = policy.local.next_id();
    let epoch_key = &policy.keys.epochs[stripe(key)];
    let ttl = policy.generation_ttl;
    cache.invalidate_marker(key, epoch_key, &epoch, ttl).await
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
        let started = begin(cache, p, key).await?;
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

    #[tokio::test]
    async fn evicted_epoch_refuses_a_stale_fill() -> TestResult {
        // The write's epoch is pushed out of a tiny cache before the
        // delayed fill is admitted: a missing epoch must read as changed.
        let cache = MemoryCache::new(8);
        let p = policy(TTL);
        let started = begin(&cache, &p, MARKER).await?;
        invalidate(&cache, MARKER, &p).await?;
        for n in 0..16 {
            cache.set(&format!("unrelated-{n}"), vec![1], None).await?;
        }
        assert_eq!(cache.get(&p.keys.epochs[stripe(MARKER)]).await?, None);
        assert_eq!(admit(&cache, MARKER, &p, &started).await?, None);
        // The next request recreates the epoch and is admitted.
        assert!(current(&cache, MARKER, &p).await.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn begin_creates_a_missing_epoch_once() -> TestResult {
        let cache = MemoryCache::new(8);
        let p = policy(TTL);
        let first = begin(&cache, &p, MARKER).await?;
        let second = begin(&cache, &p, MARKER).await?;
        assert_eq!(first.epoch, second.epoch);
        let key = &p.keys.epochs[stripe(MARKER)];
        assert_eq!(cache.get(key).await?, Some(first.epoch));
        Ok(())
    }

    #[test]
    fn stripes_are_stable_and_bounded() {
        assert_eq!(stripe("route-v2-target:a"), stripe("route-v2-target:a"));
        let seen: std::collections::HashSet<usize> = (0..2000)
            .map(|n| stripe(&format!("route-v2-target:{n}")))
            .collect();
        assert!(seen.iter().all(|s| *s < STRIPES));
        assert!(seen.len() > STRIPES / 2, "spread over stripes");
        // Fixed vector: stripes must not change between builds.
        assert_eq!(stripe(""), 0xcbf2_9ce4_8422_2325 % 64);
    }

    #[test]
    fn identifiers_are_unique_within_and_across_layers() {
        let (a, b) = (Local::new(), Local::new());
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(a.next_id()));
            assert!(seen.insert(b.next_id()));
        }
    }

    #[test]
    fn spent_note_expires_with_its_window() {
        let local = Local::new();
        assert!(!local.spent());
        local.spend(Duration::from_millis(30));
        assert!(local.spent());
        std::thread::sleep(Duration::from_millis(60));
        assert!(!local.spent());
    }

    #[derive(Clone, Copy)]
    enum Rotation {
        Invalidate,
        Evict,
    }
}
