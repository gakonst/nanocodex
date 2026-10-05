//! Explicit economic cache warming for the Claude-native durable adapter.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Caller-supplied forecasts and prices. No provider rates or reuse forecasts are inferred.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheWarmPolicy {
    /// Native cache lifetime: 300 or 3600 seconds.
    pub ttl_seconds: u64,
    /// Maximum cumulative warm spend, including uncertain reservations.
    pub max_spend_usd: f64,
    /// Expected bill for this warm request.
    pub estimated_write_usd: f64,
    /// Expected bill per cached reuse.
    pub estimated_read_usd: f64,
    /// Expected bill per equivalent uncached request.
    pub estimated_uncached_usd: f64,
    /// Probability of the forecast reuses, between zero and one.
    pub reuse_probability: f64,
    /// Forecast number of reuses.
    pub expected_reuse_count: u64,
    /// Forecast latest reuse must fall within the cache lifetime.
    pub expected_reuse_within_seconds: u64,
    /// Live caller-supplied input rate in USD per million tokens.
    pub input_usd_per_million: f64,
    /// Live caller-supplied output rate in USD per million tokens.
    pub output_usd_per_million: f64,
    /// Live caller-supplied cache-write rate for the selected TTL.
    pub cache_write_usd_per_million: f64,
    /// Live caller-supplied cache-read rate.
    pub cache_read_usd_per_million: f64,
}

impl CacheWarmPolicy {
    /// Validate the explicit forecasts before admitting a charge.
    pub fn validate(&self) -> Result<()> {
        let values = [
            self.max_spend_usd,
            self.estimated_write_usd,
            self.estimated_read_usd,
            self.estimated_uncached_usd,
            self.input_usd_per_million,
            self.output_usd_per_million,
            self.cache_write_usd_per_million,
            self.cache_read_usd_per_million,
        ];
        if values
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
            || self.max_spend_usd == 0.0
            || self.estimated_write_usd == 0.0
            || !matches!(self.ttl_seconds, 300 | 3600)
            || !self.reuse_probability.is_finite()
            || self.reuse_probability <= 0.0
            || self.reuse_probability > 1.0
            || self.expected_reuse_count == 0
            || self.expected_reuse_within_seconds == 0
            || self.expected_reuse_within_seconds > self.ttl_seconds
        {
            return Err(invalid(
                "cache warming requires finite explicit prices, spend, TTL and reuse forecasts",
            ));
        }
        let savings = self.reuse_probability
            * self.expected_reuse_count as f64
            * (self.estimated_uncached_usd - self.estimated_read_usd);
        if !savings.is_finite()
            || savings <= self.estimated_write_usd
            || self.estimated_write_usd > self.max_spend_usd
        {
            return Err(invalid(
                "cache warm expected savings must exceed write cost within spend limit",
            ));
        }
        Ok(())
    }

    fn cost(&self, usage: &nanocodex_claude::Usage) -> Result<f64> {
        let cost = (usage.input_tokens as f64 * self.input_usd_per_million
            + usage.output_tokens as f64 * self.output_usd_per_million
            + usage.cache_creation_input_tokens as f64 * self.cache_write_usd_per_million
            + usage.cache_read_input_tokens as f64 * self.cache_read_usd_per_million)
            / 1_000_000.0;
        if !cost.is_finite() {
            return Err(invalid("cache warm usage cost overflow"));
        }
        Ok(cost)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Budget {
    reserved_usd: f64,
    actual_usd: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    budget: Budget,
    usage: nanocodex_claude::Usage,
    actual_usd: f64,
}
fn invalid(message: &str) -> Error {
    Error::InvalidState(message.into())
}

const BUDGET_KEY: &str = "nanocodex.cache-warm.budget";

async fn retained_budget(
    owner: &crate::session::DurableOwner,
    state: &Value,
) -> Result<(u64, Budget)> {
    let saved = owner.document(BUDGET_KEY).await?;
    let version = saved.as_ref().map_or(0, |document| document.version);
    let budget = match saved {
        Some(document) => serde_json::from_value(document.value)?,
        None => serde_json::from_value(
            state
                .get("cache_warm_budget")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"reserved_usd":0.0,"actual_usd":0.0})),
        )?,
    };
    Ok((version, budget))
}

fn budget_write(version: u64, budget: &Budget) -> Result<crate::DocumentWrite> {
    Ok(crate::DocumentWrite {
        key: BUDGET_KEY.into(),
        expected_version: version,
        value: serde_json::to_value(budget)?,
        fork: crate::DocumentForkPolicy::Current,
    })
}

