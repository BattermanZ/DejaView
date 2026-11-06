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
    /// Files smaller than this many bytes are skipped. Unset means
    /// [`DEFAULT_MIN_SIZE`]; empty files are skipped whatever it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<u64>,
    /// How many files are walked and quick-hashed at once. Unset means
    /// [`DEFAULT_THREADS`].
    pub threads: Option<usize>,
    /// How many files are read in full at once. Unset means
    /// [`DEFAULT_FULL_THREADS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_threads: Option<usize>,
    /// Names of files and folders every walk leaves out, as globs. Unset means
    /// [`DEFAULT_EXCLUDE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    pub link_mode: Option<LinkMode>,
}

/// The size floor when the config sets none: 1 MiB (ADR 0002).
pub const DEFAULT_MIN_SIZE: u64 = 1024 * 1024;

/// Files walked and quick-hashed at once when the config sets none. Both wait
/// on the network drive far more than on the CPU, so this is not the CPU count.
pub const DEFAULT_THREADS: usize = 16;

/// Files read in full at once when the config sets none. Full reads are limited
/// by the drive's bandwidth, and more of them at once make a disk seek.
pub const DEFAULT_FULL_THREADS: usize = 2;

/// Folders a NAS or an OS keeps for itself: recycle bins, snapshots, thumbnail
/// caches and fsck's leftovers. Walking them costs time, and a recycle-bin copy
/// could otherwise become a link target.
pub const DEFAULT_EXCLUDE: &[&str] = &[
    "@eaDir",
    "#recycle",
    "#snapshot",
    "@Recycle",
    ".@__thumb",
    ".recycle",
    ".snapshots",
    ".Trash-*",
    "$RECYCLE.BIN",
    "lost+found",
];

impl Config {
    /// The smallest file a run considers. Never below 1 byte, so an empty file
    /// is never a candidate: every empty file matches every other, and a torrent
    /// client creates a file empty before writing into it.
    pub fn effective_min_size(&self) -> u64 {
        self.min_size.unwrap_or(DEFAULT_MIN_SIZE).max(1)
    }

    pub fn effective_threads(&self) -> usize {
        self.threads.unwrap_or(DEFAULT_THREADS).max(1)
    }

    pub fn effective_full_threads(&self) -> usize {
        self.full_threads.unwrap_or(DEFAULT_FULL_THREADS).max(1)
    }

    /// The name patterns walks leave out.
    pub fn effective_exclude(&self) -> Vec<String> {
        match &self.exclude {
            Some(patterns) => patterns.clone(),
            None => DEFAULT_EXCLUDE.iter().map(|p| p.to_string()).collect(),
        }
    }
}

/// Check exclude patterns before they are saved. Each is matched against one
/// file or folder name, so a pattern with `/` could never match.
pub fn check_exclude_patterns(patterns: &[String]) -> Result<()> {
    for pattern in patterns {
        if pattern.is_empty() {
            return Err(anyhow!("an exclude pattern cannot be empty"));
        }
        if pattern.contains('/') {
            return Err(anyhow!(
                "exclude pattern {pattern:?} contains /, but each pattern is matched against one file or folder name; give the folder's name alone, such as \"#recycle\""
            ));
        }
    }
    Ok(())
}

/// The config file: `DEJAVIEW_CONFIG` when set, else `dejaview/config.json`
/// under the platform's config folder. A container run as a user with no home
/// has no config folder, so the variable is the way to point at one.
pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DEJAVIEW_CONFIG").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let config_dir = dirs::config_dir().ok_or_else(|| {
        anyhow!(
            "cannot find a config folder: neither XDG_CONFIG_HOME nor HOME is set and this user has no home folder; set DEJAVIEW_CONFIG to the config file's path"
        )
    })?;
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
    let s = fs::read_to_string(path)
        .map_err(|err| anyhow!("cannot read config file {}: {err}", path.display()))?;
    // Every command loads the config first, so a broken file would otherwise
    // block the very commands that set it. Name the file and the way out.
    serde_json::from_str(&s).map_err(|err| {
        anyhow!(
            "config file {} is not valid JSON ({err}); fix it, or delete it and set the paths again with `dejaview config set-downloads` and `dejaview config set-media`",
            path.display()
        )
    })
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
    let downloads = config.downloads.as_ref().ok_or_else(|| {
        anyhow!(
            "downloads path not set; set it with `dejaview config set-downloads PATH`, or with e in the dashboard"
        )
    })?;
    let media = config.media.as_ref().ok_or_else(|| {
        anyhow!(
            "media path not set; set it with `dejaview config set-media PATH`, or with e in the dashboard"
        )
    })?;

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

    #[test]
    fn exclude_defaults_until_set_and_refuses_a_path() {
        let mut config = Config::default();
        assert!(config.effective_exclude().iter().any(|p| p == "#recycle"));
        config.exclude = Some(Vec::new());
        assert!(config.effective_exclude().is_empty());
        assert!(check_exclude_patterns(&["#recycle".into(), ".Trash-*".into()]).is_ok());
        assert!(check_exclude_patterns(&["media/#recycle".into()]).is_err());
        assert!(check_exclude_patterns(&[String::new()]).is_err());
    }
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
            min_size: Some(4096),
            threads: Some(4),
            full_threads: None,
            exclude: None,
            link_mode: Some(LinkMode::SymlinkOnly),
        };

        save_config_to_path(&path, &config).expect("save config");
        let loaded = load_config_from_path(&path).expect("load config");
        assert_eq!(loaded.downloads, config.downloads);
        assert_eq!(loaded.media, config.media);
        assert_eq!(loaded.min_age, config.min_age);
        assert_eq!(loaded.min_size, config.min_size);
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

        let err = load_config_from_path(&path).expect_err("invalid config must fail");
        let message = err.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "names the file: {message}"
        );
        assert!(
            message.contains("delete it"),
            "says how to recover: {message}"
        );

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn min_size_defaults_to_one_mib_and_never_drops_below_one_byte() {
        assert_eq!(Config::default().effective_min_size(), 1024 * 1024);
        let zero = Config {
            min_size: Some(0),
            ..Config::default()
        };
        assert_eq!(zero.effective_min_size(), 1);
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
            min_size: None,
            threads: None,
            full_threads: None,
            exclude: None,
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
            min_size: None,
            threads: None,
            full_threads: None,
            exclude: None,
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
            min_size: None,
            threads: None,
            full_threads: None,
            exclude: None,
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
            min_size: None,
            threads: None,
            full_threads: None,
            exclude: None,
            link_mode: None,
        };

        let result = validate_runtime_paths(&config);
        assert!(result.is_err());
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }
}
