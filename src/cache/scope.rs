use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::rand_core::{OsRng, RngCore};

/// The repository and refs a job may read from and write to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheScope {
    pub repo: String,
    pub git_ref: String,
    /// Refs the job may also restore from, in order: for a pull request its base branch,
    /// then the default branch. Writes always go to `git_ref`.
    pub fallback_refs: Vec<String>,
}

impl CacheScope {
    /// Refs a lookup searches, own ref first, without duplicates.
    pub fn readable_refs(&self) -> Vec<&str> {
        let mut refs = vec![self.git_ref.as_str()];
        for fallback in &self.fallback_refs {
            if !refs.contains(&fallback.as_str()) {
                refs.push(fallback);
            }
        }
        refs
    }
}

/// Maps per-job access tokens to the scope the runner assigned to that job.
///
/// The scope is decided by the runner from the job manifest, never by the client:
/// the cache server is reachable by every job (and by anything on the network), so
/// a client-supplied scope would let any job read or poison any repository's cache.
#[derive(Default)]
pub struct ScopeRegistry {
    grants: Mutex<HashMap<String, CacheScope>>,
}

impl ScopeRegistry {
    /// Issues a new token for `scope`. Access is revoked when the returned grant is dropped.
    pub fn grant(self: &Arc<Self>, scope: CacheScope) -> CacheGrant {
        let token = random_token();
        self.lock().insert(token.clone(), scope);
        CacheGrant {
            token,
            registry: Arc::clone(self),
        }
    }

    pub fn resolve(&self, token: &str) -> Option<CacheScope> {
        self.lock().get(token).cloned()
    }

    fn revoke(&self, token: &str) {
        self.lock().remove(token);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, CacheScope>> {
        // The map is always left consistent, so a panic elsewhere must not lock everyone out.
        self.grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A job's access to the cache. Dropping it (job finished, failed or aborted) revokes the token.
pub struct CacheGrant {
    token: String,
    registry: Arc<ScopeRegistry>,
}

impl CacheGrant {
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for CacheGrant {
    fn drop(&mut self) {
        self.registry.revoke(&self.token);
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
#[path = "scope_test.rs"]
mod scope_test;
