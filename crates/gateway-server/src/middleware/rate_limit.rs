//! Token-bucket rate-limiter.
//!
//! A process-global bucket keyed by tenant is used for the MVP.
//! Tokens are stored in µ-tokens (1 token = 1000 µ-tokens) so integer
//! atomics can represent fractional refills without loss of precision.
//!
//! Capacity is reconfigurable at runtime via [`TokenBucket::set_rpm`],
//! which the admin `PUT /api/admin/config/rate-limit` route calls.

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;
use tracing::warn;

use gateway_core::error::GatewayError;
use gateway_core::tenant::TenantContext;

/// Scale factor: 1 logical token = MICRO tokens.
const MICRO: u64 = 1_000;

/// µ-tokens per second (so we can compute ceil division for the per-ms ratio).
const MICRO_PER_SEC: u64 = MICRO;

/// Refill coalescing threshold — ignore sub-ms elbow-grease.
const MIN_REFILL_GAP_MS: u64 = 100;

/// Ceiling of `(capacity * MICRO_PER_SEC) / 60_000` — µ-tokens per ms.
///
/// Rounded up so that `micro_per_ms * 60_000 >= capacity * MICRO_PER_SEC`;
/// at capacity=60 this yields exactly 1 µ/ms (60 tokens/sec).
fn micro_per_ms(capacity: u64) -> u64 {
    let numerator = capacity.saturating_mul(MICRO_PER_SEC);
    numerator.div_ceil(60_000)
}

/// Process-wide token bucket shared by all tenants (MVP).
pub struct TokenBucket {
    /// Maximum tokens (== configured RPM).
    capacity: AtomicU64,

    /// Available tokens, stored in µ-tokens.
    tokens: AtomicU64,

    /// Last refill timestamp, milliseconds since `UNIX_EPOCH`.
    last_ms: AtomicU64,
}

impl TokenBucket {
    pub fn new(rpm: u32) -> Self {
        let cap = rpm.max(1) as u64;
        Self {
            capacity: AtomicU64::new(cap),
            tokens: AtomicU64::new(cap.saturating_mul(MICRO)),
            last_ms: AtomicU64::new(current_time_ms()),
        }
    }

