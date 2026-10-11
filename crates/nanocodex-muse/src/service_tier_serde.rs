use std::fmt;

use nanocodex_oai_api::pricing::ServiceTier;
use serde::{
    Deserialize, Deserializer, Serializer,
    de::{Error, IgnoredAny, IntoDeserializer, MapAccess, Visitor},
    ser::SerializeMap,
};

pub(crate) fn serialize<S: Serializer>(
    tier: &ServiceTier,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(1))?;
    map.serialize_entry("service_tier", tier)?;
    map.end()
}

// Pending compaction admission compares exact input bytes across process restarts.
// Standard and Fast identities encode the preference as a boolean.
#[cfg(feature = "openai")]
pub(crate) fn serialize_admission<S: Serializer>(
    tier: &ServiceTier,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(1))?;
    match tier {
        ServiceTier::Ultrafast => map.serialize_entry("service_tier", tier)?,
        _ => map.serialize_entry("fast_mode", &!matches!(tier, ServiceTier::Standard))?,
    }
    map.end()
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<ServiceTier, D::Error> {
    #[derive(Deserialize)]
    #[serde(field_identifier, rename_all = "snake_case")]
    enum Field {
        ServiceTier,
        FastMode,
        #[serde(other)]
        Ignore,
    }

    struct TierVisitor;

    impl<'de> Visitor<'de> for TierVisitor {
        type Value = ServiceTier;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("one service_tier or legacy fast_mode field")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut tier = None;
            while let Some(field) = map.next_key()? {
                let next = match field {
                    Field::ServiceTier => {
                        let value = map.next_value::<String>()?;
                        ServiceTier::deserialize(value.into_deserializer())?
                    }
                    Field::FastMode => ServiceTier::from_fast_mode(map.next_value::<bool>()?),
                    Field::Ignore => {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                };
                if tier.replace(next).is_some() {
                    return Err(M::Error::custom(
                        "duplicate or conflicting service-tier fields",
                    ));
                }
            }
            tier.ok_or_else(|| M::Error::missing_field("service_tier"))
        }
    }

    deserializer.deserialize_map(TierVisitor)
}
