use super::*;
use tempfile::TempDir;

#[test]
fn rsa_key_roundtrip() {
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    let params = private_key_to_rsa_params(&key).unwrap();
    let reconstructed = rsa_params_to_private_key(&params).unwrap();

    assert_eq!(key.n(), reconstructed.n());
    assert_eq!(key.e(), reconstructed.e());
    assert_eq!(key.d(), reconstructed.d());
}

fn make_credentials(key: &RsaPrivateKey) -> RunnerCredentials {
    let params = private_key_to_rsa_params(key).unwrap();

    RunnerCredentials {
        info: RunnerInfo {
            agent_id: 42,
            agent_name: "test-runner".into(),
            pool_id: 1,
            server_url: "https://pipelines.actions.githubusercontent.com/abc/".into(),
            server_url_v2: "https://broker.actions.githubusercontent.com".into(),
            git_hub_url: "https://github.com/org/repo".into(),
            work_folder: "_work".into(),
            use_v2_flow: true,
        },
        oauth: OAuthCredentials {
            scheme: "OAuth".into(),
            client_id: "client-id-123".into(),
            authorization_url: "https://vstoken.actions.githubusercontent.com/abc".into(),
        },
        rsa_params: params,
    }
}

#[test]
fn credentials_save_load_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let creds = make_credentials(&key);

    save_runner_credentials(&runners_dir, "test-runner", &creds).unwrap();
    let loaded = load_runner_credentials(&runners_dir, "test-runner").unwrap();

    assert_eq!(loaded.info.agent_id, 42);
    assert_eq!(loaded.info.agent_name, "test-runner");
    assert_eq!(loaded.oauth.client_id, "client-id-123");

    // Verify RSA key survives roundtrip through files
    let loaded_key = rsa_params_to_private_key(&loaded.rsa_params).unwrap();
    assert_eq!(key.n(), loaded_key.n());
}

#[test]
fn config_load_save_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let config = ChimeraConfig {
        daemon: DaemonConfig {
            log_format: "json".into(),
            shutdown_timeout_secs: 300,
        },
        runners: vec!["runner-0".into(), "runner-1".into()],
        ..Default::default()
    };

    save_config(&config_path, &config).unwrap();
    let loaded = load_config(&config_path).unwrap();

    assert_eq!(loaded.runners.len(), 2);
    assert_eq!(loaded.runners[0], "runner-0");
    assert_eq!(loaded.daemon.log_format, "json");
}

#[test]
fn load_config_creates_default_file_when_missing() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    assert!(!config_path.exists());
    let config = load_config(&config_path).unwrap();
    assert!(config_path.exists());

    assert!(config.runners.is_empty());
    assert_eq!(config.daemon.log_format, "text");
    assert_eq!(config.daemon.shutdown_timeout_secs, 300);
    assert_eq!(config.cache.max_gb, 10);
    assert_eq!(config.cache.cache_port, 9999);

    // Verify the written file contains all sections
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("[daemon]"));
    assert!(contents.contains("[cache]"));
    assert!(contents.contains("log_format"));
    assert!(contents.contains("shutdown_timeout_secs"));
    assert!(contents.contains("max_gb"));
    assert!(contents.contains("cache_port"));
}

#[test]
fn path_construction() {
    let paths = ChimeraPaths::new(PathBuf::from("/home/user/.chimera"));
    assert_eq!(
        paths.config_file(),
        PathBuf::from("/home/user/.chimera/config.toml")
    );
    assert_eq!(
        paths.runners_dir(),
        PathBuf::from("/home/user/.chimera/runners")
    );
    assert_eq!(
        paths.runner_dir("r0"),
        PathBuf::from("/home/user/.chimera/runners/r0")
    );
    assert_eq!(paths.work_dir(), PathBuf::from("/home/user/.chimera/work"));
    assert_eq!(
        paths.tool_cache_dir(),
        PathBuf::from("/home/user/.chimera/tool-cache")
    );
    assert_eq!(
        paths.pid_file(),
        PathBuf::from("/home/user/.chimera/chimera.pid")
    );
    assert_eq!(
        paths.state_file(),
        PathBuf::from("/home/user/.chimera/state.json")
    );
}

#[test]
fn missing_credentials_file_errors() {
    let tmp = TempDir::new().unwrap();
    let result = load_runner_credentials(tmp.path(), "nonexistent");
    assert!(result.is_err());
}

#[test]
fn public_key_xml_format() {
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let xml = public_key_to_xml(&key);

    assert!(xml.starts_with("<RSAKeyValue>"));
    assert!(xml.ends_with("</RSAKeyValue>"));
    assert!(xml.contains("<Modulus>"));
    assert!(xml.contains("<Exponent>"));
}

