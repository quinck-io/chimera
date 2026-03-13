use bollard::models::HostConfig;

use super::*;

// ── parse_options ────────────────────────────────────────────────────

#[test]
fn privileged() {
    let opts = parse_options(Some("--privileged"));
    assert!(opts.privileged);

    let mut hc = HostConfig::default();
    opts.apply_to_host_config(&mut hc);
    assert_eq!(hc.privileged, Some(true));
}

#[test]
fn cap_add() {
    let opts = parse_options(Some("--cap-add SYS_PTRACE --cap-add NET_ADMIN"));
    assert_eq!(opts.cap_add, vec!["SYS_PTRACE", "NET_ADMIN"]);

    let mut hc = HostConfig::default();
    opts.apply_to_host_config(&mut hc);
    assert_eq!(
        hc.cap_add.as_deref(),
        Some(&["SYS_PTRACE".to_string(), "NET_ADMIN".to_string()] as &[String])
    );
}

#[test]
fn cap_drop() {
    let opts = parse_options(Some("--cap-drop ALL"));
    assert_eq!(opts.cap_drop, vec!["ALL"]);
}

#[test]
fn empty() {
    let opts = parse_options(None);
    assert!(!opts.privileged);
    assert!(opts.cap_add.is_empty());
    assert!(opts.health_check.is_none());
}

#[test]
fn health_cmd() {
    let opts = parse_options(Some("--health-cmd 'pg_isready -U postgres'"));
    let hc = opts.health_check.unwrap();
    assert_eq!(
        hc.test,
        Some(vec![
            "CMD-SHELL".to_string(),
            "pg_isready -U postgres".to_string()
        ])
    );
}

#[test]
fn health_full() {
    let opts = parse_options(Some(
        "--health-cmd 'curl -f http://localhost/' --health-interval 10s --health-timeout 5s --health-retries 3 --health-start-period 30s",
    ));
    let hc = opts.health_check.unwrap();
    assert_eq!(
        hc.test,
        Some(vec!["CMD-SHELL".into(), "curl -f http://localhost/".into()])
    );
    assert_eq!(hc.interval, Some(10_000_000_000));
    assert_eq!(hc.timeout, Some(5_000_000_000));
    assert_eq!(hc.retries, Some(3));
    assert_eq!(hc.start_period, Some(30_000_000_000));
}

#[test]
fn shm_size() {
    let opts = parse_options(Some("--shm-size 256m"));
    assert_eq!(opts.shm_size, Some(256 * 1024 * 1024));
}

#[test]
fn memory() {
    let opts = parse_options(Some("--memory 512m"));
    assert_eq!(opts.memory, Some(512 * 1024 * 1024));

    let opts2 = parse_options(Some("-m 1g"));
    assert_eq!(opts2.memory, Some(1024 * 1024 * 1024));
}

#[test]
fn cpus() {
    let opts = parse_options(Some("--cpus 2.5"));
    assert_eq!(opts.nano_cpus, Some(2_500_000_000));
}

#[test]
fn user() {
    let opts = parse_options(Some("--user root"));
    assert_eq!(opts.user.as_deref(), Some("root"));

    let opts2 = parse_options(Some("-u 1000:1000"));
    assert_eq!(opts2.user.as_deref(), Some("1000:1000"));
}

#[test]
fn mixed() {
    let opts = parse_options(Some(
        "--privileged --cap-add SYS_PTRACE --memory 1g --cpus 2 --user root --health-cmd 'true'",
    ));
    assert!(opts.privileged);
    assert_eq!(opts.cap_add, vec!["SYS_PTRACE"]);
    assert_eq!(opts.memory, Some(1024 * 1024 * 1024));
    assert_eq!(opts.nano_cpus, Some(2_000_000_000));
    assert_eq!(opts.user.as_deref(), Some("root"));
    assert!(opts.health_check.is_some());
}

