//! Server-side scripts for the route cache: counter, admission, rotation.
//!
//! Scripts run through [`redis::Script`], which sends `EVALSHA` and falls
//! back to loading the script when the server does not know it yet, so the
//! script text crosses the wire once per server rather than per call.

use super::{MAX_TTL_MS, RedisError, RedisStore, required_ttl};
use redis::{Script, Value};
use std::sync::LazyLock;
use std::time::Duration;

/// `INCRBY` plus an expiry for a key that has none.
static INCR_EXPIRE: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        "local n = redis.call('INCRBY', KEYS[1], ARGV[1]) \
         if redis.call('PTTL', KEYS[1]) < 0 then \
         redis.call('PEXPIRE', KEYS[1], ARGV[2]) end return n",
    )
});

/// Marker admission.
///
/// KEYS: marker, epoch. ARGV: token, expected epoch, marker ttl (ms),
/// budget, window (ms), counter key prefix, counter ttl (ms).
/// Replies `{0, 0}` stale epoch, `{1, value}` admitted (existing or new
/// marker), `{2, ms}` budget spent with `ms` left in the window.
/// The window index comes from the server clock (`TIME`), the same clock
/// that expires the counter, so processes with skewed clocks agree.
static ADMIT: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        "if redis.call('GET', KEYS[2]) ~= ARGV[2] then return {0, 0} end \
         local live = redis.call('GET', KEYS[1]) \
         if live then return {1, live} end \
         local t = redis.call('TIME') \
         local now = t[1] * 1000 + math.floor(t[2] / 1000) \
         local window = tonumber(ARGV[5]) \
         local index = math.floor(now / window) \
         local counter = ARGV[6] .. string.format('%d', index) \
         local used = tonumber(redis.call('GET', counter) or '0') \
         if used >= tonumber(ARGV[4]) then \
         return {2, (index + 1) * window - now} end \
         redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[3]) \
         redis.call('INCR', counter) \
         if redis.call('PTTL', counter) < 0 then \
         redis.call('PEXPIRE', counter, ARGV[7]) end \
         return {1, ARGV[1]}",
    )
});

/// What [`RedisStore::admit_marker`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerReply {
    /// The marker's stored value: the caller's token when it created the
    /// marker, otherwise the live marker's.
    Admitted(String),
    /// The epoch differs from the expected one, or is missing.
    Stale,
    /// The window's budget is spent; the window ends after this long.
    Exhausted(Duration),
}

/// Arguments of [`RedisStore::admit_marker`]. Keys are logical (the store
/// prefix is applied); values are the strings stored as-is.
#[derive(Debug, Clone, Copy)]
pub struct MarkerAdmission<'a> {
    /// Marker key to create when absent.
    pub marker: &'a str,
    /// Value stored when this call creates the marker.
    pub token: &'a str,
    /// Epoch key read first.
    pub epoch_key: &'a str,
    /// Epoch value the caller observed before its origin read.
    pub epoch: &'a str,
    /// Counter key prefix; the script appends the server window index.
    pub budget_prefix: &'a str,
    /// New markers admitted per window.
    pub max_new: u64,
    /// Window length, also the marker lifetime.
    pub window: Duration,
}

impl RedisStore {
    /// `INCRBY` plus an expiry for a key that has none, as one atomic
    /// server-side script.
    ///
    /// A missing key starts at `0`. The key receives `ttl` only when it has
    /// no expiry afterwards (it was created by this call, or was persistent);
    /// an existing deadline is kept. The two steps cannot be separated by an
    /// expiry or eviction, so the key is never left without a TTL.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `ttl` is below 1ms or above
    /// `i64::MAX / 2` milliseconds, before any command is sent. Otherwise
    /// connection or command errors (the value is not an integer, or it
    /// would overflow).
    pub async fn incr_by_expire(
        &self,
        key: &str,
        by: i64,
        ttl: Duration,
    ) -> Result<i64, RedisError> {
        let ms = required_ttl(ttl)?;
        let mut invocation = INCR_EXPIRE.key(self.key(key));
        let call = invocation.arg(by).arg(ms);
        let mut conn = self.manager.clone();
        call.invoke_async(&mut conn).await.map_err(RedisError::from)
    }

    /// Admit a marker in one script: check the epoch, find or create the
    /// marker, and count the creation against the window's budget.
    ///
    /// The window is `floor(server TIME / window)`, so every process agrees
    /// on it whatever its own clock says; the counter expires after twice
    /// the window on the same server clock. Nothing is created when the
    /// epoch differs or the budget is spent. The counter key is built inside
    /// the script, so this requires a single (non-cluster) deployment, and
    /// Redis 5 or newer (effects replication after `TIME`).
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `window` is below 1ms or above
    /// `i64::MAX / 4` milliseconds, before any command is sent. Otherwise
    /// connection or command errors.
    pub async fn admit_marker(&self, args: MarkerAdmission<'_>) -> Result<MarkerReply, RedisError> {
        let window = required_ttl(args.window)?;
        let counter_ttl = window
            .checked_mul(2)
            .filter(|ttl| *ttl <= MAX_TTL_MS)
            .ok_or_else(|| ttl_error("window"))?;
        let mut invocation = ADMIT.key(self.key(args.marker));
        let call = invocation
            .key(self.key(args.epoch_key))
            .arg(args.token)
            .arg(args.epoch)
            .arg(window)
            .arg(args.max_new)
            .arg(window)
            .arg(self.key(args.budget_prefix))
            .arg(counter_ttl);
        let mut conn = self.manager.clone();
        let (code, value): (i64, Value) = call
            .invoke_async(&mut conn)
            .await
            .map_err(RedisError::from)?;
        match (code, value) {
            (0, _) => Ok(MarkerReply::Stale),
            (1, Value::BulkString(bytes)) => String::from_utf8(bytes)
                .map(MarkerReply::Admitted)
                .map_err(|_| RedisError::Command("marker is not UTF-8".into())),
            (2, Value::Int(ms)) => {
                let ms = u64::try_from(ms).unwrap_or(0);
                Ok(MarkerReply::Exhausted(Duration::from_millis(ms)))
            }
            _ => Err(RedisError::Command(
                "unexpected reply from the admission script".into(),
            )),
        }
    }

    /// Set `epoch_key` to `epoch` with a lifetime and delete `marker`, as
    /// one `MULTI`/`EXEC` round trip.
    ///
    /// # Errors
    /// [`RedisError::Invalid`] when `ttl` is below 1ms or above
    /// `i64::MAX / 2` milliseconds. Otherwise connection or command errors.
    pub async fn rotate_marker(
        &self,
        marker: &str,
        epoch_key: &str,
        epoch: &str,
        ttl: Duration,
    ) -> Result<(), RedisError> {
        let ms = required_ttl(ttl)?;
        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("SET")
            .arg(self.key(epoch_key))
            .arg(epoch)
            .arg("PX")
            .arg(ms)
            .ignore()
            .cmd("DEL")
            .arg(self.key(marker))
            .ignore();
        let mut conn = self.manager.clone();
        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(RedisError::from)
    }
}

fn ttl_error(what: &str) -> RedisError {
    RedisError::Invalid(format!("redis {what} must be at most i64::MAX / 4 ms"))
}
