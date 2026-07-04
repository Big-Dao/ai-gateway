use gateway_core::error::GatewayError;
use gateway_core::tenant::TenantQuotas;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Result of a quota check.
pub type QuotaCheckResult = Result<(), QuotaViolation>;

/// Details about a quota violation.
#[derive(Debug, Clone)]
pub struct QuotaViolation {
    pub limit_type: String,
    pub limit: u64,
    pub current: u64,
}

impl From<QuotaViolation> for GatewayError {
    fn from(v: QuotaViolation) -> Self {
        GatewayError::QuotaExceeded {
            limit_type: v.limit_type,
            limit: v.limit,
            current: v.current,
        }
    }
}

/// Per-tenant quota state window.
#[derive(Debug, Default)]
struct TenantQuotaState {
    /// Current minute bucket (Unix minute).
    current_minute: u64,
    /// Requests this minute.
    rpm_this_minute: u64,
    /// Tokens this minute.
    tpm_this_minute: u64,
    /// Current day bucket (Unix day).
    current_day: u64,
    /// Requests today.
    rpd_today: u64,
    /// Tokens today.
    tpd_today: u64,
}

fn current_unix_minute() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60
}

fn current_unix_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400
}

/// QuotaEngine enforces per-tenant RPM / RPD / TPM / TPD limits.
pub struct QuotaEngine {
    states: Arc<Mutex<HashMap<String, TenantQuotaState>>>,
}

impl QuotaEngine {
    pub fn new() -> Self {
        Self {
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check whether a request is allowed under the tenant's quota.
    /// `tokens_estimated` is an upper-bound estimate of the tokens for this request.
    /// Returns Ok(()) if allowed, or Err(QuotaViolation) if exceeded.
    pub async fn check(
        &self,
        tenant_id: &str,
        quotas: &TenantQuotas,
        tokens_estimated: u64,
    ) -> QuotaCheckResult {
        let mut states = self.states.lock().await;
        let state = states.entry(tenant_id.to_string()).or_default();

        let now_minute = current_unix_minute();
        let now_day = current_unix_day();

        // Reset minute bucket if minute changed
        if state.current_minute != now_minute {
            state.current_minute = now_minute;
            state.rpm_this_minute = 0;
            state.tpm_this_minute = 0;
        }
        // Reset day bucket if day changed
        if state.current_day != now_day {
            state.current_day = now_day;
            state.rpd_today = 0;
            state.tpd_today = 0;
        }

        // 0 = unlimited
        if quotas.max_rpm > 0 {
            let new_rpm = state.rpm_this_minute + 1;
            if new_rpm > quotas.max_rpm as u64 {
                warn!(tenant = %tenant_id, rpm = new_rpm, limit = quotas.max_rpm, "RPM quota exceeded");
                return Err(QuotaViolation {
                    limit_type: "rpm".into(),
                    limit: quotas.max_rpm as u64,
                    current: state.rpm_this_minute,
                });
            }
        }
        if quotas.max_tpm > 0 {
            let new_tpm = state.tpm_this_minute + tokens_estimated;
            if new_tpm > quotas.max_tpm {
                return Err(QuotaViolation {
                    limit_type: "tpm".into(),
                    limit: quotas.max_tpm,
                    current: state.tpm_this_minute,
                });
            }
        }
        if quotas.max_rpd > 0 {
            let new_rpd = state.rpd_today + 1;
            if new_rpd > quotas.max_rpd {
                return Err(QuotaViolation {
                    limit_type: "rpd".into(),
                    limit: quotas.max_rpd,
                    current: state.rpd_today,
                });
            }
        }
        if quotas.max_tpd > 0 {
            let new_tpd = state.tpd_today + tokens_estimated;
            if new_tpd > quotas.max_tpd {
                return Err(QuotaViolation {
                    limit_type: "tpd".into(),
                    limit: quotas.max_tpd,
                    current: state.tpd_today,
                });
            }
        }

        // Record usage
        state.rpm_this_minute += 1;
        state.tpm_this_minute += tokens_estimated;
        state.rpd_today += 1;
        state.tpd_today += tokens_estimated;

        debug!(tenant = %tenant_id, rpm = state.rpm_this_minute, tpm = state.tpm_this_minute, "quota check passed");
        Ok(())
    }

    /// Get current usage snapshot for a tenant.
    #[allow(dead_code)] // exposed for the admin/observability API
    pub async fn tenant_state(&self, tenant_id: &str) -> Option<(u64, u64, u64, u64)> {
        let states = self.states.lock().await;
        states.get(tenant_id).map(|s| {
            (
                s.rpm_this_minute,
                s.tpm_this_minute,
                s.rpd_today,
                s.tpd_today,
            )
        })
    }

    /// Reset all quotas (for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    pub async fn reset(&self) {
        self.states.lock().await.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quotas(rpm: u32, rpd: u64, tpm: u64, tpd: u64) -> TenantQuotas {
        TenantQuotas {
            max_rpm: rpm,
            max_rpd: rpd,
            max_tpm: tpm,
            max_tpd: tpd,
        }
    }

    #[tokio::test]
    async fn test_zero_means_unlimited() {
        let engine = QuotaEngine::new();
        let q = quotas(0, 0, 0, 0);
        for _ in 0..1000 {
            engine.check("t1", &q, 100).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_rpm_limit_enforced() {
        let engine = QuotaEngine::new();
        let q = quotas(3, 0, 0, 0);
        engine.check("t1", &q, 0).await.unwrap();
        engine.check("t1", &q, 0).await.unwrap();
        engine.check("t1", &q, 0).await.unwrap();
        let result = engine.check("t1", &q, 0).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.limit_type, "rpm");
        assert_eq!(err.limit, 3);
    }

    #[tokio::test]
    async fn test_tpm_limit_enforced() {
        let engine = QuotaEngine::new();
        let q = quotas(0, 0, 100, 0);
        engine.check("t1", &q, 50).await.unwrap();
        engine.check("t1", &q, 40).await.unwrap();
        // 50+40+20=110 > 100, should fail
        let result = engine.check("t1", &q, 20).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_per_tenant_isolation() {
        let engine = QuotaEngine::new();
        let q = quotas(1, 0, 0, 0);
        engine.check("tenant-a", &q, 0).await.unwrap();
        // tenant-a blocked now
        assert!(engine.check("tenant-a", &q, 0).await.is_err());
        // tenant-b still allowed
        engine.check("tenant-b", &q, 0).await.unwrap();
    }
}
