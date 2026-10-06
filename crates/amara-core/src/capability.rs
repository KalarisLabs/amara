//! Capability types for agent discovery and advertisement.

use serde::{Deserialize, Serialize};

/// Standard capability categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityCategory {
    Reasoning,
    Coding,
    Vision,
    Audio,
    Speech,
    Ocr,
    Search,
    Embeddings,
    Reranking,
    Simulation,
    Robotics,
    Sensors,
    Database,
    Filesystem,
    Web,
    ComputerControl,
    Custom,
}

impl CapabilityCategory {
    pub fn from_id(id: u8) -> Self {
        match id {
            0 => Self::Reasoning,
            1 => Self::Coding,
            2 => Self::Vision,
            3 => Self::Audio,
            4 => Self::Speech,
            5 => Self::Ocr,
            6 => Self::Search,
            7 => Self::Embeddings,
            8 => Self::Reranking,
            9 => Self::Simulation,
            10 => Self::Robotics,
            11 => Self::Sensors,
            12 => Self::Database,
            13 => Self::Filesystem,
            14 => Self::Web,
            15 => Self::ComputerControl,
            _ => Self::Custom,
        }
    }

    pub fn to_id(&self) -> u8 {
        match self {
            Self::Reasoning => 0,
            Self::Coding => 1,
            Self::Vision => 2,
            Self::Audio => 3,
            Self::Speech => 4,
            Self::Ocr => 5,
            Self::Search => 6,
            Self::Embeddings => 7,
            Self::Reranking => 8,
            Self::Simulation => 9,
            Self::Robotics => 10,
            Self::Sensors => 11,
            Self::Database => 12,
            Self::Filesystem => 13,
            Self::Web => 14,
            Self::ComputerControl => 15,
            Self::Custom => 255,
        }
    }
}

/// A single capability exposed by an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    /// Canonical name (e.g., "vision", "ocr", "image-classification").
    pub name: String,
    /// Category for quick filtering.
    pub category: CapabilityCategory,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Model information advertised by an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model name (e.g., "llama-3.2", "qwen-vl").
    pub name: String,
    /// Input modalities (e.g., ["text", "image"]).
    #[serde(default)]
    pub modalities: Vec<String>,
    /// Optional provider (e.g., "ollama", "mlx").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// Resource limits advertised by an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum input size in bytes.
    #[serde(default)]
    pub max_input_bytes: u64,
    /// Maximum context window (tokens).
    #[serde(default)]
    pub max_context: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 8 * 1024 * 1024, // 8 MiB
            max_context: 32768,
        }
    }
}

/// The compact capability document advertised over the mesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDocument {
    /// Agent display name.
    pub name: String,
    /// Node kind (usually "agent").
    #[serde(default = "default_kind")]
    pub kind: String,
    /// List of capability names.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Models available on this agent.
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    /// Tools this agent exposes.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Resource limits.
    #[serde(default)]
    pub limits: ResourceLimits,
    /// Current availability status.
    #[serde(default)]
    pub availability: Availability,
}

fn default_kind() -> String {
    "agent".to_string()
}

/// Coarse load classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LoadClass {
    #[default]
    Idle,
    Low,
    Medium,
    High,
    Overloaded,
}

impl LoadClass {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Idle,
            1 => Self::Low,
            2 => Self::Medium,
            3 => Self::High,
            4 => Self::Overloaded,
            _ => Self::Idle,
        }
    }

    pub fn to_u8(&self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Overloaded => 4,
        }
    }
}

/// Agent availability status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
    Offline,
    #[default]
    Online,
    Busy,
    Away,
}

impl Availability {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Offline,
            1 => Self::Online,
            2 => Self::Busy,
            3 => Self::Away,
            _ => Self::Offline,
        }
    }

    pub fn to_u8(&self) -> u8 {
        match self {
            Self::Offline => 0,
            Self::Online => 1,
            Self::Busy => 2,
            Self::Away => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_category_roundtrip() {
        for id in 0..=16 {
            let cat = CapabilityCategory::from_id(id);
            if id <= 15 {
                assert_eq!(cat.to_id(), id);
            }
        }
    }

    #[test]
    fn capability_document_serialization() {
        let doc = CapabilityDocument {
            name: "orion".to_string(),
            kind: "agent".to_string(),
            capabilities: vec!["vision".to_string(), "ocr".to_string()],
            models: vec![ModelInfo {
                name: "qwen-vl".to_string(),
                modalities: vec!["image".to_string(), "text".to_string()],
                provider: Some("ollama".to_string()),
            }],
            tools: vec!["ocr".to_string()],
            limits: ResourceLimits::default(),
            availability: Availability::Online,
        };

        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.len() < 4096, "Capability doc must fit in 4KiB limit");

        let parsed: CapabilityDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "orion");
        assert_eq!(parsed.capabilities.len(), 2);
    }
}
