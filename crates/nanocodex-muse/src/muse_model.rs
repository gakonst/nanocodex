//! Convenience selectors for the shared Muse model identifiers.
use nanocodex_oai_api::Model;
use serde::{Deserialize, Serialize};
/// Muse Spark subscription variant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(into = "Model", try_from = "Model")]
pub enum MuseModel {
    /// Standard Spark 1.3.
    #[default]
    Spark,
    /// Subsidized Spark: Meta may train on prompts and completions.
    Contributor,
}
impl MuseModel {
    /// Provider-native model ID.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spark => Model::MuseSpark13.as_str(),
            Self::Contributor => Model::MuseSpark13Contributor.as_str(),
        }
    }
}
impl From<MuseModel> for Model {
    fn from(model: MuseModel) -> Self {
        match model {
            MuseModel::Spark => Self::MuseSpark13,
            MuseModel::Contributor => Self::MuseSpark13Contributor,
        }
    }
}
impl TryFrom<Model> for MuseModel {
    type Error = String;
    fn try_from(model: Model) -> Result<Self, Self::Error> {
        match model {
            Model::MuseSpark13 => Ok(Self::Spark),
            Model::MuseSpark13Contributor => Ok(Self::Contributor),
            other => Err(invalid_model(other.as_str())),
        }
    }
}

fn invalid_model(value: &str) -> String {
    format!("invalid Muse model {value:?}; expected muse-spark-1.3 or muse-spark-1.3-contributor")
}

impl std::fmt::Display for MuseModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl std::str::FromStr for MuseModel {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<Model>()
            .map_err(|_| invalid_model(value))
            .and_then(Self::try_from)
    }
}
