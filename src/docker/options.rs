/// Translates Docker CLI `--options` strings into bollard API structs.
///
/// GitHub sends container options as a raw string (e.g. `"--health-cmd 'pg_isready'
/// --memory 512m"`). The official runner passes this verbatim to `docker create` on
/// the CLI. Chimera uses bollard (the Docker REST API), which requires structured
/// fields — so we parse the CLI flags ourselves.
use bollard::models::{HealthConfig, HostConfig};
use tracing::warn;

/// Parsed container options from the `--options` string.
#[derive(Debug, Default)]
pub(crate) struct ContainerOptions {
    pub privileged: bool,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub health_check: Option<HealthConfig>,
    pub shm_size: Option<i64>,
    pub memory: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub user: Option<String>,
}

impl ContainerOptions {
    /// Apply host-config-level fields. Health check and user go directly on `Config`.
    pub fn apply_to_host_config(&self, hc: &mut HostConfig) {
        if self.privileged {
            hc.privileged = Some(true);
        }
        if !self.cap_add.is_empty() {
            hc.cap_add
                .get_or_insert_with(Vec::new)
                .extend(self.cap_add.clone());
        }
        if !self.cap_drop.is_empty() {
            hc.cap_drop
                .get_or_insert_with(Vec::new)
                .extend(self.cap_drop.clone());
        }
        if let Some(size) = self.shm_size {
            hc.shm_size = Some(size);
        }
        if let Some(mem) = self.memory {
            hc.memory = Some(mem);
        }
        if let Some(cpus) = self.nano_cpus {
            hc.nano_cpus = Some(cpus);
        }
    }
}

/// Parse the `--options` string into a [`ContainerOptions`] struct.
pub(crate) fn parse_options(options: Option<&str>) -> ContainerOptions {
    let options = match options {
        Some(o) if !o.is_empty() => o,
        _ => return ContainerOptions::default(),
    };

    let tokens = tokenize(options);
    let mut opts = ContainerOptions::default();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--privileged" => opts.privileged = true,
            "--cap-add" => {
                if let Some(val) = next_token(&tokens, &mut i) {
                    opts.cap_add.push(val);
                }
            }
            "--cap-drop" => {
                if let Some(val) = next_token(&tokens, &mut i) {
                    opts.cap_drop.push(val);
                }
            }
            "--health-cmd" => {
                if let Some(val) = next_token(&tokens, &mut i) {
                    let hc = opts.health_check.get_or_insert_with(HealthConfig::default);
                    hc.test = Some(vec!["CMD-SHELL".into(), val]);
                }
            }
            "--health-interval" => {
                if let Some(val) = next_token(&tokens, &mut i)
                    && let Ok(ns) = parse_duration_ns(&val)
                {
                    opts.health_check
                        .get_or_insert_with(HealthConfig::default)
                        .interval = Some(ns);
                }
            }
            "--health-timeout" => {
                if let Some(val) = next_token(&tokens, &mut i)
                    && let Ok(ns) = parse_duration_ns(&val)
                {
                    opts.health_check
                        .get_or_insert_with(HealthConfig::default)
                        .timeout = Some(ns);
                }
            }
            "--health-retries" => {
                if let Some(val) = next_token(&tokens, &mut i)
                    && let Ok(n) = val.parse::<i64>()
                {
                    opts.health_check
                        .get_or_insert_with(HealthConfig::default)
                        .retries = Some(n);
                }
            }
            "--health-start-period" => {
                if let Some(val) = next_token(&tokens, &mut i)
                    && let Ok(ns) = parse_duration_ns(&val)
                {
                    opts.health_check
                        .get_or_insert_with(HealthConfig::default)
                        .start_period = Some(ns);
                }
            }
            "--shm-size" => {
                if let Some(val) = next_token(&tokens, &mut i)
                    && let Ok(bytes) = parse_size_bytes(&val)
                {
                    opts.shm_size = Some(bytes);
                }
            }
            "--memory" | "-m" => {
                if let Some(val) = next_token(&tokens, &mut i)
                    && let Ok(bytes) = parse_size_bytes(&val)
                {
                    opts.memory = Some(bytes);
                }
            }
            "--cpus" => {
                if let Some(val) = next_token(&tokens, &mut i)
                    && let Ok(f) = val.parse::<f64>()
                {
                    opts.nano_cpus = Some((f * 1e9) as i64);
                }
            }
            "--user" | "-u" => {
                if let Some(val) = next_token(&tokens, &mut i) {
                    opts.user = Some(val);
                }
            }
            other => {
                warn!(option = other, "ignoring unrecognized container option");
            }
        }
        i += 1;
    }
    opts
}

/// Consume the next token as a flag value, advancing `i`.
fn next_token(tokens: &[String], i: &mut usize) -> Option<String> {
    if *i + 1 < tokens.len() {
        *i += 1;
        Some(tokens[*i].clone())
    } else {
        None
    }
}

/// Tokenize an options string respecting single and double quotes.
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                let quote = c;
                for inner in chars.by_ref() {
                    if inner == quote {
                        break;
                    }
                    current.push(inner);
                }
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Parse a Docker-style duration string (e.g. "30s", "1m", "500ms") into nanoseconds.
fn parse_duration_ns(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix("ms") {
        let ms: f64 = rest.parse().map_err(|e| format!("invalid duration: {e}"))?;
        return Ok((ms * 1_000_000.0) as i64);
    }
    if let Some(rest) = s.strip_suffix('s') {
        let secs: f64 = rest.parse().map_err(|e| format!("invalid duration: {e}"))?;
        return Ok((secs * 1_000_000_000.0) as i64);
    }
    if let Some(rest) = s.strip_suffix('m') {
        let mins: f64 = rest.parse().map_err(|e| format!("invalid duration: {e}"))?;
        return Ok((mins * 60.0 * 1_000_000_000.0) as i64);
    }
    if let Some(rest) = s.strip_suffix('h') {
        let hours: f64 = rest.parse().map_err(|e| format!("invalid duration: {e}"))?;
        return Ok((hours * 3600.0 * 1_000_000_000.0) as i64);
    }
    // Plain number: treat as nanoseconds (Docker default)
    let ns: i64 = s.parse().map_err(|e| format!("invalid duration: {e}"))?;
    Ok(ns)
}

/// Parse a Docker-style size string (e.g. "256m", "1g", "64k") into bytes.
fn parse_size_bytes(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (num_str, multiplier) =
        if let Some(rest) = s.strip_suffix('g').or_else(|| s.strip_suffix('G')) {
            (rest, 1024 * 1024 * 1024)
        } else if let Some(rest) = s.strip_suffix('m').or_else(|| s.strip_suffix('M')) {
            (rest, 1024 * 1024)
        } else if let Some(rest) = s.strip_suffix('k').or_else(|| s.strip_suffix('K')) {
            (rest, 1024)
        } else if let Some(rest) = s.strip_suffix('b').or_else(|| s.strip_suffix('B')) {
            (rest, 1)
        } else {
            (s, 1)
        };
    let num: f64 = num_str.parse().map_err(|e| format!("invalid size: {e}"))?;
    Ok((num * multiplier as f64) as i64)
}

#[cfg(test)]
#[path = "options_test.rs"]
mod options_test;
