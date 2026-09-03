use serde::{Deserialize, Serialize};

use super::UsdAmount;
use crate::{Model, responses::Usage};

// OpenAI publishes rates per one million tokens. All supported rates convert
// exactly to nano-USD per token, avoiding floating point and division.
const SOL_STANDARD: TokenRates = TokenRates {
    input: 4_000,
    cached_input: 400,
    cache_write_input: 5_000,
    output: 20_000,
};
const SOL_PRIORITY: TokenRates = TokenRates {
    input: 8_000,
    cached_input: 800,
    cache_write_input: 10_000,
    output: 40_000,
};
const SOL_LONG_CONTEXT_STANDARD: TokenRates = TokenRates {
    input: 8_000,
    cached_input: 800,
    cache_write_input: 10_000,
    output: 30_000,
};
const SOL_LONG_CONTEXT_PRIORITY: TokenRates = TokenRates {
    input: 16_000,
    cached_input: 1_600,
    cache_write_input: 20_000,
    output: 60_000,
};
const TERRA_STANDARD: TokenRates = TokenRates {
    input: 2_000,
    cached_input: 200,
    cache_write_input: 2_500,
    output: 12_000,
};
const TERRA_PRIORITY: TokenRates = TokenRates {
    input: 4_000,
    cached_input: 400,
    cache_write_input: 5_000,
    output: 24_000,
};
const TERRA_LONG_CONTEXT_STANDARD: TokenRates = TokenRates {
    input: 4_000,
    cached_input: 400,
    cache_write_input: 5_000,
    output: 18_000,
};
const TERRA_LONG_CONTEXT_PRIORITY: TokenRates = TokenRates {
    input: 8_000,
    cached_input: 800,
    cache_write_input: 10_000,
    output: 36_000,
};
const LUNA_STANDARD: TokenRates = TokenRates {
    input: 200,
    cached_input: 20,
    cache_write_input: 250,
    output: 1_200,
};
const LUNA_PRIORITY: TokenRates = TokenRates {
    input: 400,
    cached_input: 40,
    cache_write_input: 500,
    output: 2_400,
};
const LUNA_LONG_CONTEXT_STANDARD: TokenRates = TokenRates {
    input: 400,
    cached_input: 40,
    cache_write_input: 500,
    output: 1_800,
};
const LUNA_LONG_CONTEXT_PRIORITY: TokenRates = TokenRates {
    input: 800,
    cached_input: 80,
    cache_write_input: 1_000,
    output: 3_600,
};
const ASTRA_STANDARD: TokenRates = TokenRates {
    input: 10_000,
    cached_input: 1_000,
    cache_write_input: 12_500,
    output: 50_000,
};
const ASTRA_PRIORITY: TokenRates = TokenRates {
    input: 20_000,
    cached_input: 2_000,
    cache_write_input: 25_000,
    output: 100_000,
};
const ASTRA_LONG_CONTEXT_STANDARD: TokenRates = TokenRates {
    input: 20_000,
    cached_input: 2_000,
    cache_write_input: 25_000,
    output: 75_000,
};
const ASTRA_LONG_CONTEXT_PRIORITY: TokenRates = TokenRates {
    input: 40_000,
    cached_input: 4_000,
    cache_write_input: 50_000,
    output: 150_000,
};
const LONG_CONTEXT_THRESHOLD: u64 = 272_000;

#[derive(Clone, Copy)]
struct TokenRates {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
}

