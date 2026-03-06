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

#[test]
fn credentials_save_load_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let runners_dir = tmp.path().join("runners");

    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let params = private_key_to_rsa_params(&key).unwrap();

    let creds = RunnerCredentials {
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
    };

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
        daemon: Some(DaemonConfig {
            log_format: "json".into(),
        }),
        runners: vec!["runner-0".into(), "runner-1".into()],
    };

    save_config(&config_path, &config).unwrap();
    let loaded = load_config(&config_path).unwrap();

    assert_eq!(loaded.runners.len(), 2);
    assert_eq!(loaded.runners[0], "runner-0");
    assert_eq!(loaded.daemon.unwrap().log_format, "json");
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
