use super::*;

fn parse(tcp: &str, host_tcp: &str) -> Result<AppConfig, ConfigError> {
    AppConfig::from_toml_str(&format!(
        r#"
[modules.tcp]
{tcp}
[[hosts]]
id = "r1"
address = "192.0.2.1"
groups = ["example"]
[hosts.modules.tcp]
{host_tcp}
"#
    ))
}

#[test]
fn resolves_named_checks_and_host_timing_overrides() {
    let config = parse(
        "enabled = true\ninterval = '1m'",
        "timeout = '2s'\nchecks = [{ id = 'web', port = 443 }, { id = 'ssh', port = 22 }]",
    )
    .unwrap();
    let tcp = &config.hosts[0].modules.tcp;
    assert!(tcp.enabled);
    assert_eq!(tcp.interval, Duration::from_secs(60));
    assert_eq!(tcp.timeout, Duration::from_secs(2));
    assert_eq!(tcp.checks.len(), 2);
    assert_eq!(tcp.checks[0].id.as_str(), "web");
}

#[rstest::rstest]
#[case("enabled = true", "checks = [{ id = 'web', port = 0 }]")]
#[case("enabled = true", "checks = [{ id = 'web', port = 65536 }]")]
#[case(
    "enabled = true",
    "checks = [{ id = 'web', port = 443 }, { id = 'web', port = 22 }]"
)]
#[case("enabled = true", "checks = [{ id = 'bad/id', port = 443 }]")]
#[case("enabled = true", "checks = [{ id = 'web', port = 443, typo = true }]")]
#[case("enabled = false", "enabled = true")]
#[case("concurrency = 0", "")]
#[case("concurrency = 4097", "")]
#[case("timeout = '0s'", "")]
#[case("", "interval = '0s'")]
#[case("typo = true", "")]
#[case("", "typo = true")]
fn rejects_invalid_tcp_configuration(#[case] tcp: &str, #[case] host_tcp: &str) {
    assert!(parse(tcp, host_tcp).is_err());
}

#[test]
fn rejects_excessive_check_count() {
    let checks = (0..65)
        .map(|i| format!("{{ id = 'c{i}', port = 443 }}"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(parse("enabled = true", &format!("checks = [{checks}]")).is_err());
}

#[test]
fn tcp_is_disabled_by_default() {
    assert!(
        !parse("", "checks = [{ id = 'web', port = 443 }]")
            .unwrap()
            .hosts[0]
            .modules
            .tcp
            .enabled
    );
}