impl TokenRates {
    const fn for_model(model: Model, service_tier: ServiceTier, input_tokens: u64) -> Self {
        let fast = !matches!(service_tier, ServiceTier::Standard);
        let long = input_tokens > LONG_CONTEXT_THRESHOLD;
        match (model, fast, long) {
            (Model::Sol, false, false) => SOL_STANDARD,
            (Model::Sol, true, false) => SOL_PRIORITY,
            (Model::Sol, false, true) => SOL_LONG_CONTEXT_STANDARD,
            (Model::Sol, true, true) => SOL_LONG_CONTEXT_PRIORITY,
            (Model::Terra, false, false) => TERRA_STANDARD,
            (Model::Terra, true, false) => TERRA_PRIORITY,
            (Model::Terra, false, true) => TERRA_LONG_CONTEXT_STANDARD,
            (Model::Terra, true, true) => TERRA_LONG_CONTEXT_PRIORITY,
            (Model::Luna, false, false) => LUNA_STANDARD,
            (Model::Luna, true, false) => LUNA_PRIORITY,
            (Model::Luna, false, true) => LUNA_LONG_CONTEXT_STANDARD,
            (Model::Luna, true, true) => LUNA_LONG_CONTEXT_PRIORITY,
            (Model::Astra, false, false) => ASTRA_STANDARD,
            (Model::Astra, true, false) => ASTRA_PRIORITY,
            (Model::Astra, false, true) => ASTRA_LONG_CONTEXT_STANDARD,
            (Model::Astra, true, true) => ASTRA_LONG_CONTEXT_PRIORITY,
        }
    }
}

/// OpenAI service tiers supported by Nanocodex.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    /// Standard processing and token rates.
    #[default]
    Standard,
    /// Priority processing selected by `fast_mode`.
    Priority,
    /// Fast processing label used for models newer than GPT-5.6.
    Fast,
}

impl ServiceTier {
    /// Returns the OpenAI service-tier name used in events and traces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Priority => "priority",
            Self::Fast => "fast",
        }
    }

    /// Resolves the tier label assumed for the selected model and mode.
    #[must_use]
    pub const fn for_model(model: Model, fast_mode: bool) -> Self {
        match (model, fast_mode) {
            (_, false) => Self::Standard,
            (Model::Astra, true) => Self::Fast,
            (_, true) => Self::Priority,
        }
    }
}

/// Exact estimated USD cost for provider-reported token usage.
///
/// Nanocodex calculates this automatically using the selected model's built-in
/// rates for the requested processing mode. This is a local estimate, not a
/// charge or observed service tier reported by the Responses API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EstimatedUsdCost {
    #[serde(rename = "usd")]
    amount: UsdAmount,
    #[serde(rename = "input_usd")]
    input: UsdAmount,
    #[serde(rename = "cached_input_usd")]
    cached_input: UsdAmount,
    #[serde(rename = "cache_write_input_usd")]
    cache_write_input: UsdAmount,
    #[serde(rename = "output_usd")]
    output: UsdAmount,
    #[serde(default)]
    service_tier: ServiceTier,
}

impl EstimatedUsdCost {
    /// Returns the exact aggregate estimate.
    #[must_use]
    pub const fn amount(&self) -> UsdAmount {
        self.amount
    }

    /// Returns the ordinary-input component.
    #[must_use]
    pub const fn input(&self) -> UsdAmount {
        self.input
    }

    /// Returns the cache-read component.
    #[must_use]
    pub const fn cached_input(&self) -> UsdAmount {
        self.cached_input
    }

    /// Returns the cache-write component.
    #[must_use]
    pub const fn cache_write_input(&self) -> UsdAmount {
        self.cache_write_input
    }

    /// Returns the output component, including reasoning output.
    #[must_use]
    pub const fn output(&self) -> UsdAmount {
        self.output
    }

    /// Returns the service tier whose built-in rates were applied.
    #[must_use]
    pub const fn service_tier(&self) -> ServiceTier {
        self.service_tier
    }

    /// Adds estimates from separate provider operations without reapplying
    /// per-request pricing thresholds to their aggregate token counts.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        debug_assert_eq!(self.service_tier, other.service_tier);
        Self {
            amount: self.amount.saturating_add(other.amount),
            input: self.input.saturating_add(other.input),
            cached_input: self.cached_input.saturating_add(other.cached_input),
            cache_write_input: self
                .cache_write_input
                .saturating_add(other.cache_write_input),
            output: self.output.saturating_add(other.output),
            service_tier: self.service_tier,
        }
    }
}

