use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use axum::{
    body::Body,
    extract::State,
    http::{Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use codespan_reporting::{
    diagnostic::{Diagnostic, Severity},
    files::Files,
};
use eyre::Context;
use hook::CheckRunPayload;
use jwt_simple::prelude::*;
use tracing::{debug, info, warn};
use typst::syntax::{package::PackageSpec, FileId};

use crate::{check, world::SystemWorld};

use api::{
    check::{CheckRun, CheckRunAction},
    *,
};

mod api;
pub mod git;

use self::{
    api::check::{Annotation, AnnotationLevel, CheckRunOutput, CheckSuite, CheckSuiteAction},
    git::GitRepo,
    hook::{CheckSuitePayload, HookPayload},
};

/// Application configuration, read from .env file.
#[derive(Clone)]
struct AppState {
    webhook_secret: Vec<u8>,
    private_key: String,
    app_id: String,
    git_dir: String,
}

/// Runs an HTTP server to handle GitHub hooks
pub async fn hook_server() {
    let state = AppState {
        webhook_secret: std::env::var("GITHUB_WEBHOOK_SECRET")
            .expect("GITHUB_WEBHOOK_SECRET is not set.")
            .into_bytes(),
        private_key: std::env::var("GITHUB_PRIVATE_KEY")
            .expect("GITHUB_PRIVATE_KEY is not set.")
            .replace('&', "\n"),
        app_id: std::env::var("GITHUB_APP_IDENTIFIER").expect("GITHUB_APP_IDENTIFIER is not set."),
        git_dir: std::env::var("PACKAGES_DIR").expect("PACKAGES_DIR is not set."),
    };

    GitRepo::open(Path::new(&state.git_dir[..]))
        .clone_if_needed("https://github.com/typst/packages.git")
        .await
        .expect("Can't clone the packages repository");

    let app = Router::new()
        .route("/", get(index))
        .route("/github-hook", post(github_hook))
        .route("/force-review/:install/:sha", get(force))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    info!("Starting…");
    let listener = tokio::net::TcpListener::bind("0.0.0.0:7878")
        .await
        .expect("Can't listen on 0.0.0.0:7878");
    axum::serve(listener, app).await.expect("Server error");
}

/// The page served on `/`, just to check that everything runs properly.
async fn index() -> &'static str {
    "typst-package-check is running"
}

async fn force(
    state: State<AppState>,
    api_client: GitHub,
    axum::extract::Path((install, sha)): axum::extract::Path<(String, String)>,
) -> Result<&'static str, &'static str> {
    github_hook(
        state,
        api_client,
        HookPayload::CheckSuite(CheckSuitePayload {
            action: CheckSuiteAction::Requested,
            installation: Installation {
                id: str::parse(&install).map_err(|_| "Invalid installation ID")?,
            },
            repository: Repository::new("typst/packages").map_err(|e| {
                debug!("{}", e);
                "Invalid repository path"
            })?,
            check_suite: CheckSuite { head_sha: sha },
        }),
    )
    .await
    .map_err(|e| {
        debug!("Error: {:?}", e);
        "Error in the GitHub hook handler"
    })?;

    Ok("OK!")
}

/// The route to handle GitHub hooks. Mounted on `/github-hook`.
async fn github_hook(
    State(state): State<AppState>,
    mut api_client: GitHub,
    payload: HookPayload,
) -> Result<(), WebError> {
    debug!("GitHub hook was triggered");
    api_client.auth_installation(&payload).await?;
    debug!("Successfully authenticated application");

    let (head_sha, repository, previous_check_run) = match payload {
        HookPayload::CheckSuite(CheckSuitePayload {
            action: CheckSuiteAction::Requested | CheckSuiteAction::Rerequested,
            repository,
            check_suite,
            ..
        }) => (check_suite.head_sha, repository, None),
        HookPayload::CheckRun(CheckRunPayload {
            action: CheckRunAction::Rerequested,
            repository,
            check_run,
            ..
        }) => (
            check_run.check_suite.head_sha.clone(),
            repository,
            Some(check_run),
        ),
        HookPayload::CheckRun(_) => return Ok(()),
        _ => return Err(WebError::UnexpectedEvent),
    };

    debug!("Starting checks for {}", head_sha);
    tokio::spawn(async move {
        async fn inner(
            state: AppState,
            head_sha: String,
            api_client: GitHub,
            repository: Repository,
            previous_check_run: Option<CheckRun>,
        ) -> eyre::Result<()> {
            let git_repo = GitRepo::open(Path::new(&state.git_dir));
            git_repo.pull_main().await?;
            git_repo.fetch_commit(&head_sha).await?;
            let touched_files = git_repo.files_touched_by(&head_sha).await?;

            let mut touches_outside_of_packages = false;

            let touched_packages = touched_files
                .into_iter()
                .filter_map(|line| {
                    let mut components = line.components();
                    if components.next()?.as_os_str() != OsStr::new("packages") {
                        touches_outside_of_packages = true;
                        return None;
                    }

                    let namespace = components.next()?.as_os_str().to_str()?.into();
                    let name = components.next()?.as_os_str().to_str()?.into();
                    let version = components.next()?.as_os_str().to_str()?.parse().ok()?;
                    Some(PackageSpec {
                        namespace,
                        name,
                        version,
                    })
                })
                .collect::<HashSet<_>>();

            for ref package in touched_packages {
                let check_run_name = format!(
                    "@{}/{}:{}",
                    package.namespace, package.name, package.version
                );

                let check_run = if let Some(previous) = previous_check_run
                    .as_ref()
                    .filter(|p| p.name == check_run_name)
                {
                    previous.clone().without_suite()
                } else {
                    api_client
                        .create_check_run(
                            repository.owner(),
                            repository.name(),
                            check_run_name,
                            &head_sha,
                        )
                        .await
                        .context("Failed to create a new check run")?
                        .without_suite()
                };

                if touches_outside_of_packages {
                    api_client.update_check_run(
                        repository.owner(),
                        repository.name(),
                        check_run.id,
                        false,
                        CheckRunOutput {
                            title: "This PR does too many things",
                            summary: "A PR should either change packages/, or the rest of the repository, but not both.",
                            annotations: &[],
                        },
                    ).await
                    .context("Failed to cancel a check run because the branch does too many things")?;
                    continue;
                }

                let checkout_dir = format!("checkout-{}", head_sha);
                git_repo
                    .checkout_commit(&head_sha, &checkout_dir)
                    .await
                    .context("Failed to checkout commit")?;

                let (world, diags) = match check::all_checks(
                    Some(package),
                    PathBuf::new()
                        .join(&checkout_dir)
                        .join("packages")
                        .join(package.namespace.as_str())
                        .join(package.name.as_str())
                        .join(package.version.to_string()),
                )
                .await
                {
                    Ok(x) => x,
                    Err(e) => {
                        api_client
                            .update_check_run(
                                repository.owner(),
                                repository.name(),
                                check_run.id,
                                false,
                                CheckRunOutput {
                                    title: "Fatal error",
                                    summary: &format!(
                                        "The following error was encountered:\n\n{}",
                                        e
                                    ),
                                    annotations: &[],
                                },
                            )
                            .await
                            .context("Failed to report fatal error")?;
                        return Err(e);
                    }
                };

                let plural = |n| if n == 1 { "" } else { "s" };

                api_client
                    .update_check_run(
                        repository.owner(),
                        repository.name(),
                        check_run.id,
                        diags.errors().is_empty() && diags.warnings().is_empty(),
                        CheckRunOutput {
                            title: &if !diags.errors().is_empty() {
                                if diags.warnings().is_empty() {
                                    format!(
                                        "{} error{}",
                                        diags.errors().len(),
                                        plural(diags.errors().len())
                                    )
                                } else {
                                    format!(
                                        "{} error{}, {} warning{}",
                                        diags.errors().len(),
                                        plural(diags.errors().len()),
                                        diags.warnings().len(),
                                        plural(diags.warnings().len())
                                    )
                                }
                            } else if diags.warnings().is_empty() {
                                "All good!".to_owned()
                            } else {
                                format!(
                                    "{} warning{}",
                                    diags.warnings().len(),
                                    plural(diags.warnings().len())
                                )
                            },
                            summary: &format!(
                                "Our bots have automatically run some checks on your packages. \
                                They found {} error{} and {} warning{}.\n\n\
                                Warnings are suggestions, your package can still be accepted even \
                                if you prefer not to fix them.\n\n\
                                A human being will soon review your package, too.",
                                diags.errors().len(),
                                plural(diags.errors().len()),
                                diags.warnings().len(),
                                plural(diags.warnings().len()),
                            ),
                            annotations: &diags
                                .errors()
                                .iter()
                                .chain(diags.warnings())
                                .filter_map(|diag| diagnostic_to_annotation(&world, package, diag))
                                .collect::<Vec<_>>(),
                        },
                    )
                    .await
                    .context("Failed to send report")?;

                tokio::fs::remove_dir_all(checkout_dir).await?;
            }

            Ok(())
        }

        if let Err(e) = inner(state, head_sha, api_client, repository, previous_check_run).await {
            warn!("Error in hook handler: {:#}", e)
        }
    });

    Ok(())
}

fn diagnostic_to_annotation(
    world: &SystemWorld,
    package: &PackageSpec,
    diag: &Diagnostic<FileId>,
) -> Option<Annotation> {
    let label = diag.labels.first()?;
    let start_line = world.line_index(label.file_id, label.range.start).ok()?;
    let end_line = world.line_index(label.file_id, label.range.end).ok()?;
    let (start_column, end_column) = if start_line == end_line {
        let start = world
            .column_number(label.file_id, start_line, label.range.start)
            .ok();
        let end = world
            .column_number(label.file_id, start_line, label.range.end)
            .ok();
        (start, end)
    } else {
        (None, None)
    };
    Some(Annotation {
        path: Path::new("packages")
            .join(package.namespace.to_string())
            .join(package.name.to_string())
            .join(package.version.to_string())
            .join(label.file_id.vpath().as_rootless_path())
            .to_str()?
            .to_owned(),
        // Lines are 1-indexed on GitHub but not for codespan
        start_line: start_line + 1,
        end_line: end_line + 1,
        start_column,
        end_column,
        annotation_level: if diag.severity == Severity::Warning {
            AnnotationLevel::Warning
        } else {
            AnnotationLevel::Failure
        },
        message: diag.message.clone(),
    })
}

#[derive(Debug)]
enum WebError {
    #[allow(dead_code)]
    Api(ApiError),
    UnexpectedEvent,
}

impl IntoResponse for WebError {
    fn into_response(self) -> axum::response::Response {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!("{:?}", self)))
            .expect("Can't build error response")
    }
}

impl From<ApiError> for WebError {
    fn from(value: ApiError) -> Self {
        WebError::Api(value)
    }
}
