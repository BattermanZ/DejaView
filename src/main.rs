use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::*;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

mod config;
mod engine;
mod fsutil;
mod logger;
mod protocol;
mod tui;

use config::{load_config, save_config};
use engine::LinkMode;
use fsutil::normalize_runtime_dir;
use std::fs;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Scan for duplicates (dry run)
    Scan {
        /// Fail if any scan/hash/link errors occur
        #[arg(long)]
        strict: bool,
        /// Skip files smaller than this many bytes for this run (default: the config's, else 1 MiB)
        #[arg(long, value_name = "BYTES", value_parser = clap::value_parser!(u64).range(1..))]
        min_size: Option<u64>,
        /// Force symlink creation and never attempt hardlinks
        #[arg(long)]
        symlink_only: bool,
        #[command(flatten)]
        output: RunOutputArgs,
    },
    /// Replace duplicates with hardlinks when possible, else symlinks
    Link {
        /// Fail if any scan/hash/link errors occur
        #[arg(long)]
        strict: bool,
        /// Ask for confirmation before applying changes (interactive terminals only)
        #[arg(long)]
        confirm: bool,
        /// Skip files smaller than this many bytes for this run (default: the config's, else 1 MiB)
        #[arg(long, value_name = "BYTES", value_parser = clap::value_parser!(u64).range(1..))]
        min_size: Option<u64>,
        /// Force symlink creation and never attempt hardlinks
        #[arg(long)]
        symlink_only: bool,
        /// Internal filter for linking only specific source paths
        #[arg(long = "only-source", hide = true)]
        only_sources: Vec<PathBuf>,
        /// Internal: a JSON list of reviewed {source, target} rows; link only those
        #[arg(
            long,
            hide = true,
            value_name = "FILE",
            conflicts_with = "only_sources"
        )]
        plan: Option<PathBuf>,
        #[command(flatten)]
        output: RunOutputArgs,
    },
    /// Manage the configuration
    Config(ConfigArgs),
    /// Manage the cache
    Cache(CacheArgs),
    /// Open interactive terminal UI
    Tui {
        /// Use strict mode for scan/link actions triggered from the TUI
        #[arg(long)]
        strict: bool,
    },
    /// Preview alternative terminal logo directions
    TuiLogoPreview,
}

#[derive(Parser, Debug)]
struct RunOutputArgs {
    /// Print machine-readable JSON summary to stdout
    #[arg(long)]
    json: bool,
    /// Write run report JSON to this path
    #[arg(long)]
    report: Option<PathBuf>,
    /// Progress display mode
    #[arg(long, value_enum, default_value_t = ProgressMode::Auto)]
    progress: ProgressMode,
    /// Shortcut for --progress off
    #[arg(long, conflicts_with = "progress")]
    no_progress: bool,
    /// Internal NDJSON event stream for the TUI subprocess runner
    #[arg(long, hide = true)]
    tui_events: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum ProgressMode {
    Auto,
    On,
    Off,
}

fn progress_enabled(mode: ProgressMode, no_progress: bool, json: bool, tui_events: bool) -> bool {
    if no_progress || json || tui_events {
        return false;
    }
    match mode {
        ProgressMode::Auto => std::io::stdout().is_terminal(),
        ProgressMode::On => true,
        ProgressMode::Off => false,
    }
}

#[derive(Parser, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommands,
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    /// Set the downloads path
    SetDownloads { path: PathBuf },
    /// Set the media path
    SetMedia { path: PathBuf },
    /// Show the current configuration
    Show,
    /// Set the minimum file age in seconds before considering files
    SetMinAge { seconds: u64 },
    /// Set the smallest file size in bytes to consider (default 1 MiB; empty files are always skipped)
    SetMinSize {
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        bytes: u64,
    },
    /// Set worker thread count for hashing
    SetThreads { count: usize },
    /// Set link mode
    SetLinkMode { mode: LinkModeArg },
    /// Validate configured paths and permissions
    Validate,
}

#[derive(Parser, Debug)]
struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommands,
}

