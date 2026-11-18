use std::env;
use std::ops::DerefMut;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use typst::syntax::package::PackageSpec;
use typst_package_check::check::codes::{Code, filter_diagnostics};
use typst_package_check::check::{Exclude, TryExt, all_checks};
use typst_package_check::github::api::pr::PullRequestEvent;
use typst_package_check::github::api::{Installation, Repository};
use typst_package_check::github::{AppState, run_github_check};
use typst_package_check::workspace::{SystemPackages, Workspace};
use typst_package_check::{cli, json};

#[derive(clap::Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Clone)]
enum Commands {
    /// Check a local package at the specified version. To be run in
    /// typst/packages/packages or your own repository.
    Check(CheckCommand),
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

/// Check a local package at the specified version. To be run in
/// typst/packages/packages or your own repository.
#[derive(clap::Args, Clone)]
struct CheckCommand {
    /// Packages to check. Either the name of a directory with a typst.toml
    /// manifest (to run in your own repository), or a package specification
    /// in the @preview/name:version format (to run in the packages
    /// directory of typst/packages).
    packages: Vec<String>,

    /// Skip lints that require network access.
    #[clap(long)]
    offline: bool,

    /// Filter the list of printed diagnostics.
    #[clap(long, value_enum, default_value_t)]
    show: Show,

    /// Coloring.
    #[clap(long, value_enum, default_value_t)]
    color: clap::ColorChoice,

    /// Whether to generate JSON(L) output.
    ///
    /// The output is an object on one line per package with the following keys:
    ///   - "denied": [Diagnostic] // regular emitted diagnostics
    ///   - "allowed": [Diagnostic] // diagnostics that have been
    ///
    /// `Diagnostic` has the following keys:
    ///   - "kind": "error"|"warning"|"note"
    ///   - "code": str
    ///   - "message": str
    ///   - "hints": [str]?
    ///   - "location": Location?
    ///
    /// `Location` has the following keys:
    ///   - "file": str
    ///   - "from": LineCol
    ///   - "to": LineCol
    ///
    /// `LineCol` has the following keys:
    ///   - "line": int // 1-indexed
    ///   - "column": int  // 1-indexed character (not byte) count
    ///
    /// In case checking a package fails with a fatal error, another object with
    /// the following keys is written:
    ///   - "fatal": FatalError
    ///
    /// `FatalError` has the following keys:
    ///   - "code": str
    ///   - "message": str
    #[clap(long, verbatim_doc_comment)]
    json: bool,

    /// Don't exit immediately after one package emits a warning or error.
    #[clap(long)]
    keep_going: bool,
}

#[derive(clap::ValueEnum, Default, Copy, Clone, Eq, PartialEq)]
enum Show {
    /// These are the regular emitted diagnostics.
    #[default]
    Denied,
    /// These are the diagnostics that were supressed due to the
    /// `[tool.package-check.lint]` configuration.
    Allowed,
    /// Print all diagonstics, including ones that were allowed.
    All,
}

impl From<Show> for cli::Show {
    fn from(value: Show) -> Self {
        match value {
            Show::Denied => cli::Show::Denied,
            Show::Allowed => cli::Show::Allowed,
            Show::All => cli::Show::All,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
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
        Commands::Check(cmd) => return run_cli(cmd).await,
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

    ExitCode::SUCCESS
}

async fn run_cli(mut cmd: CheckCommand) -> ExitCode {
    if cmd.packages.is_empty() {
        cmd.packages.push(".".into());
    }

    let packages_repo_dir = PathBuf::from(
        std::env::var("PACKAGES_DIR")
            .or_else(|_| std::env::var("GITHUB_WORKSPACE"))
            .unwrap_or(".".to_owned()),
    );
    let workspace = Workspace::new(packages_repo_dir, SystemPackages::default());

    let color = match cmd.color {
        clap::ColorChoice::Auto => termcolor::ColorChoice::Auto,
        clap::ColorChoice::Always => termcolor::ColorChoice::Always,
        clap::ColorChoice::Never => termcolor::ColorChoice::Never,
    };
    let stdout = termcolor::BufferWriter::stdout(color);

    let mut exit_code: Option<u8> = None;
    for spec_or_path in cmd.packages.iter() {
        let package_spec: Option<PackageSpec> = spec_or_path.parse().ok();
        let package_dir = if let Some(ref package_spec) = package_spec {
            workspace.repo().path_of(package_spec)
        } else {
            PathBuf::from(spec_or_path)
        };

        if !cmd.json {
            let mut buf = stdout.buffer();
            let mut buf = typst_utils::defer(&mut buf, |buf| stdout.print(buf).unwrap());
            cli::write_header(buf.deref_mut(), &package_dir, cmd.show.into()).unwrap();
        }

        let mut buf = stdout.buffer();
        let mut buf = typst_utils::defer(&mut buf, |buf| stdout.print(buf).unwrap());
        let res = all_checks(&workspace, package_spec.as_ref(), package_dir, cmd.offline).await;
        match res {
            Ok((checks, mut diags)) => {
                let allowed = filter_diagnostics(&checks, &mut diags);

                // Remove the exclude, so reading excluded files doesn't error.
                // Otherwise printing some diagnostics might fail.
                let world = checks.worlds.package.exclude(Exclude::empty());

                if cmd.json {
                    json::write_output(buf.deref_mut(), &world, &diags, &allowed).unwrap();
                } else {
                    cli::write_output(buf.deref_mut(), &world, &diags, &allowed, cmd.show.into())
                        .unwrap();
                }

                if !diags.errors().is_empty() {
                    exit_code = Some(1);
                } else if !diags.warnings().is_empty() {
                    exit_code = exit_code.or(Some(2));
                }
            }
            Err(err) => {
                if cmd.json {
                    json::write_fatal_error(buf.deref_mut(), &err).unwrap();
                } else {
                    cli::write_fatal_error(buf.deref_mut(), &err).unwrap();
                }
                exit_code = Some(1);
            }
        }

        if !cmd.keep_going
            && let Some(code) = exit_code
        {
            return ExitCode::from(code);
        }
    }

    if let Some(code) = exit_code {
        return ExitCode::from(code);
    }

    ExitCode::SUCCESS
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
    .error(Code::GithubActionsEventIo, "Failed to read event metadata")
    .unwrap();
    let event: PullRequestEvent = serde_json::from_str(&event)
        .error(Code::GithubActionsEventInvalid, "Invalid event JSON")
        .unwrap();

    run_github_check(
        &state.workspace,
        event.pull_request.head.sha.clone(),
        api_client,
        repository,
        None,
        Some(event.pull_request),
    )
    .await
    .unwrap();
}
