use crate::domain::UsageOs;
use async_trait::async_trait;
use serde::Deserialize;
use std::{process::Stdio, time::Duration};
use tokio::{process::Command, time};

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
        let mut command = Command::new("ssh");
        command
            .args(ssh_args(
                &request.address,
                request.timeout,
                request.ssh_verify_host_key,
                &script,
            ))
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|err| UsageFailure {
            message: format!("failed to start ssh: {err}"),
        })?;

        let output = time::timeout(request.timeout, child.wait_with_output())
            .await
            .map_err(|_| UsageFailure {
                message: "ssh collection timed out".into(),
            })?
            .map_err(|err| UsageFailure {
                message: format!("failed to wait for ssh: {err}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
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

pub fn ssh_args(
    address: &str,
    timeout: Duration,
    verify_host_key: bool,
    script: &str,
) -> Vec<String> {
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={}", timeout.as_secs().max(1)),
        "-o".into(),
        "NumberOfPasswordPrompts=0".into(),
    ];
    if !verify_host_key {
        args.extend([
            "-o".into(),
            "StrictHostKeyChecking=no".into(),
            "-o".into(),
            "UserKnownHostsFile=/dev/null".into(),
        ]);
    }
    args.extend([address.into(), "sh".into(), "-lc".into(), script.into()]);
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
            r#"snm_os=$(uname -s 2>/dev/null || echo unknown)
case "$snm_os" in
  Darwin) min_uid={macos_min_uid} ;;
  *) min_uid={linux_min_uid} ;;
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
  for session in $(loginctl list-sessions --no-legend --no-pager 2>/dev/null | awk '{{print $1}}'); do
    name=""
    uid=""
    session_remote=""
    state=""
    class=""
    while IFS='=' read -r key value; do
      case "$key" in
        Name) name=$value ;;
        User) uid=$value ;;
        Remote) session_remote=$value ;;
        State) state=$value ;;
        Class) class=$value ;;
      esac
    done <<SNM_LOGINCTL
$(loginctl show-session "$session" -p Name -p User -p Remote -p State -p Class 2>/dev/null)
SNM_LOGINCTL
    [ -n "$name" ] || continue
    case "$uid" in *[!0-9]*|"") uid=0 ;; esac
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
  if [ "$console" -gt 0 ] || [ "$remote" -gt 0 ]; then
    printf '{{"console_users":%s,"remote_users":%s}}\n' "$console" "$remote"
    exit 0
  fi
fi
while read -r user tty rest; do
  [ -n "$user" ] || continue
  uid=$(id -u "$user" 2>/dev/null || echo 0)
  case "$uid" in *[!0-9]*|"") uid=0 ;; esac
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
$(who)
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
        let args = ssh_args("host.example", Duration::from_secs(5), true, "echo ok");
        assert_eq!(args[0], "-o");
        assert!(args.contains(&"BatchMode=yes".into()));
        assert!(!args.contains(&"StrictHostKeyChecking=no".into()));
        assert!(args.contains(&"host.example".into()));
        assert!(args.contains(&"sh".into()));
    }

    #[test]
    fn ssh_args_can_disable_host_key_verification() {
        let args = ssh_args("host.example", Duration::from_secs(5), false, "echo ok");
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
}
