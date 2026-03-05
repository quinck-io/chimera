use super::*;
use rsa::RsaPrivateKey;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_private_key() -> RsaPrivateKey {
    RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap()
}

#[test]
fn jwt_creation_and_validation() {
    let private_key = test_private_key();
    let client_id = "test-client-id";
    let auth_url = "https://token.actions.githubusercontent.com/oauth2/token";

    let token = create_jwt(&private_key, client_id, auth_url).unwrap();

    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);

    let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0])
        .unwrap();
    let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
    assert_eq!(header["alg"], "PS256");
    assert_eq!(header["typ"], "JWT");

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .unwrap();
    let claims: JwtClaims = serde_json::from_slice(&payload).unwrap();

    assert_eq!(claims.sub, client_id);
    assert_eq!(claims.iss, client_id);
    assert_eq!(claims.aud, auth_url);
    assert!(!claims.jti.is_empty());
    assert!(claims.exp > claims.iat);
    assert!(claims.exp - claims.iat <= 300);
}

#[test]
fn jwt_claims_have_correct_expiry() {
    let private_key = test_private_key();

    let token = create_jwt(&private_key, "my-client", "https://example.com/token").unwrap();

    let parts: Vec<&str> = token.split('.').collect();
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .unwrap();
    let claims: JwtClaims = serde_json::from_slice(&payload).unwrap();

    let now = Utc::now().timestamp();
    assert!((claims.iat - (now - 30)).abs() < 5);
    assert!((claims.exp - claims.iat - 300).abs() < 5);
    assert_eq!(claims.nbf, claims.iat);
}

#[test]
fn jwt_signature_verifies_with_rsa_pss() {
    use rsa::pss::VerifyingKey;
    use rsa::signature::Verifier;

    let private_key = test_private_key();
    let token = create_jwt(&private_key, "client-1", "https://example.com/token").unwrap();

    let parts: Vec<&str> = token.split('.').collect();
    let message = format!("{}.{}", parts[0], parts[1]);
    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .unwrap();

    let verifying_key = VerifyingKey::<Sha256>::new(private_key.to_public_key());
    let signature = rsa::pss::Signature::try_from(sig_bytes.as_slice()).unwrap();
    verifying_key
        .verify(message.as_bytes(), &signature)
        .expect("JWT PS256 signature should verify with the public key");
}

#[tokio::test]
async fn token_exchange_success() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "ghs_test_token_123",
            "expires_in": 3600
        })))
        .mount(&mock_server)
        .await;

    let private_key = test_private_key();
    let tm = TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    );

    let token = tm.get_token().await.unwrap();
    assert_eq!(token, "ghs_test_token_123");
}

#[tokio::test]
async fn token_exchange_failure() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .mount(&mock_server)
        .await;

    let private_key = test_private_key();
    let tm = TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    );

    let result = tm.get_token().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn token_caching() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "cached_token",
            "expires_in": 7200
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let private_key = test_private_key();
    let tm = TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    );

    let token1 = tm.get_token().await.unwrap();
    let token2 = tm.get_token().await.unwrap();
    assert_eq!(token1, "cached_token");
    assert_eq!(token2, "cached_token");
}

#[tokio::test]
async fn invalidate_forces_refresh() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new_token",
            "expires_in": 7200
        })))
        .expect(2)
        .mount(&mock_server)
        .await;

    let private_key = test_private_key();
    let tm = TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    );

    tm.get_token().await.unwrap();
    tm.invalidate().await;
    tm.get_token().await.unwrap();
}
