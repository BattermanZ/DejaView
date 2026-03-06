use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::*;
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
    Scan,
    /// Replace duplicates with symlinks
    Link,
    /// Manage the configuration
    Config(ConfigArgs),
    /// Manage the cache
    Cache(CacheArgs),
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
        Commands::Scan => engine::run_scan(&config, true)?,
        Commands::Link => engine::run_scan(&config, false)?,
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