#[derive(Subcommand, Debug)]
enum CacheCommands {
    /// Clear the cache
    Clear,
    /// Show cache info
    Info,
    /// List DejaView temp artifacts left in the downloads tree
    AuditTemp,
    /// Remove DejaView temp artifacts left in the downloads tree
    CleanupTemp,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum LinkModeArg {
    Best,
    SymlinkOnly,
}

impl From<LinkModeArg> for LinkMode {
    fn from(value: LinkModeArg) -> Self {
        match value {
            LinkModeArg::Best => LinkMode::Best,
            LinkModeArg::SymlinkOnly => LinkMode::SymlinkOnly,
        }
    }
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err:?}");
            // ADR 0003: 3 tells a script that another run had the lock and
            // this one did nothing, which is not a failure to alert on.
            if err.is::<engine::LockHeld>() {
                ExitCode::from(3)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    let command_name = command_name(&cli.command);
    let (_log_session, log_place) = logger::init(command_name);
    match &log_place {
        Ok(place) => logger::info("main", format!("logger initialized at {place}")),
        Err(warning) => eprintln!("{} {warning}", "⚠".yellow()),
    }
    let mut config = load_config()?;

    if let Some(threads) = config.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }

    match cli.command {
        Commands::Scan {
            strict,
            min_size,
            symlink_only,
            output,
        } => {
            ensure_required_paths(&config)?;
            if min_size.is_some() {
                config.min_size = min_size;
            }
            engine::run_scan_with_options(
                &config,
                engine::ScanRunOptions {
                    dry_run: true,
                    strict,
                    report_path: output.report.as_deref(),
                    json_output: output.json,
                    show_progress: progress_enabled(
                        output.progress,
                        output.no_progress,
                        output.json,
                        output.tui_events,
                    ),
                    require_confirm: false,
                    tui_events: output.tui_events,
                    link_mode: effective_link_mode(&config, symlink_only),
                    only_sources: &[],
                    only_pairs: &[],
                    cancel: run_cancel_token(output.tui_events),
                },
            )?
        }
        Commands::Link {
            strict,
            confirm,
            min_size,
            symlink_only,
            only_sources,
            plan,
            output,
        } => {
            ensure_required_paths(&config)?;
            let only_pairs = match &plan {
                Some(path) => read_plan(path)?,
                None => Vec::new(),
            };
            let only_sources: Vec<PathBuf> = if only_pairs.is_empty() {
                only_sources
            } else {
                only_pairs.iter().map(|(src, _)| src.clone()).collect()
            };
            if min_size.is_some() {
                config.min_size = min_size;
            }
            engine::run_scan_with_options(
                &config,
                engine::ScanRunOptions {
                    dry_run: false,
                    strict,
                    report_path: output.report.as_deref(),
                    json_output: output.json,
                    show_progress: progress_enabled(
                        output.progress,
                        output.no_progress,
                        output.json,
                        output.tui_events,
                    ),
                    require_confirm: confirm,
                    tui_events: output.tui_events,
                    link_mode: effective_link_mode(&config, symlink_only),
                    only_sources: &only_sources,
                    only_pairs: &only_pairs,
                    cancel: run_cancel_token(output.tui_events),
                },
            )?
        }
        Commands::Config(args) => match args.command {
            ConfigCommands::SetDownloads { path } => {
                config.downloads = Some(normalize_runtime_dir(&path)?);
                save_config(&config)?;
                println!("{} Downloads path set.", "✔".green());
            }
            ConfigCommands::SetMedia { path } => {
                config.media = Some(normalize_runtime_dir(&path)?);
                save_config(&config)?;
                println!("{} Media path set.", "✔".green());
            }
            ConfigCommands::Show => {
                println!("{}", serde_json::to_string_pretty(&config)?);
            }
            ConfigCommands::SetMinAge { seconds } => {
                config.min_age = Some(seconds);
                save_config(&config)?;
                println!("{} min_age set to {} seconds.", "✔".green(), seconds);
            }
            ConfigCommands::SetMinSize { bytes } => {
                config.min_size = Some(bytes);
                save_config(&config)?;
                println!("{} min_size set to {} bytes.", "✔".green(), bytes);
            }
            ConfigCommands::SetThreads { count } => {
                if count == 0 {
                    return Err(anyhow::anyhow!("threads must be greater than 0"));
                }
                config.threads = Some(count);
                save_config(&config)?;
                println!("{} threads set to {}.", "✔".green(), count);
            }
            ConfigCommands::SetLinkMode { mode } => {
                config.link_mode = Some(mode.into());
                save_config(&config)?;
                println!("{} link mode set.", "✔".green());
            }
            ConfigCommands::Validate => validate_config(&config)?,
        },
        Commands::Cache(args) => match args.command {
            CacheCommands::Clear => {
                engine::clear_cache(&config)?;
                println!("{} Cache cleared.", "✔".green());
            }
            CacheCommands::Info => {
                let (path, entries) = engine::cache_info(&config)?;
                println!("Cache location: {}", path.display());
                println!("Cached entries: {}", entries);
            }
            CacheCommands::AuditTemp => {
                let paths = engine::audit_temp_artifacts(&config)?;
                println!("Temp artifacts: {}", paths.len());
                for path in paths {
                    println!("{}", path.display());
                }
            }
            CacheCommands::CleanupTemp => {
                let paths = engine::cleanup_temp_artifacts(&config)?;
                println!("Removed temp artifacts: {}", paths.len());
                for path in paths {
                    println!("{}", path.display());
                }
            }
        },
        Commands::Tui { strict } => {
            tui::run(&config, strict)?;
        }
        Commands::TuiLogoPreview => {
            tui::run_logo_preview()?;
        }
    }
    Ok(())
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Scan { .. } => "scan",
        Commands::Link { .. } => "link",
        Commands::Config(_) => "config",
        Commands::Cache(_) => "cache",
        Commands::Tui { .. } => "tui",
        Commands::TuiLogoPreview => "tui-logo-preview",
    }
}