#[test]
fn quoted_strings() {
    let opts = parse_options(Some(r#"--health-cmd "pg_isready -U postgres""#));
    let hc = opts.health_check.unwrap();
    assert_eq!(
        hc.test,
        Some(vec!["CMD-SHELL".into(), "pg_isready -U postgres".into()])
    );
}

// ── parse_duration_ns ────────────────────────────────────────────────

#[test]
fn duration_seconds() {
    assert_eq!(parse_duration_ns("30s").unwrap(), 30_000_000_000);
}

#[test]
fn duration_minutes() {
    assert_eq!(parse_duration_ns("2m").unwrap(), 120_000_000_000);
}

#[test]
fn duration_milliseconds() {
    assert_eq!(parse_duration_ns("500ms").unwrap(), 500_000_000);
}

#[test]
fn duration_hours() {
    assert_eq!(parse_duration_ns("1h").unwrap(), 3_600_000_000_000);
}

#[test]
fn duration_plain_number() {
    assert_eq!(parse_duration_ns("1000000000").unwrap(), 1_000_000_000);
}

#[test]
fn duration_invalid() {
    assert!(parse_duration_ns("abc").is_err());
}

// ── parse_size_bytes ─────────────────────────────────────────────────

#[test]
fn size_megabytes() {
    assert_eq!(parse_size_bytes("256m").unwrap(), 256 * 1024 * 1024);
    assert_eq!(parse_size_bytes("256M").unwrap(), 256 * 1024 * 1024);
}

#[test]
fn size_gigabytes() {
    assert_eq!(parse_size_bytes("1g").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_size_bytes("1G").unwrap(), 1024 * 1024 * 1024);
}

#[test]
fn size_kilobytes() {
    assert_eq!(parse_size_bytes("64k").unwrap(), 64 * 1024);
}

#[test]
fn size_plain_number() {
    assert_eq!(parse_size_bytes("4096").unwrap(), 4096);
}

#[test]
fn size_invalid() {
    assert!(parse_size_bytes("xyz").is_err());
}

// ── tokenize ─────────────────────────────────────────────────────────

#[test]
fn tokenize_simple() {
    let tokens = tokenize("--privileged --cap-add SYS_PTRACE");
    assert_eq!(tokens, vec!["--privileged", "--cap-add", "SYS_PTRACE"]);
}

#[test]
fn tokenize_single_quotes() {
    let tokens = tokenize("--health-cmd 'pg_isready -U postgres'");
    assert_eq!(tokens, vec!["--health-cmd", "pg_isready -U postgres"]);
}

#[test]
fn tokenize_double_quotes() {
    let tokens = tokenize(r#"--health-cmd "curl -f http://localhost/""#);
    assert_eq!(tokens, vec!["--health-cmd", "curl -f http://localhost/"]);
}

#[test]
fn tokenize_mixed_quotes() {
    let tokens = tokenize(r#"--health-cmd 'test' --user "root""#);
    assert_eq!(tokens, vec!["--health-cmd", "test", "--user", "root"]);
}

// ── apply_to_host_config ─────────────────────────────────────────────

#[test]
fn apply_to_host_config_all_fields() {
    let opts = ContainerOptions {
        privileged: true,
        cap_add: vec!["SYS_PTRACE".into()],
        cap_drop: vec!["ALL".into()],
        shm_size: Some(256 * 1024 * 1024),
        memory: Some(512 * 1024 * 1024),
        nano_cpus: Some(2_000_000_000),
        ..Default::default()
    };

    let mut hc = HostConfig::default();
    opts.apply_to_host_config(&mut hc);

    assert_eq!(hc.privileged, Some(true));
    assert_eq!(hc.cap_add, Some(vec!["SYS_PTRACE".into()]));
    assert_eq!(hc.cap_drop, Some(vec!["ALL".into()]));
    assert_eq!(hc.shm_size, Some(256 * 1024 * 1024));
    assert_eq!(hc.memory, Some(512 * 1024 * 1024));
    assert_eq!(hc.nano_cpus, Some(2_000_000_000));
}
