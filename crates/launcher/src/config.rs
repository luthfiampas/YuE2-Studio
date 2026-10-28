use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub runpod_api_key: Option<String>,
    #[serde(default = "default_local_port")]
    pub local_port: u16,
    #[serde(default)]
    pub ssh_key_path: Option<PathBuf>,
    #[serde(default = "default_pod_refresh_interval_secs")]
    pub pod_refresh_interval_secs: u64, // 0 means manual refresh only
    #[serde(default = "default_server_poll_interval_secs")]
    pub server_poll_interval_secs: u64, // 0 disables polling entirely
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Role name to colour (`#rrggbb` or a colour name), overriding the preset.
    #[serde(default)]
    pub theme_colors: BTreeMap<String, String>,
}

/// Container image every pod is deployed from.
pub const DOCKER_IMAGE: &str = "runpod/pytorch:1.4.1-cu1281-torch2130-ubuntu2404";

fn default_theme() -> String {
    "default".to_string()
}

fn default_local_port() -> u16 {
    8791
}

fn default_pod_refresh_interval_secs() -> u64 {
    30
}

fn default_server_poll_interval_secs() -> u64 {
    2
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runpod_api_key: std::env::var("RUNPOD_API_KEY").ok(),
            local_port: default_local_port(),
            ssh_key_path: None,
            pod_refresh_interval_secs: default_pod_refresh_interval_secs(),
            server_poll_interval_secs: default_server_poll_interval_secs(),
            theme: default_theme(),
            theme_colors: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        Path::new(&home)
            .join(".config")
            .join("yue2-studio")
            .join("launcher.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(mut cfg) = toml::from_str::<Config>(&content)
        {
            if cfg.runpod_api_key.is_none() {
                cfg.runpod_api_key = std::env::var("RUNPOD_API_KEY").ok();
            }
            return cfg;
        }
        Self::default()
    }

    /// Writes the config to `~/.config/yue2-studio/launcher.toml`.
    ///
    /// The file holds the RunPod API key, so it is owner-only.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::config_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            restrict(parent, 0o700)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        restrict(path, 0o600)?;
        Ok(())
    }
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(mode),
    )?)
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yue2-config-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_every_field() {
        let dir = scratch("roundtrip");
        let path = dir.join("launcher.toml");
        let config = Config {
            runpod_api_key: Some("secret".to_string()),
            theme: "warm".to_string(),
            theme_colors: BTreeMap::from([("accent".to_string(), "#123456".to_string())]),
            local_port: 9000,
            ..Config::default()
        };
        config.save_to(&path).expect("save");

        let text = std::fs::read_to_string(&path).expect("read back");
        let loaded: Config = toml::from_str(&text).expect("parse");
        assert_eq!(loaded.runpod_api_key, config.runpod_api_key);
        assert_eq!(loaded.theme, "warm");
        assert_eq!(
            loaded.theme_colors.get("accent").map(String::as_str),
            Some("#123456")
        );
        assert_eq!(loaded.local_port, 9000);
        assert_eq!(loaded.server_poll_interval_secs, 2);
    }

    #[cfg(unix)]
    #[test]
    fn stored_api_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("perms");
        let path = dir.join("launcher.toml");
        Config::default().save_to(&path).expect("save");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "config file must not be world readable"
        );
        let dir_mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "config dir must be owner only");
    }

    #[test]
    fn a_missing_file_falls_back_to_defaults() {
        let config = Config::default();
        assert_eq!(config.local_port, 8791);
        assert_eq!(config.theme, "default");
        assert!(config.theme_colors.is_empty());
    }
}
