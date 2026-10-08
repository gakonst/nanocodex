use serde::{Deserialize, Serialize};

use super::UsdAmount;
use crate::{Model, responses::Usage};

// OpenAI publishes rates per one million tokens. All supported rates convert
// exactly to nano-USD per token, avoiding floating point and division.
const SOL_STANDARD: TokenRates = TokenRates {
    input: 2_000,
    cached_input: 100,
    cache_write_input: 2_500,
    output: 10_000,
};
const SOL_PRIORITY: TokenRates = TokenRates {
    input: 4_000,
    cached_input: 200,
    cache_write_input: 5_000,
    output: 20_000,
};
const SOL_LONG_CONTEXT_STANDARD: TokenRates = TokenRates {
    input: 4_000,
    cached_input: 200,
    cache_write_input: 5_000,
    output: 15_000,
};
const SOL_LONG_CONTEXT_PRIORITY: TokenRates = TokenRates {
    input: 8_000,
    cached_input: 400,
    cache_write_input: 10_000,
    output: 30_000,
};
const LUNA_STANDARD: TokenRates = TokenRates {
    input: 100,
    cached_input: 10,
    cache_write_input: 125,
    output: 500,
};
const LUNA_PRIORITY: TokenRates = TokenRates {
    input: 200,
    cached_input: 20,
    cache_write_input: 250,
    output: 1_000,
};
const LUNA_LONG_CONTEXT_STANDARD: TokenRates = TokenRates {
    input: 200,
    cached_input: 20,
    cache_write_input: 250,
    output: 750,
};
const LUNA_LONG_CONTEXT_PRIORITY: TokenRates = TokenRates {
    input: 400,
    cached_input: 40,
    cache_write_input: 500,
    output: 1_500,
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

// https://developers.openai.com/api/docs/pricing?latest-pricing=ultrafast
const SOL_ULTRAFAST: TokenRates = TokenRates {
    input: 12_000,
    cached_input: 600,
    cache_write_input: 15_000,
    output: 60_000,
};
const SOL_LONG_CONTEXT_ULTRAFAST: TokenRates = TokenRates {
    input: 24_000,
    cached_input: 1_200,
    cache_write_input: 30_000,
    output: 90_000,
};
const ASTRA_ULTRAFAST: TokenRates = TokenRates {
    input: 60_000,
    cached_input: 6_000,
    cache_write_input: 75_000,
    output: 300_000,
};
const ASTRA_LONG_CONTEXT_ULTRAFAST: TokenRates = TokenRates {
    input: 120_000,
    cached_input: 12_000,
    cache_write_input: 150_000,
    output: 450_000,
};

// https://developers.cloudflare.com/workers-ai/models/glm-5.3/
// Workers AI publishes no separate cache-write or priority surcharge.
const GLM53_STANDARD: TokenRates = TokenRates {
    input: 1_400,
    cached_input: 260,
    cache_write_input: 1_400,
    output: 4_400,
};
// Gateway catalog base hints verified 2026-09-22; not actual provider billing.
// MiMo cached input rounds $0.0036/M up to the estimator's $0.001/M unit.
const KIMI_STANDARD: TokenRates = TokenRates {
    input: 3_000,
    cached_input: 300,
    cache_write_input: 3_000,
    output: 15_000,
};
const MIMO_STANDARD: TokenRates = TokenRates {
    input: 435,
    cached_input: 4,
    cache_write_input: 435,
    output: 870,
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
    /// Selects published rates for an already model-clamped tier; Luna has no
    /// Ultrafast rates because `ServiceTier::effective_for_model` runs it as Fast.
    const fn for_model(model: Model, service_tier: ServiceTier, input_tokens: u64) -> Self {
        use ServiceTier::{Fast, Priority, Standard, Ultrafast};

        let long = input_tokens > LONG_CONTEXT_THRESHOLD;
        match (model, service_tier, long) {
            (Model::Glm53, _, _) => GLM53_STANDARD,
            (Model::Kimi, _, _) => KIMI_STANDARD,
            (Model::Mimo, _, _) => MIMO_STANDARD,
            (Model::Sol, Standard, false) => SOL_STANDARD,
            (Model::Sol, Priority | Fast, false) => SOL_PRIORITY,
            (Model::Sol, Ultrafast, false) => SOL_ULTRAFAST,
            (Model::Sol, Standard, true) => SOL_LONG_CONTEXT_STANDARD,
            (Model::Sol, Priority | Fast, true) => SOL_LONG_CONTEXT_PRIORITY,
            (Model::Sol, Ultrafast, true) => SOL_LONG_CONTEXT_ULTRAFAST,
            (Model::Luna, Standard, false) => LUNA_STANDARD,
            (Model::Luna, Priority | Fast | Ultrafast, false) => LUNA_PRIORITY,
            (Model::Luna, Standard, true) => LUNA_LONG_CONTEXT_STANDARD,
            (Model::Luna, Priority | Fast | Ultrafast, true) => LUNA_LONG_CONTEXT_PRIORITY,
            (Model::Astra, Standard, false) => ASTRA_STANDARD,
            (Model::Astra, Priority | Fast, false) => ASTRA_PRIORITY,
            (Model::Astra, Ultrafast, false) => ASTRA_ULTRAFAST,
            (Model::Astra, Standard, true) => ASTRA_LONG_CONTEXT_STANDARD,
            (Model::Astra, Priority | Fast, true) => ASTRA_LONG_CONTEXT_PRIORITY,
            (Model::Astra, Ultrafast, true) => ASTRA_LONG_CONTEXT_ULTRAFAST,
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
    /// Compatibility alias for Fast processing.
    Priority,
    /// Fast processing and token rates.
    Fast,
    /// Ultrafast processing and token rates, supported by Astra and Sol.
    Ultrafast,
}

impl ServiceTier {
    /// Returns the OpenAI service-tier name used in events and traces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Priority => "priority",
            Self::Fast => "fast",
            Self::Ultrafast => "ultrafast",
        }
    }

    /// Converts the legacy priority-processing switch into a tier preference.
    #[must_use]
    pub const fn from_fast_mode(enabled: bool) -> Self {
        if enabled { Self::Fast } else { Self::Standard }
    }

    /// Clamps a preference to the fastest tier supported by the selected model.
    #[must_use]
    pub const fn effective_for_model(self, model: Model) -> Self {
        match (model, self) {
            (Model::Glm53 | Model::Kimi | Model::Mimo, _) => Self::Standard,
            (Model::Luna, Self::Ultrafast) => Self::Fast,
            _ => self,
        }
    }

    /// Resolves the tier label assumed for the selected model and legacy mode.
    #[must_use]
    pub const fn for_model(model: Model, fast_mode: bool) -> Self {
        Self::from_fast_mode(fast_mode).effective_for_model(model)
    }

    /// Returns the Responses API `service_tier` value, omitting Standard.
    #[cfg(feature = "client")]
    pub(crate) const fn request_value(self, model: Model) -> Option<&'static str> {
        match self.effective_for_model(model) {
            Self::Standard => None,
            Self::Priority | Self::Fast => Some("priority"),
            Self::Ultrafast => Some("ultrafast"),
        }
    }
}

