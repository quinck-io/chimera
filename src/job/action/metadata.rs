use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

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

#[derive(Debug, Deserialize)]
pub struct ActionRuns {
    pub using: String,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub pre: Option<String>,
    #[serde(default)]
    pub post: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<serde_yaml::Value>>,
}

impl ActionRuns {
    pub fn is_node(&self) -> bool {
        self.using.starts_with("node")
    }

    pub fn is_composite(&self) -> bool {
        self.using == "composite"
    }

    pub fn is_docker(&self) -> bool {
        self.using == "docker"
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
