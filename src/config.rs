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

/// What [`check`] found wrong with a configuration: every problem, each a
/// sentence that names the path and what to do.
#[derive(Debug)]
pub struct Problems(pub Vec<String>);

impl std::fmt::Display for Problems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join("; "))
    }
}

impl std::error::Error for Problems {}

/// The two folders of a configuration that [`check`] passed, resolved.
#[derive(Debug)]
pub struct Folders {
    pub downloads: PathBuf,
    pub media: PathBuf,
}

/// Everything a configuration must pass before a run may use it, the one
/// check `config validate`, every scan and link, and the dashboard's path
/// editor make: both folders set, absolute and existing; downloads writable,
/// since every run writes its cache and lock there and a link run its links;
/// media readable; and neither folder inside the other, nesting behind a
/// symlink included. It reports every problem, not just the first.
///
/// It does not walk the folders. A library reached through a second mount
/// (a bind mount, a mergerfs branch) is found by
/// `engine::ensure_roots_not_mounted_inside`, which `config validate` and
/// the path editor call after this, and by every walk.
pub fn check(config: &Config) -> std::result::Result<Folders, Problems> {
    let mut problems = Vec::new();
    let downloads = check_folder(
        "downloads",
        config.downloads.as_deref(),
        "set-downloads",
        &mut problems,
    );
    let media = check_folder("media", config.media.as_deref(), "set-media", &mut problems);
    if let Some(downloads) = &downloads
        && let Err(err) = probe_directory_writable(downloads)
    {
        problems.push(format!(
            "downloads path is not writable: {} ({err}); every run writes its cache there, and a link run its links. Let the user DejaView runs as write to it",
            downloads.display()
        ));
    }
    if let Some(media) = &media
        && let Err(err) = fs::read_dir(media)
    {
        problems.push(format!(
            "media path is not readable: {} ({err}); let the user DejaView runs as read it",
            media.display()
        ));
    }
    match (downloads, media) {
        (Some(downloads), Some(media)) if problems.is_empty() => {
            match ensure_roots_disjoint(&downloads, &media) {
                Ok(()) => Ok(Folders { downloads, media }),
                Err(err) => Err(Problems(vec![format!("{err:#}")])),
            }
        }
        _ => Err(Problems(problems)),
    }
}

/// One folder of the configuration, resolved, or None with its problem
/// pushed.
fn check_folder(
    name: &str,
    path: Option<&Path>,
    command: &str,
    problems: &mut Vec<String>,
) -> Option<PathBuf> {
    let Some(path) = path else {
        problems.push(format!(
            "{name} path not set; set it with `dejaview config {command} PATH`, or with e in the dashboard"
        ));
        return None;
    };
    let problem = if !path.is_absolute() {
        "must be absolute"
    } else if !path.exists() {
        "does not exist"
    } else if !path.is_dir() {
        "is not a directory"
    } else {
        return match normalize_runtime_dir(path) {
            Ok(resolved) => Some(resolved),
            Err(err) => {
                problems.push(format!("{name} path cannot be used: {err:#}"));
                None
            }
        };
    };
    problems.push(format!(
        "{name} path {problem}: {}; set it with `dejaview config {command} PATH`",
        path.display()
    ));
    None
}