impl From<bool> for ServiceTier {
    fn from(enabled: bool) -> Self {
        Self::from_fast_mode(enabled)
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
/// assert_eq!(cost.amount().decimal(), "0.00103");
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
    let service_tier = service_tier.effective_for_model(model);
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
    fn glm53_rates_are_independent_of_priority_and_context() {
        for tier in [
            ServiceTier::Standard,
            ServiceTier::Priority,
            ServiceTier::Fast,
        ] {
            let cost = estimate_for_model(
                &Usage {
                    input_tokens: 1_000_000,
                    input_tokens_details: Some(InputTokenDetails {
                        cached_tokens: 250_000,
                        cache_write_tokens: 0,
                    }),
                    output_tokens: 1_000_000,
                    output_tokens_details: None,
                    total_tokens: 2_000_000,
                },
                Model::Glm53,
                tier,
            );
            assert_eq!(cost.amount().decimal(), "5.515");
        }
        assert_eq!(
            ServiceTier::for_model(Model::Glm53, true),
            ServiceTier::Standard
        );
    }

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

        assert_eq!(estimate.input().decimal(), "2.6");
        assert_eq!(estimate.cached_input().decimal(), "0.05");
        assert_eq!(estimate.cache_write_input().decimal(), "0.5");
        assert_eq!(estimate.output().decimal(), "3");
        assert_eq!(estimate.amount().decimal(), "6.15");
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

        assert_eq!(standard.amount().decimal(), "19");
        assert_eq!(priority.amount().decimal(), "38");
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

        assert_eq!(standard.input().decimal(), "0.14");
        assert_eq!(standard.cached_input().decimal(), "0.004");
        assert_eq!(standard.cache_write_input().decimal(), "0.025");
        assert_eq!(standard.output().decimal(), "0.75");
        assert_eq!(standard.amount().decimal(), "0.919");
        assert_eq!(priority.amount().decimal(), "1.838");
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
    fn sol_ultrafast_rates_include_cache_and_long_context_while_luna_runs_fast() {
        let sol = |input_tokens| {
            estimate_tokens(
                input_tokens,
                100_000,
                50_000,
                100_000,
                Model::Sol,
                ServiceTier::Ultrafast,
            )
        };
        let (short, long) = (sol(272_000), sol(272_001));
        assert_eq!(
            [
                short.input(),
                short.cached_input(),
                short.cache_write_input(),
                short.output(),
                short.amount(),
            ]
            .map(|amount| amount.decimal()),
            ["1.464", "0.06", "0.75", "6", "8.274"]
        );
        assert_eq!(
            [
                long.input(),
                long.cached_input(),
                long.cache_write_input(),
                long.output(),
                long.amount(),
            ]
            .map(|amount| amount.decimal()),
            ["2.928024", "0.12", "1.5", "9", "13.548024"]
        );
        assert_eq!(long.service_tier(), ServiceTier::Ultrafast);

        let luna = estimate_tokens(
            272_001,
            100_000,
            50_000,
            100_000,
            Model::Luna,
            ServiceTier::Ultrafast,
        );
        assert_eq!(luna.amount().decimal(), "0.2278004");
        assert_eq!(luna.service_tier(), ServiceTier::Fast);
    }

    #[test]
    fn malformed_detail_counts_do_not_double_charge_input() {
        let estimate = estimate_tokens(10, 8, 8, 0, Model::Sol, ServiceTier::Standard);

        assert_eq!(estimate.input().nano_usd(), 0);
        assert_eq!(estimate.cached_input().nano_usd(), 800);
        assert_eq!(estimate.cache_write_input().nano_usd(), 5_000);
    }
}
