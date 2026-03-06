use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub downloads: Option<PathBuf>,
    pub media: Option<PathBuf>,
    pub min_age: Option<u64>,
    pub threads: Option<usize>,
}

pub fn config_path() -> Result<PathBuf> {
    let config_dir =
        dirs::config_dir().ok_or_else(|| anyhow!("Could not find config directory"))?;
    Ok(config_dir.join("dejaview").join("config.json"))
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    load_config_from_path(&path)
}

pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path()?;
    save_config_to_path(&path, config)
}

fn load_config_from_path(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let s = fs::read_to_string(path)?;
    let config = serde_json::from_str(&s)?;
    Ok(config)
}

fn save_config_to_path(path: &Path, config: &Config) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let s = serde_json::to_string_pretty(config)?;
    fs::write(path, s)?;
    Ok(())
}

pub fn cache_path(config: &Config) -> Result<PathBuf> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    Ok(downloads.join(".dejaview_cache.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dejaview_config_{prefix}_{}_{}",
            std::process::id(),
            ts
        ))
    }

    #[test]
    fn load_save_roundtrip() {
        let root = unique_temp_dir("roundtrip");
        let path = root.join("config").join("config.json");
        let config = Config {
            downloads: Some(root.join("downloads")),
            media: Some(root.join("media")),
            min_age: Some(60),
            threads: Some(4),
        };

        save_config_to_path(&path, &config).expect("save config");
        let loaded = load_config_from_path(&path).expect("load config");
        assert_eq!(loaded.downloads, config.downloads);
        assert_eq!(loaded.media, config.media);
        assert_eq!(loaded.min_age, config.min_age);
        assert_eq!(loaded.threads, config.threads);

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn load_invalid_json_fails() {
        let root = unique_temp_dir("invalid_json");
        let path = root.join("config").join("config.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("create config dir");
        fs::write(&path, b"{not-valid-json").expect("write invalid config");

        let result = load_config_from_path(&path);
        assert!(result.is_err());

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn cache_path_requires_downloads() {
        let config = Config::default();
        let result = cache_path(&config);
        assert!(result.is_err());
    }
}