/// Estimates one provider operation from its authoritative usage record.
///
/// Cached and cache-write tokens are subsets of `input_tokens`; this function
/// subtracts both before pricing ordinary input. The returned value is a local
/// estimate, not a charge reported by the Responses API.
///
/// ```
/// use nanocodex_oai_api::{
///     pricing::{ServiceTier, estimate},
///     responses::{InputTokenDetails, Usage},
/// };
///
/// let usage = Usage {
///     input_tokens: 1_000,
///     input_tokens_details: Some(InputTokenDetails {
///         cached_tokens: 800,
///         cache_write_tokens: 100,
///     }),
///     output_tokens: 50,
///     total_tokens: 1_050,
///     ..Usage::default()
/// };
/// let cost = estimate(&usage, ServiceTier::Standard);
///
/// assert_eq!(cost.amount().decimal(), "0.00222");
/// ```
#[must_use]
pub fn estimate(usage: &Usage, service_tier: ServiceTier) -> EstimatedUsdCost {
    estimate_for_model(usage, Model::Sol, service_tier)
}

/// Estimates one provider operation using the selected model's built-in rates.
///
/// This is the model-aware form of [`estimate`]. Managed sessions use it so
/// every supported model receives an estimate from reported usage.
#[must_use]
pub fn estimate_for_model(
    usage: &Usage,
    model: Model,
    service_tier: ServiceTier,
) -> EstimatedUsdCost {
    let cached_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .map_or(0, |details| details.cached_tokens);
    let cache_write_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .map_or(0, |details| details.cache_write_tokens);
    estimate_tokens(
        usage.input_tokens,
        cached_input_tokens,
        cache_write_input_tokens,
        usage.output_tokens,
        model,
        service_tier,
    )
}

pub(crate) fn estimate_tokens(
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    model: Model,
    service_tier: ServiceTier,
) -> EstimatedUsdCost {
    let rates = TokenRates::for_model(model, service_tier, input_tokens);
    let cached_input_tokens = cached_input_tokens.min(input_tokens);
    let remaining_input = input_tokens.saturating_sub(cached_input_tokens);
    let cache_write_input_tokens = cache_write_input_tokens.min(remaining_input);
    let ordinary_input_tokens = remaining_input.saturating_sub(cache_write_input_tokens);

    let input = UsdAmount::saturating_mul(ordinary_input_tokens, rates.input);
    let cached_input = UsdAmount::saturating_mul(cached_input_tokens, rates.cached_input);
    let cache_write_input =
        UsdAmount::saturating_mul(cache_write_input_tokens, rates.cache_write_input);
    let output = UsdAmount::saturating_mul(output_tokens, rates.output);
    let amount = input
        .saturating_add(cached_input)
        .saturating_add(cache_write_input)
        .saturating_add(output);

    EstimatedUsdCost {
        amount,
        input,
        cached_input,
        cache_write_input,
        output,
        service_tier,
    }
}

#[cfg(test)]
mod tests {
    use super::{ServiceTier, estimate, estimate_for_model, estimate_tokens};
    use crate::{
        Model,
        responses::{InputTokenDetails, OutputTokenDetails, Usage},
    };

    #[test]
    fn standard_rates_price_each_input_class_once() {
        let estimate = estimate(
            &Usage {
                input_tokens: 1_000_000,
                input_tokens_details: Some(InputTokenDetails {
                    cached_tokens: 250_000,
                    cache_write_tokens: 100_000,
                }),
                output_tokens: 200_000,
                output_tokens_details: Some(OutputTokenDetails {
                    reasoning_tokens: 150_000,
                }),
                total_tokens: 1_200_000,
            },
            ServiceTier::Standard,
        );

        assert_eq!(estimate.input().decimal(), "5.2");
        assert_eq!(estimate.cached_input().decimal(), "0.2");
        assert_eq!(estimate.cache_write_input().decimal(), "1");
        assert_eq!(estimate.output().decimal(), "6");
        assert_eq!(estimate.amount().decimal(), "12.4");
    }

