use std::sync::{Arc, RwLock};

const MASK: &str = "***";

/// The set of secret values hidden from every log line of a job.
///
/// Cloning shares the same set, so a value registered by `::add-mask::` in one step
/// is masked in every later line of the job.
#[derive(Clone, Default)]
pub struct SecretMasker {
    /// Kept longest first, so a secret that contains another is replaced whole
    /// rather than leaving its tail visible.
    values: Arc<RwLock<Vec<String>>>,
}

impl SecretMasker {
    pub fn new<S: AsRef<str>>(secrets: impl IntoIterator<Item = S>) -> Self {
        let masker = Self::default();
        for secret in secrets {
            masker.add(secret.as_ref());
        }
        masker
    }

    /// Registers a secret. Logs are masked line by line, so like the official runner
    /// each line of a multi-line secret (a PEM key, say) is registered on its own too.
    pub fn add(&self, secret: &str) {
        let secret = secret.trim();
        if secret.is_empty() {
            return;
        }

        let lines = secret
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && *line != secret);

        let mut values = self
            .values
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for value in std::iter::once(secret).chain(lines) {
            if !values.iter().any(|known| known == value) {
                values.push(value.to_string());
            }
        }
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    }

    pub fn mask(&self, text: &str) -> String {
        let values = self
            .values
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        values.iter().fold(text.to_string(), |masked, value| {
            masked.replace(value, MASK)
        })
    }

    pub fn reveals_secret(&self, text: &str) -> bool {
        let values = self
            .values
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        values.iter().any(|value| text.contains(value.as_str()))
    }
}

#[cfg(test)]
#[path = "masker_test.rs"]
mod masker_test;