/// A run driven by the TUI can be cancelled by a `cancel` line on stdin; any
/// other run ignores stdin.
fn run_cancel_token(tui_events: bool) -> engine::CancelToken {
    let token = engine::CancelToken::default();
    if tui_events {
        token.watch(std::io::stdin());
    }
    token
}

/// The rows a person reviewed, as written by the TUI. An empty plan is an
/// error: an empty filter would mean "link everything".
fn read_plan(path: &std::path::Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let bytes = fs::read(path)
        .map_err(|err| anyhow::anyhow!("cannot read the plan {}: {err}", path.display()))?;
    let pairs: Vec<protocol::PlannedPair> = serde_json::from_slice(&bytes)
        .map_err(|err| anyhow::anyhow!("the plan {} is not valid: {err}", path.display()))?;
    if pairs.is_empty() {
        return Err(anyhow::anyhow!(
            "the plan {} holds no rows, so there is nothing to link",
            path.display()
        ));
    }
    Ok(pairs
        .into_iter()
        .map(|pair| (PathBuf::from(pair.source), PathBuf::from(pair.target)))
        .collect())
}

fn effective_link_mode(config: &config::Config, symlink_only: bool) -> LinkMode {
    if symlink_only {
        LinkMode::SymlinkOnly
    } else {
        config.link_mode.unwrap_or(LinkMode::Best)
    }
}

fn ensure_required_paths(config: &config::Config) -> Result<()> {
    config::validate_runtime_paths(config)
}

fn validate_config(config: &config::Config) -> Result<()> {
    let mut failures = Vec::new();

    match &config.downloads {
        Some(path) => {
            if !path.exists() {
                failures.push(format!("downloads path does not exist: {}", path.display()));
            } else if !path.is_dir() {
                failures.push(format!(
                    "downloads path is not a directory: {}",
                    path.display()
                ));
            } else {
                match probe_directory_writable(path) {
                    Ok(_) => {}
                    Err(err) => failures.push(format!(
                        "downloads path is not writable (needed for cache/link ops): {} ({err})",
                        path.display()
                    )),
                }
            }
        }
        None => failures.push(
            "downloads path not set; run: dejaview config set-downloads /path/to/downloads"
                .to_string(),
        ),
    }

    match &config.media {
        Some(path) => {
            if !path.exists() {
                failures.push(format!("media path does not exist: {}", path.display()));
            } else if !path.is_dir() {
                failures.push(format!("media path is not a directory: {}", path.display()));
            } else if let Err(err) = fs::read_dir(path) {
                failures.push(format!(
                    "media path is not readable (needed for scan/hash): {} ({err})",
                    path.display()
                ));
            }
        }
        None => failures
            .push("media path not set; run: dejaview config set-media /path/to/media".to_string()),
    }

    if let (Some(downloads), Some(media)) = (&config.downloads, &config.media)
        && let (Ok(downloads), Ok(media)) = (
            normalize_runtime_dir(downloads),
            normalize_runtime_dir(media),
        )
    {
        if let Err(err) = config::ensure_roots_disjoint(&downloads, &media) {
            failures.push(err.to_string());
        } else if let Err(err) = engine::ensure_roots_not_mounted_inside(&downloads, &media) {
            failures.push(format!("{err:#}"));
        }
    }

    if failures.is_empty() {
        if let (Some(downloads), Some(media)) = (&config.downloads, &config.media) {
            report_hardlink_probe(downloads, media);
        }
        println!("{} Configuration is valid.", "✔".green());
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("{} {}", "✖".red(), failure);
        }
        Err(anyhow::anyhow!("configuration validation failed"))
    }
}

