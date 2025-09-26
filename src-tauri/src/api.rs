use clap::Parser;
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::Path};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {}

const CONFIG_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/mesh_talk.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub name: String,
    pub port: u16,
}

impl Args {
    pub fn into_config(self) -> AppConfig {
        AppConfig {
            name: default_node_name(),
            port: 7000,
        }
    }
}

impl AppConfig {
    /// Load configuration for the Tauri runtime using environment variables with sane defaults.
    pub fn from_env() -> Self {
        let mut config = Self::from_file(CONFIG_PATH).unwrap_or_default();

        if let Ok(name) = env::var("MESH_TALK_NAME") {
            if !name.trim().is_empty() {
                config.name = name;
            }
        }

        if let Ok(port_str) = env::var("MESH_TALK_PORT") {
            if let Ok(port) = port_str.parse::<u16>() {
                config.port = port;
            }
        }

        if config.name.trim().is_empty() {
            config.name = default_node_name();
        }

        config
    }

    fn from_file<P: AsRef<Path>>(path: P) -> Option<Self> {
        let contents = fs::read_to_string(path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    fn save_to_path<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to_path(CONFIG_PATH)
    }

    pub fn persist_runtime_config(name: &str, port: u16) -> std::io::Result<()> {
        let mut config = Self::from_file(CONFIG_PATH).unwrap_or_default();
        config.name = name.to_string();
        config.port = port;
        config.save()
    }
}

pub fn default_node_name() -> String {
    // Get the hostname/machine name
    let hostname = get_hostname().unwrap_or_else(|| "unknown".to_string());
    format!("{}", hostname)
}

fn get_hostname() -> Option<String> {
    // Try to get hostname from environment variables
    if let Ok(hostname) = std::env::var("HOSTNAME") {
        if !hostname.is_empty() {
            return Some(hostname);
        }
    }

    if let Ok(computer_name) = std::env::var("COMPUTERNAME") {
        if !computer_name.is_empty() {
            return Some(computer_name);
        }
    }

    // On Unix-like systems, try the hostname command
    #[cfg(unix)]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("hostname").output() {
            if output.status.success() {
                let hostname = String::from_utf8_lossy(&output.stdout);
                let hostname = hostname.trim();
                if !hostname.is_empty() {
                    return Some(hostname.to_string());
                }
            }
        }
    }

    None
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            name: default_node_name(),
            port: 7000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_config_path() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!(
            "{}/mesh_talk_config_{}.json",
            env::temp_dir().display(),
            timestamp
        )
    }

    #[test]
    fn save_to_path_writes_json() {
        let path = temp_config_path();
        let config = AppConfig {
            name: "test-node".into(),
            port: 7123,
        };

        config.save_to_path(&path).expect("write config");

        let contents = fs::read_to_string(&path).expect("read file");
        assert!(contents.contains("test-node"));
        assert!(contents.contains("7123"));
    }
}
