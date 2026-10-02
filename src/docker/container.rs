use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct JobContainerSpec {
    pub image: String,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    pub options: Option<String>,
    pub credentials: Option<ContainerCredentials>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceContainerSpec {
    pub image: String,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub environment: HashMap<String, String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    pub options: Option<String>,
    pub credentials: Option<ContainerCredentials>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContainerCredentials {
    pub username: Option<String>,
    pub password: Option<String>,
}

impl JobContainerSpec {
    pub fn resolved(&self, resolve: &impl Fn(&str) -> String) -> Self {
        Self {
            image: resolve(&self.image),
            environment: resolve_map(&self.environment, resolve),
            ports: resolve_all(&self.ports, resolve),
            volumes: resolve_all(&self.volumes, resolve),
            options: self.options.as_deref().map(resolve),
            credentials: self.credentials.as_ref().map(|c| c.resolved(resolve)),
        }
    }
}

impl ServiceContainerSpec {
    pub fn resolved(&self, resolve: &impl Fn(&str) -> String) -> Self {
        Self {
            image: resolve(&self.image),
            ports: resolve_all(&self.ports, resolve),
            environment: resolve_map(&self.environment, resolve),
            volumes: resolve_all(&self.volumes, resolve),
            options: self.options.as_deref().map(resolve),
            credentials: self.credentials.as_ref().map(|c| c.resolved(resolve)),
            alias: self.alias.clone(),
        }
    }
}

impl ContainerCredentials {
    fn resolved(&self, resolve: &impl Fn(&str) -> String) -> Self {
        Self {
            username: self.username.as_deref().map(resolve),
            password: self.password.as_deref().map(resolve),
        }
    }
}

fn resolve_all(values: &[String], resolve: &impl Fn(&str) -> String) -> Vec<String> {
    values.iter().map(|v| resolve(v)).collect()
}

fn resolve_map(
    values: &HashMap<String, String>,
    resolve: &impl Fn(&str) -> String,
) -> HashMap<String, String> {
    values
        .iter()
        .map(|(k, v)| (k.clone(), resolve(v)))
        .collect()
}

#[cfg(test)]
#[path = "container_test.rs"]
mod container_test;
