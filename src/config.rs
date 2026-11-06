use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::engine::LinkMode;
use crate::fsutil::{normalize_runtime_dir, safe_write_bytes};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub downloads: Option<PathBuf>,
    pub media: Option<PathBuf>,
    pub min_age: Option<u64>,
    pub threads: Option<usize>,
    pub link_mode: Option<LinkMode>,
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
    let s = serde_json::to_string_pretty(config)?;
    safe_write_bytes(path, s.as_bytes())?;
    Ok(())
}

pub fn cache_path(config: &Config) -> Result<PathBuf> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    Ok(downloads.join(".dejaview_cache.json"))
}

pub fn validate_runtime_paths(config: &Config) -> Result<()> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("downloads path not set"))?;
    let media = config
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("media path not set"))?;

    if !downloads.is_absolute() {
        return Err(anyhow!(
            "downloads path must be absolute: {}",
            downloads.display()
        ));
    }
    if !media.is_absolute() {
        return Err(anyhow!("media path must be absolute: {}", media.display()));
    }

    if !downloads.exists() {
        return Err(anyhow!(
            "downloads path does not exist: {}",
            downloads.display()
        ));
    }
    if !media.exists() {
        return Err(anyhow!("media path does not exist: {}", media.display()));
    }
    if !downloads.is_dir() {
        return Err(anyhow!(
            "downloads path is not a directory: {}",
            downloads.display()
        ));
    }
    if !media.is_dir() {
        return Err(anyhow!(
            "media path is not a directory: {}",
            media.display()
        ));
    }

    let normalized_downloads = normalize_runtime_dir(downloads)?;
    let normalized_media = normalize_runtime_dir(media)?;

    if !normalized_downloads.is_absolute() {
        return Err(anyhow!(
            "downloads path must be absolute: {}",
            normalized_downloads.display()
        ));
    }
    if !normalized_media.is_absolute() {
        return Err(anyhow!(
            "media path must be absolute: {}",
            normalized_media.display()
        ));
    }

    ensure_roots_disjoint(&normalized_downloads, &normalized_media)
}

/// Downloads and media must be separate trees. If one sits inside the other, the
/// downloads walk sees library files as downloads and a link run rewrites files
/// inside the library. Both paths must already be canonical, so nesting hidden
/// behind a symlink is caught too.
pub fn ensure_roots_disjoint(downloads: &Path, media: &Path) -> Result<()> {
    if downloads == media {
        return Err(anyhow!(
            "downloads and media are the same directory: {}; point them at separate folders",
            downloads.display()
        ));
    }
    if media.starts_with(downloads) {
        return Err(anyhow!(
            "media path is inside the downloads path ({} is under {}); point them at separate folders",
            media.display(),
            downloads.display()
        ));
    }
    if downloads.starts_with(media) {
        return Err(anyhow!(
            "downloads path is inside the media path ({} is under {}); point media at the library folder itself",
            downloads.display(),
            media.display()
        ));
    }
    Ok(())
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
            link_mode: Some(LinkMode::SymlinkOnly),
        };

        save_config_to_path(&path, &config).expect("save config");
        let loaded = load_config_from_path(&path).expect("load config");
        assert_eq!(loaded.downloads, config.downloads);
        assert_eq!(loaded.media, config.media);
        assert_eq!(loaded.min_age, config.min_age);
        assert_eq!(loaded.threads, config.threads);
        assert_eq!(loaded.link_mode, config.link_mode);

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

    #[test]
    fn validate_runtime_paths_accepts_existing_directories() {
        let root = unique_temp_dir("validate_ok");
        let downloads = root.join("downloads");
        let media = root.join("media");
        fs::create_dir_all(&downloads).expect("create downloads");
        fs::create_dir_all(&media).expect("create media");

        let config = Config {
            downloads: Some(downloads),
            media: Some(media),
            min_age: None,
            threads: None,
            link_mode: None,
        };

        validate_runtime_paths(&config).expect("paths should validate");
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn validate_runtime_paths_rejects_missing_directory() {
        let root = unique_temp_dir("validate_missing");
        let downloads = root.join("downloads");
        fs::create_dir_all(&downloads).expect("create downloads");

        let config = Config {
            downloads: Some(downloads),
            media: Some(root.join("missing-media")),
            min_age: None,
            threads: None,
            link_mode: None,
        };

        let result = validate_runtime_paths(&config);
        assert!(result.is_err());
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    fn config_for(downloads: &Path, media: &Path) -> Config {
        Config {
            downloads: Some(downloads.to_path_buf()),
            media: Some(media.to_path_buf()),
            min_age: None,
            threads: None,
            link_mode: None,
        }
    }

    fn assert_overlap_rejected(config: &Config, expected: &str) {
        let err = validate_runtime_paths(config).expect_err("overlapping roots must be rejected");
        assert!(
            err.to_string().contains(expected),
            "expected '{expected}' in '{err}'"
        );
    }

    #[test]
    fn validate_runtime_paths_rejects_media_inside_downloads() {
        let root = unique_temp_dir("validate_media_nested");
        let downloads = root.join("downloads");
        let media = downloads.join("library");
        fs::create_dir_all(&media).expect("create nested media");

        assert_overlap_rejected(&config_for(&downloads, &media), "media path is inside");
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn validate_runtime_paths_rejects_downloads_inside_media() {
        let root = unique_temp_dir("validate_downloads_nested");
        let media = root.join("data");
        let downloads = media.join("torrents");
        fs::create_dir_all(&downloads).expect("create nested downloads");

        assert_overlap_rejected(&config_for(&downloads, &media), "downloads path is inside");
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn validate_runtime_paths_rejects_same_directory() {
        let root = unique_temp_dir("validate_same_dir");
        let shared = root.join("shared");
        fs::create_dir_all(&shared).expect("create shared dir");

        assert_overlap_rejected(&config_for(&shared, &shared), "the same directory");
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn validate_runtime_paths_rejects_nesting_hidden_behind_symlink() {
        let root = unique_temp_dir("validate_symlink_nested");
        let downloads = root.join("downloads");
        let real_media = downloads.join("library");
        let media_link = root.join("media");
        fs::create_dir_all(&real_media).expect("create nested media");
        std::os::unix::fs::symlink(&real_media, &media_link).expect("create media symlink");

        assert_overlap_rejected(&config_for(&downloads, &media_link), "media path is inside");
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn validate_runtime_paths_rejects_relative_directories() {
        let root = unique_temp_dir("validate_relative");
        let downloads = root.join("downloads");
        let media = root.join("media");
        fs::create_dir_all(&downloads).expect("create downloads");
        fs::create_dir_all(&media).expect("create media");

        let config = Config {
            downloads: Some(PathBuf::from("downloads")),
            media: Some(PathBuf::from("media")),
            min_age: None,
            threads: None,
            link_mode: None,
        };

        let result = validate_runtime_paths(&config);
        assert!(result.is_err());
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }
}
