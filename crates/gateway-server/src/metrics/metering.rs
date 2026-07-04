use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;
use tracing::info;

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
    events: Arc<Mutex<Vec<MeteringEvent>>>,
    usage: Arc<Mutex<HashMap<String, TenantUsage>>>,
}

impl MeteringService {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::with_capacity(1024))),
            usage: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record a metering event.
    pub async fn record(&self, event: MeteringEvent) {
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
        self.events.lock().await.push(event);
    }

    /// Get aggregated usage for a tenant.
    pub async fn tenant_usage(&self, tenant_id: &str) -> Option<TenantUsage> {
        self.usage.lock().await.get(tenant_id).cloned()
    }

    /// Get all tenant usages.
    pub async fn all_usage(&self) -> HashMap<String, TenantUsage> {
        self.usage.lock().await.clone()
    }

    /// Snapshot event count (for PVC flush, deferred).
    pub async fn event_count(&self) -> usize {
        self.events.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_record_and_query_tenant_usage() {
        let svc = MeteringService::new();
        svc.record(MeteringEvent {
            timestamp_ms: 0,
            tenant_id: "t1".into(),
            key_id: "k1".into(),
            model: "gpt-4o".into(),
            provider: "openai".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            status: RequestStatus::Success,
            estimated_cost_cents: 1.5,
        })
        .await;

        let usage = svc.tenant_usage("t1").await.unwrap();
        assert_eq!(usage.total_requests, 1);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.total_cost_cents, 1.5);
        assert_eq!(usage.per_model["gpt-4o"].requests, 1);
    }

    #[tokio::test]
    async fn test_multiple_events_accumulate() {
        let svc = MeteringService::new();
        for _ in 0..3 {
            svc.record(MeteringEvent {
                timestamp_ms: 0,
                tenant_id: "t1".into(),
                key_id: "k1".into(),
                model: "m".into(),
                provider: "p".into(),
                prompt_tokens: 10,
                completion_tokens: 5,
                status: RequestStatus::Error,
                estimated_cost_cents: 0.0,
            })
            .await;
        }

        let usage = svc.tenant_usage("t1").await.unwrap();
        assert_eq!(usage.total_requests, 3);
        assert_eq!(usage.total_errors, 3);
        assert_eq!(usage.total_tokens, 45);
    }
}
