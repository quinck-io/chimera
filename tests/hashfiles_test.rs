mod common;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn returns_hex_and_is_deterministic() {
    let env = TestEnv::setup().await;
    std::fs::write(
        env.workspace.workspace_dir().join("test.txt"),
        "hello world",
    )
    .unwrap();

    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            HASH1="${{ hashFiles('test.txt') }}"
            HASH2="${{ hashFiles('test.txt') }}"
            echo "HASH1=$HASH1"
            test -n "$HASH1" || exit 1
            echo "$HASH1" | grep -qE '^[0-9a-f]{64}$' || exit 1
            test "$HASH1" = "$HASH2" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn different_patterns_differ() {
    let env = TestEnv::setup().await;
    let ws_dir = env.workspace.workspace_dir();
    std::fs::write(ws_dir.join("a.txt"), "file a").unwrap();
    std::fs::write(ws_dir.join("b.txt"), "file b").unwrap();

    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            HASH_A="${{ hashFiles('a.txt') }}"
            HASH_AB="${{ hashFiles('a.txt', 'b.txt') }}"
            test "$HASH_A" != "$HASH_AB" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn no_match_returns_empty() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"test -z "${{ hashFiles('*.nonexistent') }}" || exit 1"#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn recursive_glob() {
    let env = TestEnv::setup().await;
    let sub_dir = env.workspace.workspace_dir().join("src");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(sub_dir.join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(sub_dir.join("lib.rs"), "pub fn hello() {}").unwrap();

    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            HASH="${{ hashFiles('**/*.rs') }}"
            test -n "$HASH" || exit 1
            echo "$HASH" | grep -qE '^[0-9a-f]{64}$' || exit 1
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn in_cache_key_expression() {
    let env = TestEnv::setup().await;
    std::fs::write(env.workspace.workspace_dir().join("Cargo.lock"), "[lock]").unwrap();

    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            KEY="cargo-${{ hashFiles('Cargo.lock') }}"
            test -n "$KEY" || exit 1
            echo "$KEY" | grep -q "cargo-" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
