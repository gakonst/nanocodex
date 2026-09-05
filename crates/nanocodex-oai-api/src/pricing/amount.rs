use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

const NANO_USD_PER_USD: u64 = 1_000_000_000;

/// An exact non-negative amount of United States dollars.
///
/// The value is stored as billionths of one dollar and serialized as a decimal
/// string, so language bindings and durable eval artifacts never round through
/// a JSON floating-point number.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct UsdAmount {
    nano_usd: u64,
}

impl UsdAmount {
    /// Creates an amount from billionths of one dollar.
    #[must_use]
    pub const fn from_nano_usd(nano_usd: u64) -> Self {
        Self { nano_usd }
    }

    /// Returns the exact amount in billionths of one dollar.
    #[must_use]
    pub const fn nano_usd(self) -> u64 {
        self.nano_usd
    }

    /// Returns a decimal USD string without a currency symbol.
    #[must_use]
    pub fn decimal(self) -> String {
        format_decimal_usd(self.nano_usd)
    }

    /// Returns a floating-point projection for compatibility adapters.
    ///
    /// Use [`Self::nano_usd`] or [`Self::decimal`] for accounting and durable
    /// storage.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_f64(self) -> f64 {
        self.nano_usd as f64 / NANO_USD_PER_USD as f64
    }

    pub(super) const fn saturating_add(self, other: Self) -> Self {
        Self {
            nano_usd: self.nano_usd.saturating_add(other.nano_usd),
        }
    }

    pub(super) const fn saturating_mul(tokens: u64, nano_usd_per_token: u64) -> Self {
        Self {
            nano_usd: tokens.saturating_mul(nano_usd_per_token),
        }
    }
}

impl fmt::Display for UsdAmount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "${}", self.decimal())
    }
}

impl Serialize for UsdAmount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.decimal())
    }
}

impl<'de> Deserialize<'de> for UsdAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_decimal_usd(&value)
            .map(Self::from_nano_usd)
            .map_err(de::Error::custom)
    }
}

fn parse_decimal_usd(value: &str) -> Result<u64, &'static str> {
    if value.is_empty() {
        return Err("USD amount must not be empty");
    }
    let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("expected a non-negative decimal USD amount");
    }
    if fractional.len() > 9 {
        return Err("USD amount has more than nine decimal places");
    }
    let fractional_digits =
        u32::try_from(fractional.len()).map_err(|_| "USD amount is too large")?;
    let whole = whole
        .parse::<u64>()
        .map_err(|_| "USD amount is too large")?;
    let fractional = if fractional.is_empty() {
        0
    } else {
        fractional
            .parse::<u64>()
            .map_err(|_| "USD amount is too large")?
            .checked_mul(10_u64.pow(9_u32.saturating_sub(fractional_digits)))
            .ok_or("USD amount is too large")?
    };
    whole
        .checked_mul(NANO_USD_PER_USD)
        .and_then(|whole| whole.checked_add(fractional))
        .ok_or("USD amount is too large")
}

fn format_decimal_usd(nano_usd: u64) -> String {
    let whole = nano_usd / NANO_USD_PER_USD;
    let fractional = nano_usd % NANO_USD_PER_USD;
    if fractional == 0 {
        return whole.to_string();
    }
    let fractional = format!("{fractional:09}");
    format!("{whole}.{}", fractional.trim_end_matches('0'))
}

#[cfg(test)]
mod tests {
    use super::UsdAmount;

    #[test]
    fn exact_amount_has_a_canonical_decimal_wire_shape() {
        let amount = UsdAmount::from_nano_usd(1_250_000_001);
        assert_eq!(amount.decimal(), "1.250000001");
        assert_eq!(amount.to_string(), "$1.250000001");
        assert_eq!(
            serde_json::from_str::<UsdAmount>("\"1.250000001\"").unwrap(),
            amount
        );
        assert_eq!(serde_json::to_string(&amount).unwrap(), "\"1.250000001\"");
    }

    #[test]
    fn deserialization_rejects_lossy_or_negative_amounts() {
        assert!(serde_json::from_str::<UsdAmount>("1.25").is_err());
        assert!(serde_json::from_str::<UsdAmount>("\"-1\"").is_err());
        assert!(serde_json::from_str::<UsdAmount>("\"1.0000000001\"").is_err());
    }
}