#[test]
fn jwt_signing_survives_key_roundtrip() {
    use crate::github::auth::create_jwt;
    use rsa::pss::{Signature, VerifyingKey};
    use rsa::signature::Verifier;
    use sha2::Sha256;

    // Generate key, save params, reconstruct (same as register -> start flow)
    let original_key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let params = private_key_to_rsa_params(&original_key).unwrap();
    let reconstructed = rsa_params_to_private_key(&params).unwrap();

    // Sign JWT with reconstructed key
    let token = create_jwt(&reconstructed, "test-client", "https://example.com/token").unwrap();

    // Verify with original public key
    let parts: Vec<&str> = token.split('.').collect();
    let message = format!("{}.{}", parts[0], parts[1]);
    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .unwrap();

    let verifying_key = VerifyingKey::<Sha256>::new(original_key.to_public_key());
    let signature = Signature::try_from(sig_bytes.as_slice()).unwrap();
    verifying_key
        .verify(message.as_bytes(), &signature)
        .expect("JWT signed with roundtripped key should verify with original public key");
}

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn credentials_are_saved_owner_only() {
    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    save_runner_credentials(&runners_dir, "test-runner", &make_credentials(&key)).unwrap();

    let dir = runners_dir.join("test-runner");
    assert_eq!(mode_of(&dir), 0o700);
    for file in CREDENTIAL_FILES {
        assert_eq!(mode_of(&dir.join(file)), 0o600, "{file}");
    }
}

#[cfg(unix)]
#[test]
fn saving_credentials_leaves_no_temp_files() {
    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    save_runner_credentials(&runners_dir, "test-runner", &make_credentials(&key)).unwrap();

    let mut entries: Vec<String> = std::fs::read_dir(runners_dir.join("test-runner"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    entries.sort();
    let mut expected = CREDENTIAL_FILES.map(String::from).to_vec();
    expected.sort();
    assert_eq!(entries, expected);
}

#[cfg(unix)]
#[test]
fn fresh_runner_directory_is_not_too_open() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("runners").join("test-runner");

    create_private_dir(&dir).unwrap();

    assert!(!restrict_to_owner(&dir, PRIVATE_DIR_MODE).unwrap());
    assert_eq!(mode_of(&dir), 0o700);
}

#[cfg(unix)]
#[test]
fn freshly_saved_credentials_are_not_too_open() {
    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    save_runner_credentials(&runners_dir, "test-runner", &make_credentials(&key)).unwrap();

    let dir = runners_dir.join("test-runner");
    assert!(!restrict_to_owner(&dir, PRIVATE_DIR_MODE).unwrap());
    for file in CREDENTIAL_FILES {
        assert!(
            !restrict_to_owner(&dir.join(file), PRIVATE_FILE_MODE).unwrap(),
            "{file}"
        );
    }
}

#[cfg(unix)]
#[test]
fn restricting_keeps_permissions_narrower_than_allowed() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("rsa_params.json");
    std::fs::write(&file, "{}").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();

    let changed = restrict_to_owner(&file, PRIVATE_FILE_MODE).unwrap();

    assert!(!changed);
    assert_eq!(mode_of(&file), 0o400);
}

#[cfg(unix)]
#[test]
fn saving_replaces_stale_temp_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");
    let dir = runners_dir.join("test-runner");
    create_private_dir(&dir).unwrap();
    let stale = dir.join("rsa_params.json.tmp");
    std::fs::write(&stale, "garbage").unwrap();
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    save_runner_credentials(&runners_dir, "test-runner", &make_credentials(&key)).unwrap();

    assert!(!stale.exists());
    assert_eq!(mode_of(&dir.join(RSA_PARAMS_FILE)), 0o600);
    let loaded = load_runner_credentials(&runners_dir, "test-runner").unwrap();
    assert_eq!(loaded.rsa_params.d, make_credentials(&key).rsa_params.d);
}

#[cfg(unix)]
#[test]
fn overwriting_world_readable_credentials_restricts_them() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");
    let dir = runners_dir.join("test-runner");
    std::fs::create_dir_all(&dir).unwrap();
    let key_file = dir.join(RSA_PARAMS_FILE);
    std::fs::write(&key_file, "{}").unwrap();
    std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    save_runner_credentials(&runners_dir, "test-runner", &make_credentials(&key)).unwrap();

    assert_eq!(mode_of(&key_file), 0o600);
}

#[cfg(unix)]
#[test]
fn loading_credentials_restricts_legacy_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");
    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    save_runner_credentials(&runners_dir, "test-runner", &make_credentials(&key)).unwrap();
    let dir = runners_dir.join("test-runner");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    for file in CREDENTIAL_FILES {
        std::fs::set_permissions(dir.join(file), std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    load_runner_credentials(&runners_dir, "test-runner").unwrap();

    assert_eq!(mode_of(&dir), 0o700);
    for file in CREDENTIAL_FILES {
        assert_eq!(mode_of(&dir.join(file)), 0o600, "{file}");
    }
}