/// Create and remove a file in `path`, to learn whether this user may write
/// there. The name starts with `.dejaview_`, which scans skip, in case a
/// crash leaves it behind.
fn probe_directory_writable(path: &Path) -> std::io::Result<()> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let probe = path.join(format!(
        ".dejaview_validate_{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    // create_new: never open, let alone truncate, a file that is there.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    fs::remove_file(&probe)
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
    use crate::test_common::TempDir;

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

    #[test]
    fn load_save_roundtrip() {
        let root = TempDir::new("roundtrip");
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
    }

    #[test]
    fn load_invalid_json_fails() {
        let root = TempDir::new("invalid_json");
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
    fn check_accepts_existing_directories() {
        let root = TempDir::new("validate_ok");
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

        check(&config).expect("paths should validate");
    }

    #[test]
    fn check_rejects_missing_directory() {
        let root = TempDir::new("validate_missing");
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

        let result = check(&config);
        assert!(result.is_err());
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
        let err = check(config).expect_err("overlapping roots must be rejected");
        assert!(
            err.to_string().contains(expected),
            "expected '{expected}' in '{err}'"
        );
    }

    #[test]
    fn check_rejects_media_inside_downloads() {
        let root = TempDir::new("validate_media_nested");
        let downloads = root.join("downloads");
        let media = downloads.join("library");
        fs::create_dir_all(&media).expect("create nested media");

        assert_overlap_rejected(&config_for(&downloads, &media), "media path is inside");
    }

    #[test]
    fn check_rejects_downloads_inside_media() {
        let root = TempDir::new("validate_downloads_nested");
        let media = root.join("data");
        let downloads = media.join("torrents");
        fs::create_dir_all(&downloads).expect("create nested downloads");

        assert_overlap_rejected(&config_for(&downloads, &media), "downloads path is inside");
    }

    #[test]
    fn check_rejects_same_directory() {
        let root = TempDir::new("validate_same_dir");
        let shared = root.join("shared");
        fs::create_dir_all(&shared).expect("create shared dir");

        assert_overlap_rejected(&config_for(&shared, &shared), "the same directory");
    }

    #[cfg(unix)]
    #[test]
    fn check_rejects_nesting_hidden_behind_symlink() {
        let root = TempDir::new("validate_symlink_nested");
        let downloads = root.join("downloads");
        let real_media = downloads.join("library");
        let media_link = root.join("media");
        fs::create_dir_all(&real_media).expect("create nested media");
        std::os::unix::fs::symlink(&real_media, &media_link).expect("create media symlink");

        assert_overlap_rejected(&config_for(&downloads, &media_link), "media path is inside");
    }

    #[test]
    fn check_rejects_relative_directories() {
        let root = TempDir::new("validate_relative");
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

        let result = check(&config);
        assert!(result.is_err());
    }

    #[test]
    fn probe_directory_writable_does_not_follow_existing_symlink() {
        #[cfg(not(unix))]
        {
            return;
        }

        #[cfg(unix)]
        {
            let root = TempDir::new("probe_symlink");
            let victim = root.join("victim");
            fs::write(&victim, b"keep").expect("write victim");
            let link = root.join(format!(".dejaview_validate_{}.{}", std::process::id(), 1u8));
            std::os::unix::fs::symlink(&victim, &link).expect("create symlink");

            for _ in 0..16 {
                probe_directory_writable(&root).expect("probe should succeed");
            }

            assert_eq!(fs::read(&victim).expect("read victim"), b"keep");
        }
    }

    #[test]
    fn check_names_every_problem_at_once() {
        let root = TempDir::new("check_all");
        let config = config_for(&root.join("no-downloads"), &root.join("no-media"));
        let Problems(problems) = check(&config).expect_err("both missing");
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("downloads path does not exist"));
        assert!(problems[1].contains("media path does not exist"));
        let Problems(problems) = check(&Config::default()).expect_err("unset");
        assert!(problems[0].contains("dejaview config set-downloads PATH"));
        assert!(problems[1].contains("dejaview config set-media PATH"));
    }

    /// Every run writes its cache and lock in downloads, so a folder the user
    /// cannot write to is refused before a walk, not after hours of hashing
    /// when the cache cannot be saved. Root writes anywhere, so as root this
    /// returns early; `just test` runs it again as nobody.
    #[test]
    fn check_refuses_a_downloads_folder_it_cannot_write() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new("check_unwritable");
        let downloads = root.join("downloads");
        let media = root.join("media");
        fs::create_dir_all(&downloads).expect("create downloads");
        fs::create_dir_all(&media).expect("create media");
        fs::set_permissions(&downloads, fs::Permissions::from_mode(0o555)).expect("chmod");
        if probe_directory_writable(&downloads).is_ok() {
            return;
        }
        let Problems(problems) =
            check(&config_for(&downloads, &media)).expect_err("unwritable downloads");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("downloads path is not writable")
                && problems[0].contains(&downloads.display().to_string()),
            "{problems:?}"
        );
    }
}
