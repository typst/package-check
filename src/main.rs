use clap::Parser;
use tracing_subscriber::EnvFilter;

mod action;
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
    /// Check a local package at the specified version. To be run in
    /// typst/packages/packages or your own repository.
    Check {
        /// Packages to check. Either the name of a directory with a typst.toml
        /// manifest (to run in your own repository), or a package specification
        /// in the @preview/name:version format (to run in the packages
        /// directory of typst/packages).
        packages: Vec<String>,

        /// Whether to output diagnostics in JSON.
        #[clap(long, default_value_t = false)]
        json: bool,
    },
    TypstVersion,
    /// Check the any modified package, and report the results as a GitHub check.
    ///
    /// This command assumes to be run in GitHub Action and to have access to some
    /// GitHub specific environment variables. It is only meant to be used to lint
    /// PRs submitted to typst/packages, the `check` subcommand is more suitable to
    /// use in CI for your own repositories.
    Action,
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
        Commands::Check { packages, json } => {
            if packages.is_empty() {
                cli::main(".".into(), json).await
            }

            for package in packages {
                cli::main(package, json).await
            }
        }
        Commands::TypstVersion => {
            println!("0.14.0")
        }
        Commands::Action => action::main().await,
    }
}
