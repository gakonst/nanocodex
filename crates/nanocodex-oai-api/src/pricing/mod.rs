//! Built-in USD estimates for supported OpenAI models.
//!
//! Sol, Luna, and Astra responses are priced automatically from
//! provider-reported token usage and the selected standard or fast service tier.
//!
//! Rates are sourced from OpenAI's [pricing page](https://developers.openai.com/api/docs/pricing)
//! and the model pages for [Sol](https://developers.openai.com/api/docs/models/gpt-6-sol),
//! [Luna](https://developers.openai.com/api/docs/models/gpt-6-luna), and
//! [Astra](https://developers.openai.com/api/docs/models/gpt-6-astra).
//!
//! | Model | Service tier | Input | Cached input | Cache write | Output |
//! | --- | --- | ---: | ---: | ---: | ---: |
//! | Sol | Standard | $2.00 | $0.20 | $2.50 | $10.00 |
//! | Sol | Fast (`priority`) | $4.00 | $0.40 | $5.00 | $20.00 |
//! | Luna | Standard | $0.10 | $0.01 | $0.125 | $0.50 |
//! | Luna | Fast (`priority`) | $0.20 | $0.02 | $0.25 | $1.00 |
//! | Astra | Standard | $10.00 | $1.00 | $12.50 | $50.00 |
//! | Astra | Fast (`fast`) | $20.00 | $2.00 | $25.00 | $100.00 |
//!
//! Prices are per one million tokens. Reasoning tokens are already included
//! in output tokens and are not charged a second time. GPT-6 requests with
//! more than 272,000 input tokens apply each model's published 2x input/cache
//! and 1.5x output long-context multipliers before fast-mode pricing.

mod amount;
mod estimate;

use serde::{Deserialize, Serialize};

pub use amount::UsdAmount;
pub use estimate::{EstimatedUsdCost, ServiceTier, estimate, estimate_for_model};

/// Availability of the automatic local USD estimate.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CostStatus {
    /// Provider usage was priced using the built-in rates.
    EstimatedFromUsage,
    /// The provider omitted usage from the completed response.
    #[default]
    UsageNotReported,
    /// A retained record used a newer or unknown status.
    #[serde(other)]
    Other,
}

impl CostStatus {
    /// Returns the stable snake-case wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EstimatedFromUsage => "estimated_from_usage",
            Self::UsageNotReported => "usage_not_reported",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::CostStatus;

    #[test]
    fn cost_status_has_a_forward_compatible_wire_shape() {
        assert_eq!(
            serde_json::to_value(CostStatus::EstimatedFromUsage).unwrap(),
            json!("estimated_from_usage")
        );
        assert_eq!(
            serde_json::from_value::<CostStatus>(json!("future_status")).unwrap(),
            CostStatus::Other
        );
    }
}
