pub use nanocodex_oai_api::pricing::{CostStatus, EstimatedUsdCost, ServiceTier, UsdAmount};
use serde::{Deserialize, Serialize};

/// Exact token accounting for every Responses call in one logical agent turn.
///
/// Cache-read and cache-write tokens are subsets of input tokens. Reasoning
/// tokens are a subset of output tokens. The values are summed from provider
/// usage records across warmup, generation, tool continuation, steering, and
/// compaction calls made before the turn reaches its terminal boundary. Check
/// [`Self::cost_status`] to distinguish a provider-omitted usage record from a
/// genuine zero-token total.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[allow(clippy::struct_field_names)]
pub struct TurnUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
    estimated_cost: Option<Box<EstimatedUsdCost>>,
    cost_status: CostStatus,
}

/// Exact turn usage reported by an external backend.
///
/// Every field is named and required so wire adapters cannot silently swap or
/// default adjacent token counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportedTurnUsage {
    /// All input tokens billed or reported by the backend.
    pub input_tokens: u64,
    /// Input tokens served from the backend's prompt cache.
    pub cached_input_tokens: u64,
    /// Input tokens newly written into the backend's prompt cache.
    pub cache_write_input_tokens: u64,
    /// All output tokens billed or reported by the backend.
    pub output_tokens: u64,
    /// Reasoning tokens included within `output_tokens`.
    pub reasoning_output_tokens: u64,
    /// Backend-reported aggregate token count.
    pub total_tokens: u64,
    /// Exact retained cost estimate, when one was reported.
    pub estimated_cost: Option<EstimatedUsdCost>,
    /// Availability and provenance of `estimated_cost`.
    pub cost_status: CostStatus,
}

#[allow(clippy::struct_field_names)]
#[derive(Clone)]
#[cfg(feature = "openai")]
pub(crate) struct TurnUsageCounts {
    pub(crate) input_tokens: u64,
    pub(crate) cached_input_tokens: u64,
    pub(crate) cache_write_input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) reasoning_output_tokens: u64,
    pub(crate) total_tokens: u64,
    pub(crate) reported: bool,
    pub(crate) estimated_cost: Option<EstimatedUsdCost>,
}

impl TurnUsage {
    /// Constructs exact usage reported by an external backend.
    ///
    /// All counts, the optional retained estimate, and its status are explicit
    /// so a wire boundary cannot silently default an omitted field. This API
    /// lets dependency-light backends construct usage without a serialization
    /// round trip.
    #[must_use]
    pub fn from_reported(reported: ReportedTurnUsage) -> Self {
        Self {
            input_tokens: reported.input_tokens,
            cached_input_tokens: reported.cached_input_tokens,
            cache_write_input_tokens: reported.cache_write_input_tokens,
            output_tokens: reported.output_tokens,
            reasoning_output_tokens: reported.reasoning_output_tokens,
            total_tokens: reported.total_tokens,
            estimated_cost: reported.estimated_cost.map(Box::new),
            cost_status: reported.cost_status,
        }
    }

    #[cfg(feature = "openai")]
    pub(crate) fn from_counts(counts: TurnUsageCounts) -> Self {
        let (estimated_cost, cost_status) = if !counts.reported {
            (None, CostStatus::UsageNotReported)
        } else {
            (
                counts.estimated_cost.map(Box::new),
                CostStatus::EstimatedFromUsage,
            )
        };
        Self {
            input_tokens: counts.input_tokens,
            cached_input_tokens: counts.cached_input_tokens,
            cache_write_input_tokens: counts.cache_write_input_tokens,
            output_tokens: counts.output_tokens,
            reasoning_output_tokens: counts.reasoning_output_tokens,
            total_tokens: counts.total_tokens,
            estimated_cost,
            cost_status,
        }
    }

    /// Returns all input tokens billed or reported by the provider.
    #[must_use]
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    /// Returns input tokens served from the provider's prompt cache.
    #[must_use]
    pub const fn cached_input_tokens(&self) -> u64 {
        self.cached_input_tokens
    }

    /// Returns input tokens newly written into the provider's prompt cache.
    #[must_use]
    pub const fn cache_write_input_tokens(&self) -> u64 {
        self.cache_write_input_tokens
    }

    /// Returns all output tokens billed or reported by the provider.
    #[must_use]
    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    /// Returns reasoning tokens included within [`Self::output_tokens`].
    #[must_use]
    pub const fn reasoning_output_tokens(&self) -> u64 {
        self.reasoning_output_tokens
    }

    /// Returns the provider-reported total token count.
    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// Returns the automatic local USD estimate.
    ///
    /// Nanocodex applies the selected model's built-in rates for the requested
    /// processing mode. `None` means the provider omitted usage;
    /// [`Self::cost_status`] distinguishes that from a genuine zero-token
    /// estimate.
    #[must_use]
    pub fn estimated_cost(&self) -> Option<&EstimatedUsdCost> {
        self.estimated_cost.as_deref()
    }

    /// Returns why an estimate is present or unavailable.
    #[must_use]
    pub const fn cost_status(&self) -> CostStatus {
        self.cost_status
    }
}

#[cfg(test)]
mod tests {
    use super::{CostStatus, ReportedTurnUsage, TurnUsage};

    #[test]
    fn externally_reported_usage_preserves_exact_counts_and_cost_status() {
        let usage = TurnUsage::from_reported(ReportedTurnUsage {
            input_tokens: 13,
            cached_input_tokens: 5,
            cache_write_input_tokens: 2,
            output_tokens: 8,
            reasoning_output_tokens: 3,
            total_tokens: 21,
            estimated_cost: None,
            cost_status: CostStatus::UsageNotReported,
        });

        assert_eq!(usage.input_tokens(), 13);
        assert_eq!(usage.cached_input_tokens(), 5);
        assert_eq!(usage.cache_write_input_tokens(), 2);
        assert_eq!(usage.output_tokens(), 8);
        assert_eq!(usage.reasoning_output_tokens(), 3);
        assert_eq!(usage.total_tokens(), 21);
        assert_eq!(usage.cost_status(), CostStatus::UsageNotReported);
        assert!(usage.estimated_cost().is_none());
    }
}
