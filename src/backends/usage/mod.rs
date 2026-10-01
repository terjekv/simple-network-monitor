use crate::backends::process;
use crate::domain::UsageOs;
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct UsageCollectionRequest {
    pub host_id: String,
    pub address: String,
    pub os: UsageOs,
    pub ssh_verify_host_key: bool,
    pub timeout: Duration,
    pub linux_min_uid: u32,
    pub macos_min_uid: u32,
}

#[derive(Clone, Debug)]
pub struct UsageFailure {
    pub message: String,
}

#[async_trait]
pub trait UsageCollector: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn collect(&self, request: &UsageCollectionRequest) -> Result<UsageCounts, UsageFailure>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct UsageCounts {
    pub console_users: u32,
    pub remote_users: u32,
}

pub struct SystemSshUsageCollector;

#[async_trait]
impl UsageCollector for SystemSshUsageCollector {
    fn name(&self) -> &'static str {
        "system-ssh"
    }

    async fn collect(&self, request: &UsageCollectionRequest) -> Result<UsageCounts, UsageFailure> {
        let script = remote_usage_script(request.os, request.linux_min_uid, request.macos_min_uid);
        crate::domain::validation::validate_address(&request.address).map_err(|message| {
            UsageFailure {
                message: message.into(),
            }
        })?;
        let mut command = Command::new("ssh");
        command.args(ssh_args(
            &request.address,
            request.timeout,
            request.ssh_verify_host_key,
        ));
        let output = process::run(&mut command, script.as_bytes(), request.timeout)
            .await
            .map_err(|err| UsageFailure {
                message: format!("ssh collection failed: {err}"),
            })?;

        if !output.status.success() {
            let stderr = process::error_text(&output.stderr);
            return Err(UsageFailure {
                message: if stderr.is_empty() {
                    format!("ssh exited with {}", output.status)
                } else {
                    stderr
                },
            });
        }

        serde_json::from_slice(&output.stdout).map_err(|err| UsageFailure {
            message: format!("failed to parse usage json: {err}"),
        })
    }
}

pub fn ssh_args(address: &str, timeout: Duration, verify_host_key: bool) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={}", timeout.as_secs().max(1)),
        "-o".into(),
        "NumberOfPasswordPrompts=0".into(),
    ];
    if verify_host_key {
        args.extend(["-o".into(), "StrictHostKeyChecking=yes".into()]);
    } else {
        args.extend([
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
            "-o".into(),
            "UserKnownHostsFile=/dev/null".into(),
        ]);
    }
    args.extend(["--".into(), address.into(), "sh".into(), "-s".into()]);
    args
}

pub fn remote_usage_script(os: UsageOs, linux_min_uid: u32, macos_min_uid: u32) -> String {
    // When the operator pins os = "linux" or "macos" in config, skip the remote
    // `uname -s` probe and use the chosen min_uid directly. UsageOs::Auto keeps
    // the original behaviour.
    let resolve = match os {
        UsageOs::Linux => format!("snm_os=Linux\nmin_uid={linux_min_uid}"),
        UsageOs::Macos => format!("snm_os=Darwin\nmin_uid={macos_min_uid}"),
        UsageOs::Auto => format!(
            r#"snm_os=$(uname -s) || exit 1
case "$snm_os" in
  Darwin) min_uid={macos_min_uid} ;;
  Linux) min_uid={linux_min_uid} ;;
  *) echo "unsupported operating system" >&2; exit 1 ;;
esac"#
        ),
    };
    format!(
        r#"{resolve}
console=0
remote=0
seen_console=""
seen_remote=""
if [ "$snm_os" = Linux ] && command -v loginctl >/dev/null 2>&1; then
  sessions=$(loginctl list-sessions --no-legend --no-pager) || exit 1
  for session in $(printf '%s\n' "$sessions" | awk '{{print $1}}'); do
    name=""
    uid=""
    session_remote=""
    state=""
    class=""
    details=$(loginctl show-session "$session" -p Name -p User -p Remote -p State -p Class) || exit 1
    while IFS='=' read -r key value; do
      case "$key" in
        Name) name=$value ;;
        User) uid=$value ;;
        Remote) session_remote=$value ;;
        State) state=$value ;;
        Class) class=$value ;;
      esac
    done <<SNM_LOGINCTL
$details
SNM_LOGINCTL
    [ -n "$name" ] || {{ echo "session identity unavailable" >&2; exit 1; }}
    case "$uid" in *[!0-9]*|"") echo "invalid user identifier" >&2; exit 1 ;; esac
    case "$session_remote" in yes|no) ;; *) echo "session remote state unavailable" >&2; exit 1 ;; esac
    [ -n "$state" ] || {{ echo "session state unavailable" >&2; exit 1; }}
    [ "$uid" -ge "$min_uid" ] || continue
    [ "$state" = closing ] && continue
    [ -z "$class" ] || [ "$class" = user ] || continue
    case "$session_remote" in
      yes)
        case " $seen_remote " in *" $name "*) ;; *) seen_remote="$seen_remote $name"; remote=$((remote + 1)) ;; esac
        ;;
      *)
        case " $seen_console " in *" $name "*) ;; *) seen_console="$seen_console $name"; console=$((console + 1)) ;; esac
        ;;
    esac
  done
  printf '{{"console_users":%s,"remote_users":%s}}\n' "$console" "$remote"
  exit 0
