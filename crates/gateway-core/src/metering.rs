//! Core metering primitives shared between gateway-server and observability.
//!
//! [`RateCard`] defines the cost model for billing. [`MeteringEvent`] is the
//! per-request record emitted after each LLM call completes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::Usage;

/// Cost rate per token.
///
/// Stored in `AppConfig.rate_config` (platform-wide default). Operators can
/// later extend this to per-provider/model pricing via admin config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RateCard {
    /// Cost per 1M prompt tokens, in the smallest currency unit (e.g. cents).
    #[serde(default)]
    pub prompt_per_million: u64,
    /// Cost per 1M completion tokens, in the smallest currency unit.
    #[serde(default)]
    pub completion_per_million: u64,
}

impl RateCard {
    /// Compute the estimated cost in the card's base unit using ceil division
    /// so sub-million-token charges are not rounded to zero.
    pub fn estimate_cost(&self, usage: &Usage) -> u64 {
        let prompt_units = (usage.prompt_tokens as u64).div_ceil(1_000_000);
        let completion_units = (usage.completion_tokens as u64).div_ceil(1_000_000);
        prompt_units
            .saturating_mul(self.prompt_per_million)
            .saturating_add(completion_units.saturating_mul(self.completion_per_million))
    }
}

/// Per-model pricing rate for [`PricingTable`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRate {
    /// Cost per 1M input (prompt) tokens, in currency unit (e.g. cents).
    pub input_per_1m: f64,
    /// Cost per 1M output (completion) tokens, in currency unit (e.g. cents).
    pub output_per_1m: f64,
}

/// A lookup table of per-model pricing rates, with optional platform default.
///
/// Replaces the platform-wide [`RateCard`] with per-model granularity.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingTable {
    /// Fallback rate when a model has no explicit entry.
    pub default: Option<ModelRate>,
    /// Per-model rates keyed by model name.
    pub models: HashMap<String, ModelRate>,
}

impl PricingTable {
    /// Estimate the cost for (`model`, `usage`) in the table's base unit.
    ///
    /// Falls back to `default` when the model has no explicit entry; returns
    /// `0.0` when the table is entirely empty (i.e. free). NaN/Inf results are
    /// clamped to `0.0` with a warn-level log.
    pub fn estimate_cost(&self, model: &str, usage: &Usage) -> f64 {
        let rate = self.models.get(model).or(self.default.as_ref());
        match rate {
            Some(r) => {
                let cost = (usage.prompt_tokens as f64 * r.input_per_1m
                    + usage.completion_tokens as f64 * r.output_per_1m)
                    / 1_000_000.0;
                if cost.is_nan() || cost.is_infinite() {
                    tracing::warn!(model, "pricing produced NaN/Inf, clamping to 0.0");
                    0.0
                } else {
                    cost
                }
            }
            None => {
                tracing::debug!(model, "no pricing entry, free");
                0.0
            }
        }
    }
}

/// Per-tenant cost aggregation returned by [`crate::metrics::metering::MeteringService::cost_summary`].
#[derive(Debug, Clone, Serialize)]
pub struct CostBreakdown {
    pub tenant_id: String,
    pub total_cost_cents: f64,
    pub per_model: Vec<ModelCost>,
}

/// Cost contributions, grouped by model, for a single tenant.
#[derive(Debug, Clone, Serialize)]
pub struct ModelCost {
    pub model: String,
    pub cost_cents: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// A window-wide cost summary across all visible tenants.
#[derive(Debug, Clone, Serialize)]
pub struct CostSummary {
    pub window: String,
    pub total_cost_cents: f64,
    pub top_tenants: Vec<CostBreakdown>,
    pub per_model: Vec<ModelCost>,
    pub tenant_filter: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: u32, completion: u32) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        }
    }

    #[test]
    fn test_default_free() {
        let card = RateCard::default();
        let u = usage(100_000, 50_000);
        assert_eq!(card.estimate_cost(&u), 0);
    }

    #[test]
    fn test_ceil_division() {
        let card = RateCard {
            prompt_per_million: 30,
            completion_per_million: 60,
        };
        let u = usage(500_000, 250_000);
        // 500k → ceil = 1 * 30 = 30
        // 250k → ceil = 1 * 60 = 60
        assert_eq!(card.estimate_cost(&u), 90);
    }

    #[test]
    fn test_large_usage() {
        let card = RateCard {
            prompt_per_million: 1500,
            completion_per_million: 6000,
        };
        let u = usage(1_500_000, 2_000_000);
        // 1.5M → ceil = 2 * 1500 = 3000
        // 2M → 2 * 6000 = 12000
        assert_eq!(card.estimate_cost(&u), 15000);
    }

    #[test]
    fn test_pricing_hit_exact() {
        let mut table = PricingTable::default();
        table.models.insert(
            "gpt-4o".into(),
            ModelRate {
                input_per_1m: 2.5,
                output_per_1m: 10.0,
            },
        );
        let u = usage(1_000_000, 500_000);
        // prompt: 1.0 * 2.5 = 2.5
        // completion: 0.5 * 10.0 = 5.0
        // total: 7.5 cents
        assert!((table.estimate_cost("gpt-4o", &u) - 7.5).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_fallback_to_default() {
        let table = PricingTable {
            default: Some(ModelRate {
                input_per_1m: 5.0,
                output_per_1m: 15.0,
            }),
            ..Default::default()
        };
        let u = usage(1_000_000, 1_000_000);
        // 1.0 * 5.0 + 1.0 * 15.0 = 20.0
        assert!((table.estimate_cost("unknown-model", &u) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn test_pricing_empty_free() {
        let table = PricingTable::default();
        let u = usage(1_000_000, 1_000_000);
        assert_eq!(table.estimate_cost("gpt-4o", &u), 0.0);
    }

    #[test]
    fn test_pricing_fractional_precision() {
        let mut table = PricingTable::default();
        table.models.insert(
            "gpt-4o".into(),
            ModelRate {
                input_per_1m: 2.5,
                output_per_1m: 10.0,
            },
        );
        let u = usage(500_000, 0);
        // 0.5 * 2.5 = 1.25 cents
        assert!((table.estimate_cost("gpt-4o", &u) - 1.25).abs() < 1e-9);
    }
}
