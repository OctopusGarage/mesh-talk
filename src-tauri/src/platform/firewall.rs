use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum FirewallStatus {
    Allowed,
    NeedsPermission,
    Unsupported,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FirewallCheckResult {
    pub status: FirewallStatus,
    pub message: Option<String>,
}

pub fn check_firewall(port: u16) -> FirewallCheckResult {
    #[cfg(target_os = "windows")]
    {
        return check_windows_firewall(port);
    }

    #[cfg(target_os = "macos")]
    {
        return check_macos_firewall();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = port;
        return FirewallCheckResult {
            status: FirewallStatus::Unsupported,
            message: Some("Automatic firewall detection is not supported on Linux.".into()),
        };
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        FirewallCheckResult {
            status: FirewallStatus::Unsupported,
            message: Some("Firewall detection not implemented for this platform.".into()),
        }
    }
}

pub fn allow_firewall(port: u16) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        return allow_windows_firewall(port);
    }

    #[cfg(target_os = "macos")]
    {
        return allow_macos_firewall();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = port;
        return Err("Automatic firewall configuration is not supported on Linux.".into());
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Err("Firewall configuration not implemented for this platform.".into())
    }
}

#[cfg(target_os = "windows")]
fn windows_rule_name(port: u16) -> String {
    format!("MeshTalk TCP {}", port)
}

#[cfg(target_os = "windows")]
fn check_windows_firewall(port: u16) -> FirewallCheckResult {
    let rule_name = windows_rule_name(port);
    let output = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={}", rule_name),
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => FirewallCheckResult {
            status: FirewallStatus::Allowed,
            message: None,
        },
        Ok(_) => FirewallCheckResult {
            status: FirewallStatus::NeedsPermission,
            message: Some("Windows Firewall requires an inbound rule for MeshTalk.".into()),
        },
        Err(err) => FirewallCheckResult {
            status: FirewallStatus::Unsupported,
            message: Some(format!("Failed to query firewall: {}", err)),
        },
    }
}

#[cfg(target_os = "windows")]
fn allow_windows_firewall(port: u16) -> Result<(), String> {
    let rule_name = windows_rule_name(port);
    let delete_rule = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={}", rule_name),
        ])
        .output();
    let _ = delete_rule;

    let status = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "add",
            "rule",
            &format!("name={}", rule_name),
            "dir=in",
            "action=allow",
            "protocol=TCP",
            &format!("localport={}", port),
        ])
        .status()
        .map_err(|err| format!("Failed to execute netsh: {}", err))?;

    if status.success() {
        Ok(())
    } else {
        Err("Windows Firewall rejected the rule request. Try running as administrator.".into())
    }
}

#[cfg(target_os = "macos")]
fn firewall_binary() -> PathBuf {
    PathBuf::from("/usr/libexec/ApplicationFirewall/socketfilterfw")
}

#[cfg(target_os = "macos")]
fn current_exe_path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| p.to_str().map(|s| s.to_string()))
}

#[cfg(target_os = "macos")]
fn check_macos_firewall() -> FirewallCheckResult {
    let app_path = match current_exe_path() {
        Some(path) => path,
        None => {
            return FirewallCheckResult {
                status: FirewallStatus::Unsupported,
                message: Some("Unable to determine application path for firewall check.".into()),
            };
        }
    };

    let output = Command::new(firewall_binary()).arg("--listapps").output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains(&app_path) {
                FirewallCheckResult {
                    status: FirewallStatus::Allowed,
                    message: None,
                }
            } else {
                FirewallCheckResult {
                    status: FirewallStatus::NeedsPermission,
                    message: Some("macOS firewall needs permission to allow MeshTalk".into()),
                }
            }
        }
        Ok(out) => FirewallCheckResult {
            status: FirewallStatus::Unsupported,
            message: Some(format!(
                "Firewall query failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )),
        },
        Err(err) => FirewallCheckResult {
            status: FirewallStatus::Unsupported,
            message: Some(format!("Failed to run firewall tool: {}", err)),
        },
    }
}

#[cfg(target_os = "macos")]
fn allow_macos_firewall() -> Result<(), String> {
    let app_path =
        current_exe_path().ok_or_else(|| "Unable to determine application path".to_string())?;
    let bin = firewall_binary();

    let status = Command::new(&bin)
        .args(["--add", &app_path])
        .status()
        .map_err(|err| format!("Failed to run socketfilterfw: {}", err))?;

    if !status.success() {
        return Err("macOS firewall denied the add/app request. You may need to run the app with elevated privileges.".into());
    }

    let status = Command::new(&bin)
        .args(["--unblockapp", &app_path])
        .status()
        .map_err(|err| format!("Failed to apply unblock: {}", err))?;

    if status.success() {
        Ok(())
    } else {
        Err("macOS firewall could not unblock the application. Please allow access manually in System Settings.".into())
    }
}
