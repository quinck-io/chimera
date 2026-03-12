use serde::{Deserialize, Serialize};

fn default_max_gb() -> u64 {
    10
}

fn default_cache_port() -> u16 {
    9999
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheConfig {
    #[serde(default = "default_max_gb")]
    pub max_gb: u64,
    #[serde(default = "default_cache_port")]
    pub cache_port: u16,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_gb: default_max_gb(),
            cache_port: default_cache_port(),
        }
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod config_test;
