//! Core metering primitives shared between gateway-server and observability.
//!
//! [`RateCard`] defines the cost model for billing. [`MeteringEvent`] is the
//! per-request record emitted after each LLM call completes.

use serde::{Deserialize, Serialize};

use crate::types::Usage;

/// Cost rate per token.
///
/// Stored in `AppConfig.rate_config` (platform-wide default). Operators can
/// later extend this to per-provider/model pricing via admin config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RateCard {
    /// Cost per 1M prompt tokens, in the smallest currency unit (e.g. cents).
    #[serde(default)]
    pub prompt_per_million: u64,
    /// Cost per 1M completion tokens, in the smallest currency unit.
    #[serde(default)]
    pub completion_per_million: u64,
}

impl Default for RateCard {
    fn default() -> Self {
        Self {
            prompt_per_million: 0,
            completion_per_million: 0,
        }
    }
}

impl RateCard {
    /// Compute the estimated cost in the card's base unit using ceil division
    /// so sub-million-token charges are not rounded to zero.
    pub fn estimate_cost(&self, usage: &Usage) -> u64 {
        let prompt_units = (usage.prompt_tokens as u64 + 999_999) / 1_000_000;
        let completion_units = (usage.completion_tokens as u64 + 999_999) / 1_000_000;
        prompt_units
            .saturating_mul(self.prompt_per_million)
            .saturating_add(completion_units.saturating_mul(self.completion_per_million))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_free() {
        let card = RateCard::default();
        let usage = Usage {
            prompt_tokens: 100_000,
            completion_tokens: 50_000,
            total_tokens: 150_000,
        };
        assert_eq!(card.estimate_cost(&usage), 0);
    }

    #[test]
    fn test_ceil_division() {
        let card = RateCard {
            prompt_per_million: 30,
            completion_per_million: 60,
        };
        let usage = Usage {
            prompt_tokens: 500_000,
            completion_tokens: 250_000,
            total_tokens: 750_000,
        };
        // 500k → ceil = 1 * 30 = 30
        // 250k → ceil = 1 * 60 = 60
        assert_eq!(card.estimate_cost(&usage), 90);
    }

    #[test]
    fn test_large_usage() {
        let card = RateCard {
            prompt_per_million: 1500,
            completion_per_million: 6000,
        };
        let usage = Usage {
            prompt_tokens: 1_500_000,
            completion_tokens: 2_000_000,
            total_tokens: 3_500_000,
        };
        // 1.5M → ceil = 2 * 1500 = 3000
        // 2M → 2 * 6000 = 12000
        assert_eq!(card.estimate_cost(&usage), 15000);
    }
}
