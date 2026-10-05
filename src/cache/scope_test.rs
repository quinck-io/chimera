use std::sync::Arc;

use super::*;

fn main_scope() -> CacheScope {
    CacheScope {
        repo: "owner/repo".into(),
        git_ref: "refs/heads/main".into(),
        default_ref: "refs/heads/main".into(),
    }
}

#[test]
fn granted_token_resolves_to_scope() {
    let registry = Arc::new(ScopeRegistry::default());

    let grant = registry.grant(main_scope());

    assert_eq!(registry.resolve(grant.token()), Some(main_scope()));
}

#[test]
fn unknown_token_does_not_resolve() {
    let registry = Arc::new(ScopeRegistry::default());
    let _grant = registry.grant(main_scope());

    assert_eq!(registry.resolve("guessed-token"), None);
}

#[test]
fn dropping_grant_revokes_token() {
    let registry = Arc::new(ScopeRegistry::default());
    let grant = registry.grant(main_scope());
    let token = grant.token().to_string();

    drop(grant);

    assert_eq!(registry.resolve(&token), None);
}

#[test]
fn tokens_are_unique_and_unguessable() {
    let registry = Arc::new(ScopeRegistry::default());

    let first = registry.grant(main_scope());
    let second = registry.grant(main_scope());

    assert_ne!(first.token(), second.token());
    // 32 random bytes, base64url without padding
    assert_eq!(first.token().len(), 43);
}
