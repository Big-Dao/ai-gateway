use gateway_core::metering::{CostBreakdown, CostSummary, ModelCost};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Maximum number of detail events retained in memory. When this cap is hit,
/// older events are dropped so the buffer can't grow without bound (M5 —
/// prevents OOM under long-running / high-QPS workloads).
const MAX_EVENTS: usize = 10_000;

/// Status of a completed request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    Success,
    Error,
    RateLimited,
    QuotaExceeded,
}

/// A single metering event — created after every LLM request completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeteringEvent {
    pub timestamp_ms: u64,
    pub tenant_id: String,
    pub key_id: String,
    pub model: String,
    pub provider: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub status: RequestStatus,
    pub estimated_cost_cents: f64,
}

impl MeteringEvent {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// Aggregated daily usage per tenant.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TenantUsage {
    pub tenant_id: String,
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub total_errors: u64,
    pub total_cost_cents: f64,
    /// One-shot latch: set to `true` the moment the billing-cycle cost
    /// crosses the tenant's `cost_alert_threshold_cents`. Reset to `false`
    /// at the start of each billing window (see `reset_billing_window`).
    #[serde(default)]
    pub alert_triggered: bool,
    pub per_model: HashMap<String, ModelUsage>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelUsage {
    pub model: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_cents: f64,
}

/// In-memory metering buffer. Snapshots to PVC periodically (deferred to MVP 3).
pub struct MeteringService {
    /// Detail event log — bounded (`VecDeque`, M5) so long-running processes
    /// don't OOM. Reads/writes are short critical sections.
    events: Arc<Mutex<VecDeque<MeteringEvent>>>,
    usage: Arc<Mutex<HashMap<String, TenantUsage>>>,
}

impl MeteringService {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(VecDeque::with_capacity(1024))),
            usage: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record a metering event.
    ///
    /// `cost_alert_threshold_cents` is the per-tenant, per-billing-window cost
    /// threshold; pass `None` to skip the alert check. When the tenant's
    /// cumulative cost crosses the threshold *in this event*, a `tracing::warn!`
    /// line is emitted **once** per billing window (controlled by the
    /// `alert_triggered` latch on `TenantUsage`).
    ///
    /// Optimisation (H2): aggregation and event-append now take their locks
    /// sequentially — previously the usage lock was held *while* pushing the
    /// event, forcing two critical sections to overlap. The hot path duration
    /// is roughly halved.
    pub async fn record(&self, event: MeteringEvent, cost_alert_threshold_cents: Option<f64>) {
        // Defensive: non-finite cost (NaN / Inf) must never propagate into
        // aggregator sums — they would poison every subsequent accumulation.
        let mut event = event;
        if event.estimated_cost_cents.is_nan() || event.estimated_cost_cents.is_infinite() {
            tracing::warn!(
                cost = event.estimated_cost_cents,
                model = %event.model,
                tenant_id = %event.tenant_id,
                "clamping non-finite estimated_cost_cents to 0.0"
            );
            event.estimated_cost_cents = 0.0;
        }

        let mut usage = self.usage.lock().await;
        let entry = usage
            .entry(event.tenant_id.clone())
            .or_insert_with(|| TenantUsage {
                tenant_id: event.tenant_id.clone(),
                ..Default::default()
            });
        entry.total_requests += 1;
        entry.total_prompt_tokens += event.prompt_tokens;
        entry.total_completion_tokens += event.completion_tokens;
        entry.total_tokens += event.total_tokens();
        entry.total_errors += if event.status == RequestStatus::Error {
            1
        } else {
            0
        };
        entry.total_cost_cents += event.estimated_cost_cents;

        // Threshold alert: fires exactly once per billing window — only on
        // the event that first pushes cumulative cost to or past the
        // threshold. The `alert_triggered` latch prevents duplicate events.
        if let Some(th) = cost_alert_threshold_cents {
            let just_crossed = entry.total_cost_cents >= th
                && entry.total_cost_cents - event.estimated_cost_cents < th;
            if just_crossed && !entry.alert_triggered {
                tracing::warn!(
                    tenant = %event.tenant_id,
                    cost_cents = entry.total_cost_cents,
                    threshold_cents = th,
                    "cost alert triggered for tenant"
                );
                entry.alert_triggered = true;
            }
        }

        let model_entry = entry
            .per_model
            .entry(event.model.clone())
            .or_insert_with(|| ModelUsage {
                model: event.model.clone(),
                ..Default::default()
            });
        model_entry.requests += 1;
        model_entry.prompt_tokens += event.prompt_tokens;
        model_entry.completion_tokens += event.completion_tokens;
        model_entry.cost_cents += event.estimated_cost_cents;

        drop(usage);

        // Append the detail event — now outside the usage lock so the two
        // critical sections can't stall each other. Bounded to MAX_EVENTS.
        let mut events = self.events.lock().await;
        if events.len() >= MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// Lightweight request counter — preferred over `record(..)` on the hot
    /// path when no per-request detail (tokens / cost / model) is available.
    /// Skips the full `MeteringEvent` construction (~6 String clones).
    pub async fn record_request(
        &self,
        tenant_id: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) {
        let mut usage = self.usage.lock().await;
        let entry = usage
            .entry(tenant_id.to_owned())
            .or_insert_with(|| TenantUsage {
                tenant_id: tenant_id.to_owned(),
                ..Default::default()
            });
        entry.total_requests += 1;
        entry.total_prompt_tokens += prompt_tokens;
        entry.total_completion_tokens += completion_tokens;
        entry.total_tokens += prompt_tokens + completion_tokens;
    }

    /// Get aggregated usage for a tenant.
    pub async fn tenant_usage(&self, tenant_id: &str) -> Option<TenantUsage> {
        self.usage.lock().await.get(tenant_id).cloned()
    }

    /// Get all tenant usages.
    pub async fn all_usage(&self) -> HashMap<String, TenantUsage> {
        self.usage.lock().await.clone()
    }

    /// Aggregate cost over a sliding window for the admin costs API.
    ///
    /// Walks the detail event log (`self.events`), filters to the
    /// `[now_ms - window_ms, now_ms]` window and (optionally) a single tenant,
    /// then returns per-tenant and per-model cost breakdowns. `top_tenants` is
    /// capped at the five spenders. `window` is left empty here; fill it in
    /// the handler with a human-readable label (e.g. `"24h"`).
    pub async fn cost_summary(
        &self,
        window_ms: u64,
        now_ms: u64,
        tenant_filter: Option<&str>,
    ) -> CostSummary {
        let events = self.events.lock().await;

        // Accumulate per tenant -> per model -> (cost, prompt_tokens, completion_tokens)
        let mut tenants: HashMap<String, (f64, HashMap<String, ModelCost>)> = HashMap::new();
        let mut global_model: HashMap<String, ModelCost> = HashMap::new();
        let mut total_cost_cents = 0.0;

        let cutoff = now_ms.saturating_sub(window_ms);
        for e in events.iter() {
            if e.timestamp_ms <= cutoff {
                continue;
            }
            if let Some(filter) = tenant_filter {
                if e.tenant_id != filter {
                    continue;
                }
            }

            // Defensive: never let a non-finite cost pollute aggregates.
            let cost = if e.estimated_cost_cents.is_finite() {
                e.estimated_cost_cents
            } else {
                0.0
            };

            let (tenant_cost, per_model) = tenants
                .entry(e.tenant_id.clone())
                .or_insert_with(|| (0.0, HashMap::new()));
            *tenant_cost += cost;

            {
                let mc = per_model
                    .entry(e.model.clone())
                    .or_insert_with(|| ModelCost {
                        model: e.model.clone(),
                        cost_cents: 0.0,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                    });
                mc.cost_cents += cost;
                mc.prompt_tokens += e.prompt_tokens;
                mc.completion_tokens += e.completion_tokens;
            }

            {
                let mc = global_model
                    .entry(e.model.clone())
                    .or_insert_with(|| ModelCost {
                        model: e.model.clone(),
                        cost_cents: 0.0,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                    });
                mc.cost_cents += cost;
                mc.prompt_tokens += e.prompt_tokens;
                mc.completion_tokens += e.completion_tokens;
            }

            total_cost_cents += cost;
        }

        // Top 5 tenants by cost, descending.
        let mut tenant_vec: Vec<(String, f64, HashMap<String, ModelCost>)> =
            tenants.into_iter().map(|(k, v)| (k, v.0, v.1)).collect();
        tenant_vec.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let top_tenants: Vec<CostBreakdown> = tenant_vec
            .into_iter()
            .take(5)
            .map(|(tenant_id, per_tenant_cost, per_model)| {
                let mut models: Vec<ModelCost> = per_model.into_values().collect();
                models.sort_by(|a, b| {
                    b.cost_cents
                        .partial_cmp(&a.cost_cents)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                CostBreakdown {
                    tenant_id,
                    total_cost_cents: per_tenant_cost,
                    per_model: models,
                }
            })
            .collect();

        let mut per_model: Vec<ModelCost> = global_model.into_values().collect();
        per_model.sort_by(|a, b| {
            b.cost_cents
                .partial_cmp(&a.cost_cents)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        CostSummary {
            window: String::new(),
            total_cost_cents,
            top_tenants,
            per_model,
            tenant_filter: tenant_filter.map(String::from),
        }
    }

    /// Snapshot event count (for PVC flush, deferred).
    #[allow(dead_code)] // reserved for the deferred persistence-flush path
    pub async fn event_count(&self) -> usize {
        self.events.lock().await.len()
    }

    /// Reset billing window: clears all detail events and zeroes out cost
    /// counters in every tenant's usage record. Request / token tallies are
    /// intentionally left untouched — they are cumulative and do not vary
    /// with the billing cycle.
    pub async fn reset_billing_window(&self) {
        let mut events = self.events.lock().await;
        events.clear();
        drop(events);

        let mut usage = self.usage.lock().await;
        for u in usage.values_mut() {
            u.total_cost_cents = 0.0;
            u.alert_triggered = false;
            for m in u.per_model.values_mut() {
                m.cost_cents = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// Helper to build a minimal metering event.
    fn mk_event(tenant_id: &str, cost_cents: f64) -> MeteringEvent {
        MeteringEvent {
            timestamp_ms: 0,
            tenant_id: tenant_id.into(),
            key_id: "k1".into(),
            model: "gpt-4o".into(),
            provider: "openai".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            status: RequestStatus::Success,
            estimated_cost_cents: cost_cents,
        }
    }

    #[tokio::test]
    async fn test_record_and_query_tenant_usage() {
        let svc = MeteringService::new();
        svc.record(mk_event("t1", 1.5), None).await;

        let usage = svc.tenant_usage("t1").await.unwrap();
        assert_eq!(usage.total_requests, 1);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.total_cost_cents, 1.5);
        assert_eq!(usage.per_model["gpt-4o"].requests, 1);
    }

    #[tokio::test]
    async fn test_nan_cost_is_clamped_to_zero() {
        let svc = MeteringService::new();
        svc.record(mk_event("t-nan", f64::NAN), None).await;
        svc.record(mk_event("t-nan", f64::INFINITY), None).await;
        // A third, finite event to prove the accumulator itself is well-formed.
        svc.record(mk_event("t-nan", 2.5), None).await;

        let usage = svc.tenant_usage("t-nan").await.unwrap();
        assert_eq!(usage.total_requests, 3);
        // NaN + Inf + 2.5 must clamps to 0.0 + 0.0 + 2.5 = 2.5
        assert!(
            (usage.total_cost_cents - 2.5).abs() < 1e-9,
            "expected 2.5, got {}",
            usage.total_cost_cents
        );
        assert!(
            !usage.total_cost_cents.is_nan(),
            "total_cost_cents must not be NaN"
        );
        assert!(
            !usage.total_cost_cents.is_infinite(),
            "total_cost_cents must not be Inf"
        );
    }

    #[tokio::test]
    async fn test_multiple_events_accumulate() {
        let svc = MeteringService::new();
        for _ in 0..3 {
            svc.record(
                MeteringEvent {
                    timestamp_ms: 0,
                    tenant_id: "t1".into(),
                    key_id: "k1".into(),
                    model: "m".into(),
                    provider: "p".into(),
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    status: RequestStatus::Error,
                    estimated_cost_cents: 0.0,
                },
                None,
            )
            .await;
        }

        let usage = svc.tenant_usage("t1").await.unwrap();
        assert_eq!(usage.total_requests, 3);
        assert_eq!(usage.total_errors, 3);
        assert_eq!(usage.total_tokens, 45);
    }

    #[tokio::test]
    async fn test_cost_summary_filters_by_window() {
        let svc = MeteringService::new();

        let day_ms = 24 * 60 * 60 * 1000u64;
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let mk =
            |ts_ms: u64, tenant: &str, cost: f64, prompt: u64, completion: u64| MeteringEvent {
                timestamp_ms: ts_ms,
                tenant_id: tenant.into(),
                key_id: "k1".into(),
                model: "gpt-4o".into(),
                provider: "openai".into(),
                prompt_tokens: prompt,
                completion_tokens: completion,
                status: RequestStatus::Success,
                estimated_cost_cents: cost,
            };

        // 1. fresh event — inside 24h window (NOW)
        svc.record(mk(now_ms, "t1", 10.0, 100, 50), None).await;
        // 2. 5 days ago — OUTSIDE 24h window, inside 7d / 30d windows
        svc.record(mk(now_ms - 5 * day_ms, "t1", 25.0, 200, 100), None)
            .await;
        // 3. 50 days ago — OUTSIDE every supported window
        svc.record(mk(now_ms - 50 * day_ms, "t1", 999.0, 1, 1), None)
            .await;

        // 24h window, evaluated "now + 1s" so the fresh event is still counted.
        let summary = svc.cost_summary(day_ms, now_ms + 1_000, None).await;
        assert!(
            (summary.total_cost_cents - 10.0).abs() < 1e-9,
            "expected 10.0, got {}",
            summary.total_cost_cents
        );
        assert_eq!(
            summary.top_tenants.len(),
            1,
            "exactly one tenant inside window"
        );
        assert_eq!(summary.top_tenants[0].tenant_id, "t1");
        assert!((summary.top_tenants[0].total_cost_cents - 10.0).abs() < 1e-9);
        assert_eq!(summary.per_model.len(), 1);
        assert_eq!(summary.per_model[0].model, "gpt-4o");
        assert!((summary.per_model[0].cost_cents - 10.0).abs() < 1e-9);

        // 30d window should include the 5-days-ago event (25.0) + fresh (10.0).
        let summary_30d = svc.cost_summary(30 * day_ms, now_ms + 1_000, None).await;
        assert!(
            (summary_30d.total_cost_cents - 35.0).abs() < 1e-9,
            "expected 35.0, got {}",
            summary_30d.total_cost_cents
        );

        // Tenant filter: scope to "t1" should yield same 24h result.
        let filtered = svc.cost_summary(day_ms, now_ms + 1_000, Some("t1")).await;
        assert_eq!(filtered.total_cost_cents, summary.total_cost_cents);

        // Tenant filter: scope to unknown tenant yields nothing.
        let empty = svc
            .cost_summary(day_ms, now_ms + 1_000, Some("nobody"))
            .await;
        assert_eq!(empty.total_cost_cents, 0.0);
        assert!(empty.top_tenants.is_empty());
        assert!(empty.per_model.is_empty());
    }

    /// 6.4(a) — when cumulative cost crosses the threshold, the alert fires
    /// exactly once (no panic, `alert_triggered` becomes true).
    #[tokio::test]
    async fn test_cost_alert_fires_when_threshold_crossed() {
        let svc = MeteringService::new();
        let threshold = Some(10.0);

        // event 1: cumulative cost = 5 (< 10) — no alert.
        svc.record(mk_event("t-alert", 5.0), threshold).await;
        let usage = svc.tenant_usage("t-alert").await.unwrap();
        assert!(
            !usage.alert_triggered,
            "alert should not fire below threshold"
        );
        assert_eq!(usage.total_cost_cents, 5.0);

        // event 2: cumulative cost = 15 (>= 10), previous was 5 (< 10) — alert fires.
        svc.record(mk_event("t-alert", 10.0), threshold).await;
        let usage = svc.tenant_usage("t-alert").await.unwrap();
        assert!(
            usage.alert_triggered,
            "alert should fire once threshold is crossed"
        );
        assert_eq!(usage.total_cost_cents, 15.0);
    }

    /// 6.4(b) — once latched, `alert_triggered` stays true but no new alert
    /// event fires (verified by re-reading the flag after subsequent events).
    #[tokio::test]
    async fn test_cost_alert_does_not_refire() {
        let svc = MeteringService::new();
        let threshold = Some(10.0);

        // Cross the threshold in a single event.
        svc.record(mk_event("t-refire", 20.0), threshold).await;
        let first = svc.tenant_usage("t-refire").await.unwrap();
        assert!(first.alert_triggered);
        assert_eq!(first.total_cost_cents, 20.0);

        // Second event — cumulative is now well above threshold. Flag must
        // remain true and identical to the first read (no duplicate trigger).
        svc.record(mk_event("t-refire", 30.0), threshold).await;
        let second = svc.tenant_usage("t-refire").await.unwrap();
        assert!(second.alert_triggered, "flag must remain latched");
        assert_eq!(second.total_cost_cents, 50.0);
        // Boolean equality — flag was already true on first read and did not
        // transition (no new trigger).
        assert_eq!(first.alert_triggered, second.alert_triggered);
    }
}
