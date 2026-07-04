use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Circuit breaker states following the classic state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — requests pass through.
    Closed,
    /// Failing — all requests immediately rejected.
    Open,
    /// Testing recovery — allowing a trial request through.
    HalfOpen,
}

/// Per-provider circuit breaker tracking.
struct BreakerInner {
    state: CircuitState,
    /// Timestamps of recent failures (within the window).
    failures: Vec<Instant>,
    /// When the circuit was opened (for computing remaining cooldown).
    opened_at: Option<Instant>,
    /// Number of consecutive successes in HalfOpen state.
    half_open_successes: u32,
}

/// Configuration for the circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Failure window duration — only failures within this window count.
    pub failure_window: Duration,
    /// Number of failures within the window to trigger Open.
    pub failure_threshold: u32,
    /// How long to stay Open before transitioning to HalfOpen.
    pub cooldown: Duration,
    /// Consecutive successes in HalfOpen to return to Closed.
    pub half_open_success_threshold: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_window: Duration::from_secs(30),
            failure_threshold: 5,
            cooldown: Duration::from_secs(60),
            half_open_success_threshold: 1,
        }
    }
}

/// Thread-safe, per-provider circuit breaker.
pub struct CircuitBreaker {
    inner: RwLock<HashMap<String, BreakerInner>>,
    config: CircuitBreakerConfig,
    /// Global request counter for metrics.
    total_rejected: AtomicU64,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(HashMap::new()),
            config,
            total_rejected: AtomicU64::new(0),
        })
    }

    /// Check if a provider is currently allowed to receive requests.
    ///
    /// Uses a read lock for the fast path (Closed / HalfOpen / Open-with-unelapsed-cooldown)
    /// so that concurrent requests for different providers are not serialized.
    /// Only the rare Open→HalfOpen transition falls through to a write lock.
    pub async fn allow_request(&self, provider_name: &str) -> bool {
        // Fast path — read lock: concurrent providers can proceed in parallel.
        {
            let map = self.inner.read().await;
            match map.get(provider_name) {
                // No entry yet ≡ Closed — allow without creating one.
                None => return true,
                Some(entry) => match entry.state {
                    CircuitState::Closed => return true,
                    CircuitState::HalfOpen => return true,
                    CircuitState::Open => {
                        let cooldown_done = entry
                            .opened_at
                            .map(|t| Instant::now().duration_since(t) >= self.config.cooldown)
                            .unwrap_or(false);
                        if cooldown_done {
                            // Cooldown elapsed — fall through to upgrade to write lock.
                        } else {
                            self.total_rejected.fetch_add(1, Ordering::Relaxed);
                            return false;
                        }
                    }
                },
            }
        }

        // Slow path — write lock: only reached for Open→HalfOpen transition.
        let mut map = self.inner.write().await;
        let now = Instant::now();

        let entry = map
            .entry(provider_name.to_string())
            .or_insert_with(|| BreakerInner {
                state: CircuitState::Closed,
                failures: Vec::new(),
                opened_at: None,
                half_open_successes: 0,
            });

        // Prune stale failures outside the window.
        entry
            .failures
            .retain(|t| now.duration_since(*t) < self.config.failure_window);

        match entry.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                if entry
                    .opened_at
                    .map_or(false, |t| now.duration_since(t) >= self.config.cooldown)
                {
                    debug!(
                        provider = provider_name,
                        "Circuit breaker transitioning to HalfOpen"
                    );
                    entry.state = CircuitState::HalfOpen;
                    entry.half_open_successes = 0;
                    true
                } else {
                    self.total_rejected.fetch_add(1, Ordering::Relaxed);
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Record a successful request.
    pub async fn record_success(&self, provider_name: &str) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(provider_name) {
            match entry.state {
                CircuitState::HalfOpen => {
                    entry.half_open_successes += 1;
                    if entry.half_open_successes >= self.config.half_open_success_threshold {
                        info!(
                            provider = provider_name,
                            "Circuit breaker closing — provider recovered"
                        );
                        entry.state = CircuitState::Closed;
                        entry.failures.clear();
                        entry.opened_at = None;
                        entry.half_open_successes = 0;
                    }
                }
                CircuitState::Closed => {
                    // Success in closed state — optionally clear failures
                    // to avoid stale failures accumulating.
                    entry.failures.clear();
                }
                CircuitState::Open => {
                    // Shouldn't happen, but handle gracefully.
                    warn!(provider = provider_name, "Success recorded while Open");
                }
            }
        }
    }

    /// Record a failed request.
    pub async fn record_failure(&self, provider_name: &str) {
        let mut map = self.inner.write().await;
        let now = Instant::now();

        let entry = map
            .entry(provider_name.to_string())
            .or_insert_with(|| BreakerInner {
                state: CircuitState::Closed,
                failures: Vec::new(),
                opened_at: None,
                half_open_successes: 0,
            });

        // Prune old failures.
        entry
            .failures
            .retain(|t| now.duration_since(*t) < self.config.failure_window);
        entry.failures.push(now);

        match entry.state {
            CircuitState::Closed => {
                if entry.failures.len() as u32 >= self.config.failure_threshold {
                    warn!(
                        provider = provider_name,
                        failures = entry.failures.len(),
                        "Circuit breaker opening — too many failures"
                    );
                    entry.state = CircuitState::Open;
                    entry.opened_at = Some(now);
                }
            }
            CircuitState::HalfOpen => {
                // Failure in half-open → back to open.
                warn!(
                    provider = provider_name,
                    "Circuit breaker re-opening — failure during HalfOpen probe"
                );
                entry.state = CircuitState::Open;
                entry.opened_at = Some(now);
                entry.half_open_successes = 0;
            }
            CircuitState::Open => {
                // Already open, just track.
            }
        }
    }

    /// Get the current state of a provider's circuit breaker.
    pub async fn state(&self, provider_name: &str) -> CircuitState {
        let map = self.inner.read().await;
        map.get(provider_name)
            .map(|e| e.state)
            .unwrap_or(CircuitState::Closed)
    }

    /// Get a snapshot of all circuit breaker states.
    pub async fn all_states(&self) -> HashMap<String, CircuitState> {
        let map = self.inner.read().await;
        map.iter().map(|(k, v)| (k.clone(), v.state)).collect()
    }

    /// Total requests rejected due to open circuits.
    pub fn total_rejected(&self) -> u64 {
        self.total_rejected.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) async fn inject_state(
        &self,
        provider_name: &str,
        state: CircuitState,
        failures: usize,
        opened_at: Option<Instant>,
    ) {
        let mut map = self.inner.write().await;
        let mut entry = BreakerInner {
            state,
            failures: Vec::new(),
            opened_at,
            half_open_successes: 0,
        };
        // Simulate historical failures for state Closed→Open testing.
        for i in 0..failures {
            entry
                .failures
                .push(Instant::now() - Duration::from_secs(i as u64 + 1));
        }
        map.insert(provider_name.to_string(), entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_window: Duration::from_secs(30),
            failure_threshold: 3,
            cooldown: Duration::from_secs(60),
            half_open_success_threshold: 1,
        }
    }

    #[tokio::test]
    async fn test_closed_to_open_on_threshold() {
        let cb = CircuitBreaker::new(test_config());
        let p = "p1";

        // Initially closed.
        assert_eq!(cb.state(p).await, CircuitState::Closed);
        assert!(cb.allow_request(p).await);

        // Record failures up to threshold.
        cb.record_failure(p).await;
        cb.record_failure(p).await;
        assert_eq!(cb.state(p).await, CircuitState::Closed);
        cb.record_failure(p).await; // triggers Open
        assert_eq!(cb.state(p).await, CircuitState::Open);
        assert!(!cb.allow_request(p).await);
        assert_eq!(cb.total_rejected(), 1);
    }

    #[tokio::test]
    async fn test_open_rejects_all_requests() {
        let cb = CircuitBreaker::new(test_config());
        let p = "p2";

        cb.inject_state(p, CircuitState::Open, 0, Some(Instant::now()))
            .await;
        assert!(!cb.allow_request(p).await);
        assert!(!cb.allow_request(p).await);
        assert_eq!(cb.total_rejected(), 2);
    }

    #[tokio::test]
    async fn test_open_to_halfopen_after_cooldown() {
        let cb = CircuitBreaker::new(test_config());
        let p = "p3";

        // Inject Open state with `opened_at` far enough in the past.
        let far_past = Instant::now() - Duration::from_secs(120);
        cb.inject_state(p, CircuitState::Open, 0, Some(far_past))
            .await;

        // allow_request sees cooldown elapsed → transitions to HalfOpen.
        assert!(cb.allow_request(p).await);
        assert_eq!(cb.state(p).await, CircuitState::HalfOpen);
    }

    #[tokio::test]
    async fn test_halfopen_to_closed_on_success() {
        let cb = CircuitBreaker::new(test_config());
        let p = "p4";

        cb.inject_state(p, CircuitState::HalfOpen, 0, None).await;
        assert!(cb.allow_request(p).await);

        cb.record_success(p).await; // threshold = 1, so this closes it.
        assert_eq!(cb.state(p).await, CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_halfopen_to_open_on_failure() {
        let cb = CircuitBreaker::new(test_config());
        let p = "p5";

        cb.inject_state(p, CircuitState::HalfOpen, 0, None).await;
        assert!(cb.allow_request(p).await);

        cb.record_failure(p).await; // probe failed → back to Open
        assert_eq!(cb.state(p).await, CircuitState::Open);
        assert!(!cb.allow_request(p).await);
    }

    #[tokio::test]
    async fn test_closed_success_clears_failures() {
        let cb = CircuitBreaker::new(test_config());
        let p = "p6";

        cb.record_failure(p).await;
        cb.record_failure(p).await;
        // Still closed (below threshold).
        assert_eq!(cb.state(p).await, CircuitState::Closed);

        cb.record_success(p).await;
        // Failures cleared — adding one more won't trip the breaker.
        cb.record_failure(p).await;
        cb.record_failure(p).await;
        assert_eq!(cb.state(p).await, CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_all_states_snapshot() {
        let cb = CircuitBreaker::new(test_config());
        cb.inject_state("a", CircuitState::Closed, 0, None).await;
        cb.inject_state("b", CircuitState::Open, 0, Some(Instant::now()))
            .await;
        cb.inject_state("c", CircuitState::HalfOpen, 0, None).await;

        let snap = cb.all_states().await;
        assert_eq!(snap["a"], CircuitState::Closed);
        assert_eq!(snap["b"], CircuitState::Open);
        assert_eq!(snap["c"], CircuitState::HalfOpen);
    }
}
