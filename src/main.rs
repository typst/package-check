use clap::Parser;
use tracing_subscriber::EnvFilter;

mod check;
mod cli;
mod github;
mod package;
mod world;

#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Clone)]
enum Commands {
    /// Start a server to handle GitHub webhooks and report checks in pull
    /// requests.
    Server,
    /// Check a local package at the specified version. To be run in
    /// typst/packages/packages or your own repository.
    Check {
        /// Packages to check. Either the name of a directory with a typst.toml
        /// manifest (to run in your own repository), or a package specification
        /// in the @preview/name:version format (to run in the packages
        /// directory of typst/packages).
        packages: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    if std::env::var("LOG_STYLE").as_deref().unwrap_or("human") == "json" {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .event_format(tracing_subscriber::fmt::format::json())
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .init();
    }

    let args = Cli::parse();
    match args.command {
        Commands::Server => github::hook_server().await,
        Commands::Check { packages } => {
            if packages.is_empty() {
                cli::main(".".into()).await
            }

            for package in packages {
                cli::main(package).await
            }
        }
    }
}
