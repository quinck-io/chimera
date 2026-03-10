use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::Deserializer;

#[derive(Debug, Deserialize)]
pub struct ActionMetadata {
    pub name: Option<String>,
    #[serde(default)]
    pub inputs: HashMap<String, ActionInput>,
    pub runs: ActionRuns,
}

#[derive(Debug, Deserialize)]
pub struct ActionInput {
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionRuntime {
    Node(String),
    Composite,
    Docker,
    Unknown(String),
}

impl ActionRuntime {
    pub fn is_node(&self) -> bool {
        matches!(self, Self::Node(_))
    }

    pub fn is_composite(&self) -> bool {
        matches!(self, Self::Composite)
    }

    pub fn is_docker(&self) -> bool {
        matches!(self, Self::Docker)
    }
}

impl std::fmt::Display for ActionRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Node(v) => write!(f, "node{v}"),
            Self::Composite => write!(f, "composite"),
            Self::Docker => write!(f, "docker"),
            Self::Unknown(s) => write!(f, "{s}"),
        }
    }
}

impl<'de> Deserialize<'de> for ActionRuntime {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if let Some(version) = s.strip_prefix("node") {
            Ok(Self::Node(version.to_string()))
        } else if s == "composite" {
            Ok(Self::Composite)
        } else if s == "docker" {
            Ok(Self::Docker)
        } else {
            Ok(Self::Unknown(s))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ActionRuns {
    pub using: ActionRuntime,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub pre: Option<String>,
    #[serde(default)]
    pub post: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<serde_yaml::Value>>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_seq")]
    pub args: Option<Vec<String>>,
    #[serde(default, rename = "pre-entrypoint")]
    pub pre_entrypoint: Option<String>,
    #[serde(default, rename = "post-entrypoint")]
    pub post_entrypoint: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

fn deserialize_string_or_seq<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    struct StringOrSeq;
    impl<'de> de::Visitor<'de> for StringOrSeq {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or sequence of strings")
        }

        fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
            Ok(Some(v.split_whitespace().map(|s| s.to_string()).collect()))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut v = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                v.push(s);
            }
            Ok(Some(v))
        }
    }

    deserializer.deserialize_any(StringOrSeq)
}

impl ActionRuns {
    pub fn is_node(&self) -> bool {
        self.using.is_node()
    }

    pub fn is_composite(&self) -> bool {
        self.using.is_composite()
    }

    pub fn is_docker(&self) -> bool {
        self.using.is_docker()
    }
}

pub fn load_action_metadata(action_dir: &Path) -> Result<ActionMetadata> {
    let yml_path = action_dir.join("action.yml");
    let yaml_path = action_dir.join("action.yaml");

    let content = if yml_path.exists() {
        std::fs::read_to_string(&yml_path)
            .with_context(|| format!("reading {}", yml_path.display()))?
    } else if yaml_path.exists() {
        std::fs::read_to_string(&yaml_path)
            .with_context(|| format!("reading {}", yaml_path.display()))?
    } else {
        anyhow::bail!(
            "no action.yml or action.yaml found in {}",
            action_dir.display()
        );
    };

    serde_yaml::from_str(&content).context("parsing action metadata")
}

#[cfg(test)]
#[path = "metadata_test.rs"]
mod metadata_test;
