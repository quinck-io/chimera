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

#[cfg(test)]
#[path = "container_test.rs"]
mod container_test;
