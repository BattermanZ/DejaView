use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use colored::*;
use std::io::IsTerminal;
use std::path::PathBuf;

mod config;
mod engine;

use config::{load_config, save_config};

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
        #[command(flatten)]
        output: RunOutputArgs,
    },
    /// Replace duplicates with hardlinks when possible, else symlinks
    Link {
        /// Fail if any scan/hash/link errors occur
        #[arg(long)]
        strict: bool,
        #[command(flatten)]
        output: RunOutputArgs,
    },
    /// Manage the configuration
    Config(ConfigArgs),
    /// Manage the cache
    Cache(CacheArgs),
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
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum ProgressMode {
    Auto,
    On,
    Off,
}

fn progress_enabled(mode: ProgressMode, no_progress: bool, json: bool) -> bool {
    if no_progress || json {
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = load_config()?;

    if let Some(threads) = config.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }

    match cli.command {
        Commands::Scan { strict, output } => engine::run_scan_with_options(
            &config,
            engine::ScanRunOptions {
                dry_run: true,
                strict,
                report_path: output.report.as_deref(),
                json_output: output.json,
                show_progress: progress_enabled(output.progress, output.no_progress, output.json),
            },
        )?,
        Commands::Link { strict, output } => engine::run_scan_with_options(
            &config,
            engine::ScanRunOptions {
                dry_run: false,
                strict,
                report_path: output.report.as_deref(),
                json_output: output.json,
                show_progress: progress_enabled(output.progress, output.no_progress, output.json),
            },
        )?,
        Commands::Config(args) => match args.command {
            ConfigCommands::SetDownloads { path } => {
                config.downloads = Some(path);
                save_config(&config)?;
                println!("{} Downloads path set.", "✔".green());
            }
            ConfigCommands::SetMedia { path } => {
                config.media = Some(path);
                save_config(&config)?;
                println!("{} Media path set.", "✔".green());
            }
            ConfigCommands::Show => {
                println!("{}", serde_json::to_string_pretty(&config)?);
            }
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
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scan_command() {
        let cli = Cli::try_parse_from(["dejaview", "scan", "--strict"]).expect("parse scan");
        match cli.command {
            Commands::Scan { strict, output } => {
                assert!(strict);
                assert!(!output.json);
                assert_eq!(output.report, None);
                assert_eq!(output.progress, ProgressMode::Auto);
                assert!(!output.no_progress);
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
            Commands::Link { strict, output } => {
                assert!(!strict);
                assert!(output.json);
                assert_eq!(output.report, Some(PathBuf::from("/tmp/report.json")));
                assert_eq!(output.progress, ProgressMode::Auto);
                assert!(output.no_progress);
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
    fn parses_cache_info_command() {
        let cli = Cli::try_parse_from(["dejaview", "cache", "info"]).expect("parse cache info");
        match cli.command {
            Commands::Cache(CacheArgs {
                command: CacheCommands::Info,
            }) => {}
            _ => panic!("expected cache info command"),
        }
    }
}
