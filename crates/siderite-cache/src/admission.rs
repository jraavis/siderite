//! Marker admission for route caches: one provided [`Cache`] method.
//!
//! [`Cache::admit_marker`] is the only operation a route cache needs on its
//! store path beyond `get`/`set`. Backends that can do it in one atomic step
//! (the bundled ones do) override it; every other [`Cache`] runs the generic
//! sequence here from the primitives it already has, so external adapters
//! keep compiling and keep working.

use crate::{Cache, CacheError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What [`Cache::admit_marker`] must decide.
///
/// Built by the route cache; adapters only forward it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct AdmitMarker<'a> {
    /// Key of the target's generation marker.
    pub marker: &'a str,
    /// Value to store if this call creates the marker.
    pub token: &'a [u8],
    /// Key of the invalidation epoch the caller observed.
    pub epoch_key: &'a str,
    /// The epoch value the caller observed before its origin read.
    pub epoch: &'a [u8],
    /// Prefix of the budget counter key; the window index is appended.
    pub budget_prefix: &'a str,
    /// New markers admitted per window.
    pub max_new: u64,
    /// Window length, also the marker lifetime.
    pub window: Duration,
}

impl<'a> AdmitMarker<'a> {
    /// A request with no effective budget (unlimited, one-hour window).
    #[must_use]
    pub fn new(marker: &'a str, token: &'a [u8], epoch_key: &'a str, epoch: &'a [u8]) -> Self {
        Self {
            marker,
            token,
            epoch_key,
            epoch,
            budget_prefix: "",
            max_new: u64::MAX,
            window: Duration::from_secs(3600),
        }
    }

    /// Count creations in `prefix` + window index, at most `max_new` per
    /// `window`; `window` is also the marker lifetime.
    #[must_use]
    pub fn budget(mut self, prefix: &'a str, max_new: u64, window: Duration) -> Self {
        self.budget_prefix = prefix;
        self.max_new = max_new;
        self.window = window;
        self
    }
}

/// Outcome of [`Cache::admit_marker`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Admission {
    /// The marker's value: the caller's token when it created the marker,
    /// otherwise the token already stored.
    Marker(Vec<u8>),
    /// The epoch differs from the one the caller observed, or no longer
    /// exists: a write may have intervened, so the fill must not be stored.
    Stale,
    /// The window's budget is spent. The window ends after `resets_in`.
    Exhausted {
        /// Time left in the current window.
        resets_in: Duration,
    },
}

/// Generic admission from existing primitives.
///
/// Order: create the marker, count it only if this call created it, delete
/// it again when over budget, then compare the epoch (a marker this call
/// created is deleted again when the epoch differs). A write that lands
/// after the marker exists deletes it; one that landed earlier changed the
/// epoch. The window comes from this process's clock, so clocks that
/// disagree by more than a fraction of the window let processes count
/// against different counters; the bundled backends avoid that by reading
/// their own clock. Between creating and deleting an over-budget marker the
/// marker is briefly visible, which only a non-atomic adapter can show.
pub(crate) async fn admit_generic<C>(
    cache: &C,
    req: &AdmitMarker<'_>,
) -> Result<Admission, CacheError>
where
    C: Cache + ?Sized,
{
    let created = cache
        .set_if_absent_with_ttl(req.marker, req.token.to_vec(), req.window)
        .await?;
    let token = if created {
        let (used, resets_in) = count_creation(cache, req).await?;
        if used > req.max_new {
            cache.delete(req.marker).await?;
            return Ok(Admission::Exhausted { resets_in });
        }
        req.token.to_vec()
    } else {
        let live = cache.get(req.marker).await?;
        live.ok_or_else(|| CacheError::Backend("generation was evicted".into()))?
    };
    match cache.get(req.epoch_key).await? {
        Some(current) if current == req.epoch => Ok(Admission::Marker(token)),
        _ => {
            if created {
                // The fill will not be stored, so the marker this call
                // made would only occupy a slot. Deleting it after the
                // epoch read keeps the safety order: a write between the
                // two reads deletes the marker itself.
                let _ = cache.delete(req.marker).await;
            }
            Ok(Admission::Stale)
        }
    }
}

/// Increment the window counter for a marker this call created. Returns the
/// new count and the time left in the window.
async fn count_creation<C>(cache: &C, req: &AdmitMarker<'_>) -> Result<(u64, Duration), CacheError>
where
    C: Cache + ?Sized,
{
    let (index, resets_in) = clock_window(req.window);
    let key = format!("{}{index}", req.budget_prefix);
    let ttl = req.window.checked_mul(2).ok_or(CacheError::InvalidTtl);
    let used = match ttl {
        Ok(ttl) => cache.increment_with_expiry(&key, 1, ttl).await,
        Err(error) => Err(error),
    };
    match used {
        Ok(used) => Ok((u64::try_from(used).unwrap_or(u64::MAX), resets_in)),
        Err(error) => {
            // Nothing was spent: undo the creation before failing so an
            // adapter missing the increment leaves no uncounted marker.
            let _ = cache.delete(req.marker).await;
            Err(error)
        }
    }
}

/// Window index and time left, from this process's wall clock.
fn clock_window(window: Duration) -> (u64, Duration) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    let now = now.unwrap_or_default().as_millis();
    let ms = window.as_millis().max(1);
    let index = u64::try_from(now / ms).unwrap_or(u64::MAX);
    let left = u64::try_from(ms - now % ms).unwrap_or(u64::MAX);
    (index, Duration::from_millis(left))
}
