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
use jwt_simple::prelude::*;
use tracing::info;
use typst::syntax::{package::PackageSpec, FileId};

use crate::check::{self, SystemWorld};

use api::*;

mod api;
mod git;

use self::{
    api::check::{Annotation, AnnotationLevel, CheckRunOutput, CheckSuiteAction},
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
#[tokio::main]
pub async fn hook_server() {
    let app = Router::new()
        .route("/", get(index))
        .route("/github-hook", post(github_hook))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(AppState {
            webhook_secret: std::env::var("GITHUB_WEBHOOK_SECRET")
                .expect("GITHUB_WEBHOOK_SECRET is not set.")
                .into_bytes(),
            private_key: std::env::var("GITHUB_PRIVATE_KEY")
                .expect("GITHUB_PRIVATE_KEY is not set."),
            app_id: std::env::var("GITHUB_APP_IDENTIFIER")
                .expect("GITHUB_APP_IDENTIFIER is not set."),
            git_dir: std::env::var("PACKAGES_DIR").expect("PACKAGES_DIR is not set."),
        });

    info!("Starting…");
    let listener = tokio::net::TcpListener::bind("0.0.0.0:7878").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// The page served on `/`, just to check that everything runs properly.
async fn index() -> &'static str {
    "typst-package-check is running"
}

/// The route to handle GitHub hooks. Mounted on `/github-hook`.
async fn github_hook(
    State(state): State<AppState>,
    mut api_client: GitHub,
    payload: HookPayload,
) -> Result<(), WebError> {
    api_client.auth_installation(&payload).await?;

    let (check_run, head_sha, repository) = match payload {
        HookPayload::CheckSuite(CheckSuitePayload {
            action: CheckSuiteAction::Requested | CheckSuiteAction::Rerequested,
            repository,
            check_suite,
            ..
        }) => {
            let check_run = api_client
                .create_check_run(
                    repository.owner(),
                    repository.name(),
                    "Automated package check".to_owned(),
                    check_suite.head_sha.clone(),
                )
                .await?;
            (check_run, check_suite.head_sha, repository)
        }
        HookPayload::CheckRun(_) => return Ok(()),
        _ => return Err(WebError::UnexpectedEvent),
    };

    tokio::spawn(async move {
        // TODO: test one package at a time
        let git_repo = GitRepo::open(Path::new(&state.git_dir));
        git_repo.fetch_commit(&head_sha).await?;
        git_repo.checkout_commit(&head_sha).await?;
        let touched_files = git_repo.files_touched_by(&head_sha).await?;

        let touched_packages = touched_files
            .into_iter()
            .filter_map(|line| {
                let mut components = line.components();
                if components.next()?.as_os_str() != OsStr::new("packages") {
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
            let (world, diags) = check::all_checks(
                PathBuf::new().join(&state.git_dir).join("packages"),
                package,
            );

            api_client
                .update_check_run(
                    repository.owner(),
                    repository.name(),
                    check_run.id,
                    diags.errors.is_empty() && diags.warnings.is_empty(),
                    CheckRunOutput {
                        title: "Automated report",
                        summary: &format!(
                            "{} errors, {} warnings.",
                            diags.errors.len(),
                            diags.warnings.len()
                        ),
                        annotations: &diags
                            .errors
                            .into_iter()
                            .chain(diags.warnings)
                            .filter_map(|diag| diagnostic_to_annotation(&world, package, diag))
                            .collect::<Vec<_>>(),
                    },
                )
                .await
                .unwrap();
        }
        Some(())
    });

    Ok(())
}

fn diagnostic_to_annotation(
    world: &SystemWorld,
    package: &PackageSpec,
    diag: Diagnostic<FileId>,
) -> Option<Annotation> {
    let label = diag.labels.get(0)?;
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
            .join(&package.namespace.to_string())
            .join(&package.name.to_string())
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
        message: diag.message,
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
            .unwrap()
    }
}

impl From<ApiError> for WebError {
    fn from(value: ApiError) -> Self {
        WebError::Api(value)
    }
}
