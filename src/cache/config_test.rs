use super::*;

#[test]
fn defaults() {
    let config = CacheConfig::default();
    assert_eq!(config.max_gb, 10);
    assert_eq!(config.cache_port, 9999);
}

#[test]
fn deserialize_partial() {
    let toml_str = r#"max_gb = 10"#;
    let config: CacheConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.max_gb, 10);
    assert_eq!(config.cache_port, 9999);
}

#[test]
fn deserialize_full() {
    let toml_str = r#"
        max_gb = 100
        cache_port = 8888
    "#;
    let config: CacheConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.max_gb, 100);
    assert_eq!(config.cache_port, 8888);
}