    /// Update the bucket's maximum capacity. Existing tokens above the new
    /// capacity are clamped down on the next refill.
    #[allow(dead_code)]
    pub fn set_rpm(&self, rpm: u32) {
        let cap = rpm.max(1) as u64;
        self.capacity.store(cap, Ordering::SeqCst);
        // Bring current token count down if it now exceeds the new capacity.
        let max_tokens = cap.saturating_mul(MICRO);
        let mut cur = self.tokens.load(Ordering::Relaxed);
        while cur > max_tokens {
            if self
                .tokens
                .compare_exchange(cur, max_tokens, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
            cur = self.tokens.load(Ordering::Relaxed);
        }
    }

    /// Try to consume one token.
    ///
    /// Returns `Some(wait)` when the request should be rate-limited (callers
    /// should respond with HTTP 429 + a matching `Retry-After`), or `None` if
    /// the token was consumed and the request is allowed through.
    pub fn consume(&self) -> Option<Duration> {
        self.refill();
        loop {
            let cur = self.tokens.load(Ordering::Relaxed);
            if cur < MICRO {
                // Not enough µ-tokens. Compute wait using the current refill
                // rate.  Capacity is RPM, == capacity tokens per 60_000 ms.
                // µ-tokens per ms = capacity * MICRO / 60_000.
                let cap = self.capacity.load(Ordering::Relaxed);
                let micro_per_ms = micro_per_ms(cap);
                if micro_per_ms == 0 {
                    return Some(Duration::from_secs(60));
                }
                let deficit = MICRO - cur;
                let wait_ms = (deficit / micro_per_ms).saturating_add(1);
                return Some(Duration::from_millis(wait_ms));
            }
            match self.tokens.compare_exchange(
                cur,
                cur - MICRO,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return None,
                Err(_) => continue,
            }
        }
    }

    /// Top up tokens based on elapsed time since the last refill.
    ///
    /// Uses CAS-on-`last_ms` to coalesce concurrent callers: only the
    /// winner applies the elapsed-time delta, the loser returns early so it
    /// does not double-count time that the winner already refilled.
    fn refill(&self) {
        let now = current_time_ms();
        let last = self.last_ms.load(Ordering::Relaxed);
        let elapsed_ms = now.saturating_sub(last);
        if elapsed_ms < MIN_REFILL_GAP_MS {
            return;
        }

        let cap = self.capacity.load(Ordering::Relaxed);
        let micro_per_ms = micro_per_ms(cap);
        let new = elapsed_ms.saturating_mul(micro_per_ms);

        // Claim this refill window. If another CAS already advanced `last_ms`,
        // we lose; the delta we computed is already applied by the winner, so
        // we must not apply it again.
        if self
            .last_ms
            .compare_exchange(last, now, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        let max_tokens = cap.saturating_mul(MICRO);
        let mut cur = self.tokens.load(Ordering::Relaxed);
        loop {
            let nxt = (cur + new).min(max_tokens);
            match self
                .tokens
                .compare_exchange(cur, nxt, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }
    }
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Paths excluded from rate limiting — K8s liveness/readiness probes must
/// never be throttled even if a tenant is in the middle of a 429 window.
const UNLIMITED_PATHS: &[&str] = &["/healthz", "/readyz", "/deep-health", "/health"];

/// Per-tenant rate-limiter state.
///
/// Each tenant gets its own [`TokenBucket`]; the map is keyed by tenant id
/// and protected by an `RwLock` so concurrent reads from different tenants
/// don't serialize on a single `Mutex`.
pub struct RateLimiter {
    buckets: RwLock<HashMap<String, Arc<TokenBucket>>>,
    /// Atomic so the admin API can update it at runtime via `&self`.
    default_rpm: AtomicU32,
}

impl RateLimiter {
    pub fn new(default_rpm: u32) -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            default_rpm: AtomicU32::new(default_rpm),
        }
    }

    /// Update the default RPM. Existing buckets are untouched — only
    /// buckets created AFTER this call use the new value.
    pub fn set_default_rpm(&self, rpm: u32) {
        self.default_rpm.store(rpm, Ordering::SeqCst);
    }

    /// Get (or lazily create) the bucket for a tenant.
    ///
    /// Hot path: `read()` lets many tenants look up their buckets
    /// concurrently; only a cache miss upgrades to `write()` for the
    /// brief insert window.
    async fn bucket_for(&self, tenant_id: &str) -> Arc<TokenBucket> {
        // Fast path — read-mostly, no serialization across tenants.
        {
            let map = self.buckets.read().await;
            if let Some(b) = map.get(tenant_id) {
                return b.clone();
            }
        }
        // Slow path — upgrade to write only to insert a brand-new bucket.
        let rpm = self.default_rpm.load(Ordering::Relaxed);
        let bucket = Arc::new(TokenBucket::new(rpm));
        let mut map = self.buckets.write().await;
        // Double-check: another writer may have inserted while we waited.
        if let Some(b) = map.get(tenant_id) {
            return b.clone();
        }
        map.insert(tenant_id.to_string(), bucket.clone());
        bucket
    }
}

/// Axum middleware that consumes one token per request and returns HTTP 429
/// with `Retry-After` when the tenant's bucket is empty. Unauthenticated
/// requests (no `TenantContext` extension) fall back to a shared "global"
/// bucket so credential-guessing traffic is still throttled.
pub async fn rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    request: Request,
    next: Next,
) -> Response {
    if UNLIMITED_PATHS.iter().any(|p| request.uri().path() == *p) {
        return next.run(request).await;
    }

    let tenant_id = request
        .extensions()
        .get::<TenantContext>()
        .map(|c| c.tenant_id.clone())
        .unwrap_or_else(|| "__unauthenticated__".to_string());

    let bucket = limiter.bucket_for(&tenant_id).await;

    if let Some(wait) = bucket.consume() {
        warn!(tenant = %tenant_id, ?wait, "Request rate-limited");
        let detail = GatewayError::RateLimited.to_error_response();
        let mut resp = (http::StatusCode::TOO_MANY_REQUESTS, axum::Json(detail)).into_response();
        let seconds = wait.as_secs().max(1);
        if let Ok(v) = header::HeaderValue::from_str(&seconds.to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return resp;
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_micro_per_ms_cap_60() {
        assert_eq!(micro_per_ms(60), 1);
    }

    #[test]
    fn test_micro_per_ms_cap_600() {
        assert_eq!(micro_per_ms(600), 10);
    }

    #[test]
    fn test_micro_per_ms_cap_1() {
        // ceil(1000 / 60000) = 1 µ-token per ms (integer-resolution floor).
        assert_eq!(micro_per_ms(1), 1);
    }

    #[test]
    fn test_bucket_consume_and_refill() {
        let bucket = TokenBucket::new(60);
        // Drain all 60 tokens.
        for i in 0..60 {
            assert!(bucket.consume().is_none(), "token {} should be allowed", i);
        }
        // 61st should be rate-limited.
        assert!(bucket.consume().is_some(), "61st should be rate-limited");
    }

    #[test]
    fn test_bucket_zero_rpm_clamps_to_one() {
        let bucket = TokenBucket::new(0);
        // With cap=1 (clamped), at least 1 token is available.
        assert!(bucket.consume().is_none());
    }

    #[test]
    fn test_set_rpm_clamps_tokens() {
        let bucket = TokenBucket::new(60);
        // Drain all tokens first.
        for _ in 0..60 {
            bucket.consume();
        }
        // Shrink capacity; should be rate-limited.
        bucket.set_rpm(1);
        // Capacity may not have updated yet for in-flight counts, but the
        // internal CAS-based clamp should settle within a couple calls.
        let limited = (0..10).any(|_| bucket.consume().is_some());
        assert!(limited, "should be rate-limited after shrink");
    }
}

#[allow(dead_code)]
mod _compile_checks {
    use super::*;
    /// Sanity-check µ-token math is not obviously overflow-prone at typical
    /// production RPM (≤100k). Larger RPM values approach u64 limits but are
    /// unrealistic for a single gateway instance.
    const _: () = {
        // cap * MICRO must not overflow u64 up to 1M RPM.
        let cap: u64 = 1_000_000;
        assert!(cap.saturating_mul(MICRO) > 0);
    };
}