pub(crate) async fn warm(
    owner: &crate::session::DurableOwner,
    operation: &str,
    request_id: &str,
    mut prepared: nanocodex_claude::execution::RequestPreparation,
    client: &nanocodex_claude::ClaudeClient,
    policy: &CacheWarmPolicy,
) -> Result<nanocodex_claude::execution::RequestPreparation> {
    policy.validate()?;
    let mut request: nanocodex_claude::MessagesRequest =
        serde_json::from_value(prepared.request.clone())?;
    // Match the exact native generation prefix, including its terminal system
    // breakpoint. This is the same prefix operation used by Claude dispatch.
    request
        .cache_system_prefix()
        .map_err(|error| invalid(&error.to_string()))?;
    request
        .validate_cache_control()
        .map_err(|error| invalid(&error.to_string()))?;
    let ttl = if policy.ttl_seconds == 3600 {
        "1h"
    } else {
        "5m"
    };
    if !breakpoint(&prepared.request, ttl) {
        return Err(invalid(
            "cache warming requires an existing native breakpoint with matching TTL",
        ));
    }
    // Generation controls have no bearing on the cached prefix. The warm request
    // performs no tool calls and generates at most one output token.
    request.max_tokens = 1;
    request.thinking = None;
    request.output_config = None;
    request.tool_choice = Some(serde_json::json!({"type":"none"}));
    let reserve_step = format!("warm-reserve/{request_id}");
    let input = serde_json::json!({"request":request, "policy":policy});
    let _reservation = match owner
        .begin_step(
            operation.into(),
            reserve_step.clone(),
            "cache_warm_reservation".into(),
            &input,
            crate::ReplaySafety::Safe,
        )
        .await?
    {
        crate::BeginStep::OutcomeUnknown => {
            return Err(invalid("cache warm reservation outcome is unknown"));
        }
        crate::BeginStep::Replay(value) => value.decode::<Budget>()?,
        crate::BeginStep::Execute => {
            let (version, mut budget) = retained_budget(owner, &prepared.state).await?;
            if !budget.reserved_usd.is_finite()
                || budget.reserved_usd < 0.0
                || budget.reserved_usd + policy.estimated_write_usd > policy.max_spend_usd
            {
                return Err(invalid("cache warm spend limit exceeded"));
            }
            budget.reserved_usd += policy.estimated_write_usd;
            // The reservation is session state, not merely a successful model
            // checkpoint. An uncertain charge still constrains later turns.
            owner
                .complete_step_with_documents(
                    operation.into(),
                    reserve_step,
                    &budget,
                    vec![budget_write(version, &budget)?],
                )
                .await?;
            budget
        }
    };
    let effect = format!("cache-warm/{request_id}");
    let receipt = match owner
        .begin_step(
            operation.into(),
            effect.clone(),
            "cache_warm_http".into(),
            &input,
            crate::ReplaySafety::Unsafe,
        )
        .await?
    {
        crate::BeginStep::OutcomeUnknown => {
            return Err(invalid(
                "cache warm charge is uncertain; reconcile before another dispatch",
            ));
        }
        crate::BeginStep::Replay(value) => value.decode::<Receipt>()?,
        crate::BeginStep::Execute => {
            let response = client
                .create(&request)
                .await
                .map_err(|error| invalid(&error.to_string()))?;
            let actual_usd = policy.cost(&response.usage)?;
            let (version, current) = retained_budget(owner, &prepared.state).await?;
            let receipt = Receipt {
                budget: Budget {
                    reserved_usd: current.reserved_usd - policy.estimated_write_usd + actual_usd,
                    actual_usd: current.actual_usd + actual_usd,
                },
                usage: response.usage,
                actual_usd,
            };
            owner
                .complete_step_with_documents(
                    operation.into(),
                    effect,
                    &receipt,
                    vec![budget_write(version, &receipt.budget)?],
                )
                .await?;
            receipt
        }
    };
    prepared.state["cache_warm_budget"] = serde_json::to_value(&receipt.budget)?;
    // Only the latest usage is in the bounded policy checkpoint; complete warm
    // receipts remain in the existing immutable operation journal.
    prepared.state["last_cache_warm"] = serde_json::json!({"request_id":request_id,
        "usage":receipt.usage, "actual_usd":receipt.actual_usd, "ttl_seconds":policy.ttl_seconds});
    Ok(prepared)
}

fn breakpoint(value: &Value, ttl: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.get("cache_control").is_some_and(|control| {
                control["type"] == "ephemeral" && control["ttl"].as_str().unwrap_or("5m") == ttl
            }) || object.values().any(|child| breakpoint(child, ttl))
        }
        Value::Array(values) => values.iter().any(|child| breakpoint(child, ttl)),
        _ => false,
    }
}
