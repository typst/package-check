use std::env;
use std::path::PathBuf;

use clap::Parser;
use tracing::error;
use tracing_subscriber::EnvFilter;
use typst::syntax::package::PackageSpec;
use typst_package_check::check::{TryExt, all_checks};
use typst_package_check::cli;
use typst_package_check::github::api::pr::PullRequestEvent;
use typst_package_check::github::api::{Installation, Repository};
use typst_package_check::github::{AppState, run_github_check};
use typst_package_check::package::PackageExt;

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

        /// Skip lints that require network access.
        #[clap(long, default_value_t = false)]
        offline: bool,
    },
    /// Output the version of Typst bundled with this application.
    TypstVersion,
    /// Output the version of this application.
    Version,
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
    if env::var("IGNORE_DOTENV").is_err() {
        dotenvy::dotenv().ok();
    }

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
        Commands::Check {
            packages,
            json,
            offline,
        } => {
            if packages.is_empty() {
                run_cli(".".into(), json, offline).await
            }

            for package in packages {
                run_cli(package, json, offline).await
            }
        }
        Commands::TypstVersion => {
            let version = typst_utils::version();
            println!(
                "{}.{}.{}",
                version.major(),
                version.minor(),
                version.patch()
            )
        }
        Commands::Version => {
            println!("{}", option_env!("CARGO_PKG_VERSION").unwrap_or("n/a"))
        }
        Commands::Action => run_action().await,
    }
}

async fn run_cli(spec_or_path: String, json_output: bool, offline: bool) {
    let package_spec: Option<PackageSpec> = spec_or_path.parse().ok();
    let package_dir = if let Some(ref package_spec) = package_spec {
        package_spec.path_in_git_repo()
    } else {
        PathBuf::from(spec_or_path)
    };

    match all_checks(package_spec.as_ref(), package_dir, true, offline).await {
        Ok((world, diags)) => {
            if let Err(err) =
                cli::print_diagnostics(world, diags.errors(), diags.warnings(), json_output)
            {
                error!("failed to print diagnostics ({err})");
                error!(
                    "Raw diagnostics: {:#?}\n{:#?}",
                    diags.errors(),
                    diags.warnings()
                );
            }

            if !diags.errors().is_empty() {
                std::process::exit(1)
            }

            if !diags.warnings().is_empty() {
                std::process::exit(2)
            }
        }
        Err(e) => {
            println!("Fatal error: {}", e.message);
            std::process::exit(1)
        }
    }
}

pub async fn run_action() {
    let state = AppState::read();

    let api_client = state
        .as_github_api()
        .unwrap()
        .auth_installation(&Installation {
            id: std::env::var("GITHUB_INSTALLATION")
                .expect("GITHUB_INSTALLATION should be set")
                .parse()
                .expect("GITHUB_INSTALLATION should be a valid installation ID"),
        })
        .await
        .unwrap();

    let repository =
        Repository::new(&std::env::var("GITHUB_REPOSITORY").unwrap_or("typst/packages".to_owned()))
            .unwrap();

    let event = tokio::fs::read_to_string(
        std::env::var("GITHUB_EVENT_PATH").expect("This command should be run in GitHub Actions"),
    )
    .await
    .error("github/actions/event", "Failed to read event metadata")
    .unwrap();
    let event: PullRequestEvent = serde_json::from_str(&event)
        .error("github/actions/event/invalid", "Invalid event JSON")
        .unwrap();

    run_github_check(
        &state.git_dir,
        event.pull_request.head.sha.clone(),
        api_client,
        repository,
        None,
        Some(event.pull_request),
    )
    .await
    .unwrap();
}