    #[test]
    fn priority_rates_are_selected_by_fast_mode() {
        let standard = estimate_tokens(
            1_000_000,
            0,
            0,
            1_000_000,
            Model::Sol,
            ServiceTier::Standard,
        );
        let priority = estimate_tokens(
            1_000_000,
            0,
            0,
            1_000_000,
            Model::Sol,
            ServiceTier::Priority,
        );

        assert_eq!(standard.amount().decimal(), "38");
        assert_eq!(priority.amount().decimal(), "76");
        assert_eq!(priority.service_tier(), ServiceTier::Priority);
        assert_eq!(priority.service_tier().as_str(), "priority");
    }

    #[test]
    fn luna_rates_cover_standard_and_priority_usage() {
        let usage = Usage {
            input_tokens: 1_000_000,
            input_tokens_details: Some(InputTokenDetails {
                cached_tokens: 200_000,
                cache_write_tokens: 100_000,
            }),
            output_tokens: 1_000_000,
            ..Usage::default()
        };

        let standard = estimate_for_model(&usage, Model::Luna, ServiceTier::Standard);
        let priority = estimate_for_model(&usage, Model::Luna, ServiceTier::Priority);

        assert_eq!(standard.input().decimal(), "0.28");
        assert_eq!(standard.cached_input().decimal(), "0.008");
        assert_eq!(standard.cache_write_input().decimal(), "0.05");
        assert_eq!(standard.output().decimal(), "1.8");
        assert_eq!(standard.amount().decimal(), "2.138");
        assert_eq!(priority.amount().decimal(), "4.276");
    }

    #[test]
    fn terra_rates_cover_standard_and_priority_usage() {
        let usage = Usage {
            input_tokens: 1_000_000,
            input_tokens_details: Some(InputTokenDetails {
                cached_tokens: 200_000,
                cache_write_tokens: 100_000,
            }),
            output_tokens: 1_000_000,
            ..Usage::default()
        };

        let standard = estimate_for_model(&usage, Model::Terra, ServiceTier::Standard);
        let priority = estimate_for_model(&usage, Model::Terra, ServiceTier::Priority);

        assert_eq!(standard.input().decimal(), "2.8");
        assert_eq!(standard.cached_input().decimal(), "0.08");
        assert_eq!(standard.cache_write_input().decimal(), "0.5");
        assert_eq!(standard.output().decimal(), "18");
        assert_eq!(standard.amount().decimal(), "21.38");
        assert_eq!(priority.amount().decimal(), "42.76");
    }

    #[test]
    fn astra_rates_include_long_context_and_priority_multipliers() {
        let standard = estimate_tokens(
            272_000,
            100_000,
            50_000,
            100_000,
            Model::Astra,
            ServiceTier::Standard,
        );
        let long_standard = estimate_tokens(
            272_001,
            100_000,
            50_000,
            100_000,
            Model::Astra,
            ServiceTier::Standard,
        );
        let long_fast = estimate_tokens(
            272_001,
            100_000,
            50_000,
            100_000,
            Model::Astra,
            ServiceTier::Fast,
        );

        assert_eq!(standard.amount().decimal(), "6.945");
        assert_eq!(long_standard.amount().decimal(), "11.39002");
        assert_eq!(long_fast.amount().decimal(), "22.78004");
        assert_eq!(long_fast.service_tier(), ServiceTier::Fast);
    }

    #[test]
    fn malformed_detail_counts_do_not_double_charge_input() {
        let estimate = estimate_tokens(10, 8, 8, 0, Model::Sol, ServiceTier::Standard);

        assert_eq!(estimate.input().nano_usd(), 0);
        assert_eq!(estimate.cached_input().nano_usd(), 3_200);
        assert_eq!(estimate.cache_write_input().nano_usd(), 10_000);
    }
}
