#![allow(missing_docs)]

use std::fmt::{Display, Formatter};
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::MuAiError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    OpenAiCompatible,
    Anthropic,
}

impl Display for ProviderId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::OpenAiCompatible => "openai-compatible",
            Self::Anthropic => "anthropic",
        };
        write!(f, "{value}")
    }
}

impl FromStr for ProviderId {
    type Err = MuAiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "openai-compatible" | "openai" => Ok(Self::OpenAiCompatible),
            "anthropic" => Ok(Self::Anthropic),
            _ => Err(MuAiError::InvalidRequest(format!(
                "unknown provider {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ModelId(pub String);

impl Display for ModelId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelSpec {
    pub provider: ProviderId,
    pub id: ModelId,
    pub display_name: String,
    pub supports_tools: bool,
    pub context_window: u32,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    pub input_cost_per_million_tokens: Option<f64>,
    pub output_cost_per_million_tokens: Option<f64>,
}

impl ModelSpec {
    pub fn new(
        provider: ProviderId,
        id: impl Into<ModelId>,
        display_name: impl Into<String>,
        context_window: u32,
        max_output_tokens: u32,
    ) -> Self {
        Self {
            provider,
            id: id.into(),
            display_name: display_name.into(),
            supports_tools: true,
            context_window,
            max_output_tokens,
            input_cost_per_million_tokens: None,
            output_cost_per_million_tokens: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModelRegistry {
    builtin: Vec<ModelSpec>,
    custom: Vec<ModelSpec>,
}

impl ModelRegistry {
    pub fn new(custom: Vec<ModelSpec>) -> Self {
        Self {
            builtin: builtin_models(),
            custom,
        }
    }

    pub fn list(&self) -> Vec<ModelSpec> {
        self.builtin
            .iter()
            .chain(self.custom.iter())
            .cloned()
            .collect()
    }

    pub fn default_for(&self, provider: &ProviderId) -> Option<ModelSpec> {
        self.list()
            .into_iter()
            .find(|model| &model.provider == provider)
    }

    pub fn find(&self, provider: &ProviderId, id: &str) -> Option<ModelSpec> {
        self.list()
            .into_iter()
            .find(|model| &model.provider == provider && model.id.0 == id)
    }
}

pub fn load_custom_models(path: &Path) -> Result<Vec<ModelSpec>, MuAiError> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw = std::fs::read_to_string(path).map_err(|error| {
        MuAiError::InvalidRequest(format!("failed to read {}: {error}", path.display()))
    })?;
    let file: CustomModelsFile = toml::from_str(&raw).map_err(|error| {
        MuAiError::InvalidRequest(format!("failed to parse {}: {error}", path.display()))
    })?;
    Ok(file.models)
}

#[derive(Debug, Deserialize)]
struct CustomModelsFile {
    #[serde(default)]
    models: Vec<ModelSpec>,
}

fn default_max_output_tokens() -> u32 {
    16_384
}

fn builtin_models() -> Vec<ModelSpec> {
    vec![
        ModelSpec::new(
            ProviderId::OpenAiCompatible,
            "gpt-5.4",
            "GPT-5.4",
            1_000_000,
            100_000,
        ),
        ModelSpec::new(
            ProviderId::OpenAiCompatible,
            "gpt-4o-mini",
            "GPT-4o mini",
            128_000,
            16_384,
        ),
        ModelSpec::new(
            ProviderId::OpenAiCompatible,
            "o3-mini",
            "o3-mini",
            200_000,
            100_000,
        ),
        ModelSpec::new(
            ProviderId::Anthropic,
            "claude-3-5-sonnet-latest",
            "Claude Sonnet",
            200_000,
            8_192,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::{ModelRegistry, ProviderId};

    #[test]
    fn builtin_registry_includes_gpt_5_4() {
        let registry = ModelRegistry::new(Vec::new());
        let model = registry.find(&ProviderId::OpenAiCompatible, "gpt-5.4");
        assert!(matches!(
            model,
            Some(model) if model.display_name == "GPT-5.4" && model.context_window == 1_000_000
        ));
    }
}