fi
observations=$(who) || exit 1
while read -r user tty rest; do
  [ -n "$user" ] || continue
  uid=$(id -u "$user") || exit 1
  case "$uid" in *[!0-9]*|"") echo "invalid user identifier" >&2; exit 1 ;; esac
  [ "$uid" -ge "$min_uid" ] || continue
  case "$tty" in
    console|seat*|tty*|vc/*)
      case " $seen_console " in *" $user "*) ;; *) seen_console="$seen_console $user"; console=$((console + 1)) ;; esac
      ;;
    *)
      case "$rest" in
        *"("*")"*)
          case " $seen_remote " in *" $user "*) ;; *) seen_remote="$seen_remote $user"; remote=$((remote + 1)) ;; esac
          ;;
      esac
      ;;
  esac
done <<SNM_WHO
$observations
SNM_WHO
printf '{{"console_users":%s,"remote_users":%s}}\n' "$console" "$remote"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_args_use_batch_mode_and_remote_shell() {
        let args = ssh_args("host.example", Duration::from_secs(5), true);
        assert_eq!(args[0], "-o");
        assert!(args.contains(&"BatchMode=yes".into()));
        assert!(!args.contains(&"StrictHostKeyChecking=no".into()));
        assert!(args.contains(&"host.example".into()));
        assert!(args.contains(&"sh".into()));
    }

    #[test]
    fn ssh_args_can_disable_host_key_verification() {
        let args = ssh_args("host.example", Duration::from_secs(5), false);
        assert!(args.contains(&"StrictHostKeyChecking=no".into()));
        assert!(args.contains(&"UserKnownHostsFile=/dev/null".into()));
    }

    #[test]
    fn remote_script_outputs_json_without_names_in_format() {
        let script = remote_usage_script(UsageOs::Auto, 1000, 500);
        assert!(script.contains("console_users"));
        assert!(script.contains("remote_users"));
        assert!(!script.contains("username"));
    }

    #[test]
    fn explicit_linux_skips_uname_probe() {
        let script = remote_usage_script(UsageOs::Linux, 1000, 500);
        assert!(!script.contains("uname"));
        assert!(script.contains("min_uid=1000"));
    }

    #[test]
    fn explicit_macos_skips_uname_probe() {
        let script = remote_usage_script(UsageOs::Macos, 1000, 500);
        assert!(!script.contains("uname"));
        assert!(script.contains("min_uid=500"));
    }

    #[test]
    fn auto_keeps_uname_probe() {
        let script = remote_usage_script(UsageOs::Auto, 1000, 500);
        assert!(script.contains("uname"));
    }

    #[test]
    fn linux_script_prefers_loginctl_and_treats_seats_as_console() {
        let script = remote_usage_script(UsageOs::Linux, 1000, 500);
        assert!(script.contains("loginctl list-sessions"));
        assert!(script.contains("loginctl show-session"));
        assert!(script.contains("console|seat*|tty*|vc/*"));
    }
    #[rstest::rstest]
    #[case(false, false, 1)]
    #[case(false, true, 0)]
    #[case(true, false, 0)]
    #[tokio::test]
    async fn executes_script_and_distinguishes_empty_sessions_from_failed_collection(
        #[case] fail: bool,
        #[case] empty: bool,
        #[case] expected_console: u32,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("loginctl");
        std::fs::write(&command, format!("#!/bin/sh\nif {}; then exit 1; fi\ncase \"$1\" in list-sessions) {} ;; show-session) printf 'Name=fake-user\\nUser=1000\\nRemote=no\\nState=active\\nClass=user\\n' ;; esac\n", if fail {"true"} else {"false"}, if empty {":"} else {"echo '1 fake-user'"})).unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut shell = tokio::process::Command::new("sh");
        shell
            .arg("-s")
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()));
        let output = crate::backends::process::run(
            &mut shell,
            remote_usage_script(UsageOs::Linux, 1000, 500).as_bytes(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(output.status.success(), !fail);
        if !fail {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["console_users"], expected_console);
        }
    }

    #[test]
    fn strict_policy_is_explicit_and_script_is_transported_over_stdin() {
        let args = ssh_args("router.example", Duration::from_secs(2), true);
        assert!(args.iter().any(|arg| arg == "StrictHostKeyChecking=yes"));
        assert_eq!(
            &args[args.len() - 4..],
            ["--", "router.example", "sh", "-s"]
        );
    }
}