/// Try one real hardlink and say what link runs will do. A failure is not a
/// validation failure: the symlink fallback is the default (ADR 0001), but the
/// person should know which cause applies and whether they can fix it.
fn report_hardlink_probe(downloads: &std::path::Path, media: &std::path::Path) {
    match engine::probe_hardlink(downloads, media) {
        engine::HardlinkProbe::NoFile => {
            println!("{} No library file to try a hardlink with yet.", "ℹ".cyan())
        }
        engine::HardlinkProbe::Works { file } => println!(
            "{} A test hardlink of {} into downloads worked: hardlinks work, so link runs free space. On a mergerfs pool this tried one disk; files on others can still fall back to symlinks.",
            "✔".green(),
            file.display()
        ),
        engine::HardlinkProbe::Fails { file, cause, error } => eprintln!(
            "{} A test hardlink of {} into downloads failed: {cause} ({error}). Link runs will make symlinks instead, and each breaks if Sonarr or Radarr upgrades, renames or deletes its library file.",
            "⚠".yellow(),
            file.display()
        ),
    }
}

fn probe_directory_writable(path: &std::path::Path) -> Result<()> {
    let probe = path.join(format!(
        ".dejaview_validate_{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    file.write_all(b"probe")?;
    file.sync_all()?;
    fs::remove_file(&probe)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dejaview_main_{prefix}_{}_{}",
            std::process::id(),
            ts
        ))
    }

    #[test]
    fn parses_scan_command() {
        let cli = Cli::try_parse_from(["dejaview", "scan", "--strict"]).expect("parse scan");
        match cli.command {
            Commands::Scan {
                strict,
                min_size,
                symlink_only,
                output,
            } => {
                assert!(strict);
                assert_eq!(min_size, None);
                assert!(!symlink_only);
                assert!(!output.json);
                assert_eq!(output.report, None);
                assert_eq!(output.progress, ProgressMode::Auto);
                assert!(!output.no_progress);
                assert!(!output.tui_events);
            }
            _ => panic!("expected scan command"),
        }
    }

    #[test]
    fn parses_link_command() {
        let cli = Cli::try_parse_from([
            "dejaview",
            "link",
            "--json",
            "--report",
            "/tmp/report.json",
            "--no-progress",
        ])
        .expect("parse link");
        match cli.command {
            Commands::Link {
                strict,
                confirm,
                min_size,
                symlink_only,
                only_sources,
                plan,
                output,
            } => {
                assert!(!strict);
                assert_eq!(plan, None);
                assert_eq!(min_size, None);
                assert!(!confirm);
                assert!(!symlink_only);
                assert!(only_sources.is_empty());
                assert!(output.json);
                assert_eq!(output.report, Some(PathBuf::from("/tmp/report.json")));
                assert_eq!(output.progress, ProgressMode::Auto);
                assert!(output.no_progress);
                assert!(!output.tui_events);
            }
            _ => panic!("expected link command"),
        }
    }

    #[test]
    fn parses_scan_progress_mode() {
        let cli = Cli::try_parse_from(["dejaview", "scan", "--progress", "off"])
            .expect("parse scan progress");
        match cli.command {
            Commands::Scan { output, .. } => assert_eq!(output.progress, ProgressMode::Off),
            _ => panic!("expected scan command"),
        }
    }

    #[test]
    fn parses_config_show_command() {
        let cli = Cli::try_parse_from(["dejaview", "config", "show"]).expect("parse config show");
        match cli.command {
            Commands::Config(ConfigArgs {
                command: ConfigCommands::Show,
            }) => {}
            _ => panic!("expected config show command"),
        }
    }

    #[test]
    fn parses_config_set_min_age_command() {
        let cli = Cli::try_parse_from(["dejaview", "config", "set-min-age", "120"])
            .expect("parse config set-min-age");
        match cli.command {
            Commands::Config(ConfigArgs {
                command: ConfigCommands::SetMinAge { seconds },
            }) => assert_eq!(seconds, 120),
            _ => panic!("expected config set-min-age command"),
        }
    }

    #[test]
    fn min_size_of_zero_is_refused() {
        assert!(Cli::try_parse_from(["dejaview", "scan", "--min-size", "0"]).is_err());
        assert!(Cli::try_parse_from(["dejaview", "config", "set-min-size", "0"]).is_err());
        let cli = Cli::try_parse_from(["dejaview", "link", "--min-size", "4096"])
            .expect("parse link --min-size");
        match cli.command {
            Commands::Link { min_size, .. } => assert_eq!(min_size, Some(4096)),
            _ => panic!("expected link command"),
        }
    }

    #[test]
    fn parses_link_confirm_flag() {
        let cli = Cli::try_parse_from(["dejaview", "link", "--confirm"]).expect("parse link");
        match cli.command {
            Commands::Link { confirm, .. } => assert!(confirm),
            _ => panic!("expected link command"),
        }
    }

    #[test]
    fn parses_cache_info_command() {
        let cli = Cli::try_parse_from(["dejaview", "cache", "info"]).expect("parse cache info");
        match cli.command {
            Commands::Cache(CacheArgs {
                command: CacheCommands::Info,
            }) => {}
            _ => panic!("expected cache info command"),
        }
    }

    #[test]
    fn parses_config_set_link_mode_command() {
        let cli = Cli::try_parse_from(["dejaview", "config", "set-link-mode", "symlink-only"])
            .expect("parse config set-link-mode");
        match cli.command {
            Commands::Config(ConfigArgs {
                command: ConfigCommands::SetLinkMode { mode },
            }) => assert_eq!(mode, LinkModeArg::SymlinkOnly),
            _ => panic!("expected config set-link-mode command"),
        }
    }

    #[test]
    fn parses_cache_cleanup_temp_command() {
        let cli = Cli::try_parse_from(["dejaview", "cache", "cleanup-temp"])
            .expect("parse cache cleanup-temp");
        match cli.command {
            Commands::Cache(CacheArgs {
                command: CacheCommands::CleanupTemp,
            }) => {}
            _ => panic!("expected cache cleanup-temp command"),
        }
    }

    #[test]
    fn parses_tui_command() {
        let cli = Cli::try_parse_from(["dejaview", "tui", "--strict"]).expect("parse tui");
        match cli.command {
            Commands::Tui { strict } => assert!(strict),
            _ => panic!("expected tui command"),
        }
    }

    #[test]
    fn parses_tui_logo_preview_command() {
        let cli =
            Cli::try_parse_from(["dejaview", "tui-logo-preview"]).expect("parse logo preview");
        match cli.command {
            Commands::TuiLogoPreview => {}
            _ => panic!("expected logo preview command"),
        }
    }

    #[test]
    fn probe_directory_writable_does_not_follow_existing_symlink() {
        #[cfg(not(unix))]
        {
            return;
        }

        #[cfg(unix)]
        {
            let root = unique_temp_dir("probe_symlink");
            fs::create_dir_all(&root).expect("create root");
            let victim = root.join("victim");
            fs::write(&victim, b"keep").expect("write victim");
            let link = root.join(format!(".dejaview_validate_{}.{}", std::process::id(), 1u8));
            symlink(&victim, &link).expect("create symlink");

            for _ in 0..16 {
                probe_directory_writable(&root).expect("probe should succeed");
            }

            assert_eq!(fs::read(&victim).expect("read victim"), b"keep");
            let _ = fs::remove_dir_all(&root);
        }
    }
}
