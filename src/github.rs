use std::{
    fmt::Display,
    path::{Path, PathBuf},
};

use axum::{
    body::Body,
    extract::{FromRequest, FromRequestParts, Request, State},
    http::{request::Parts, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use codespan_reporting::{diagnostic::Severity, files::Files};
use hmac::Mac;
use jwt_simple::prelude::*;
use reqwest::RequestBuilder;
use serde::Deserialize;
use tokio::process::Command;
use tracing::{debug, info, warn};
use typst::syntax::package::PackageSpec;

use crate::check;

pub async fn main() {
    let app = Router::new()
        .route("/", get(index))
        .route("/github-hook", post(github_hook))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(AppState {
            webhook_secret: std::env::var("GITHUB_WEBHOOK_SECRET").unwrap().into_bytes(),
            private_key: std::env::var("GITHUB_PRIVATE_KEY").unwrap(),
            app_id: std::env::var("GITHUB_APP_IDENTIFIER").unwrap(),
            git_dir: std::env::var("PACKAGES_DIR").unwrap(),
        });

    info!("Starting…");
    let listener = tokio::net::TcpListener::bind("0.0.0.0:7878").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> &'static str {
    "typst-package-check is running"
}

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
        Command::new("/usr/bin/git")
            .args(&["-C", &state.git_dir, "fetch", "origin", &head_sha])
            .spawn()
            .unwrap()
            .wait()
            .await
            .unwrap();

        Command::new("/usr/bin/git")
            .args(&["-C", &state.git_dir, "checkout", &head_sha])
            .spawn()
            .unwrap()
            .wait()
            .await
            .unwrap();

        let touched_files = String::from_utf8(
            Command::new("git")
                .args(&[
                    "-C",
                    &state.git_dir,
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    &head_sha,
                    "main",
                ])
                .output()
                .await
                .unwrap()
                .stdout,
        )
        .unwrap();

        let touched_packages = touched_files
            .lines()
            .filter_map(|line| {
                let (_, path) = line.split_once("packages/preview/")?;
                let mut components = path.split('/');
                let name = components.next()?;
                let version = components.next()?;
                let version = version.parse().ok()?;
                Some((name, version))
            })
            .collect::<HashSet<_>>();

        for (name, version) in touched_packages {
            let (world, diags) = check::all_checks(
                PathBuf::new().join(&state.git_dir).join("packages"),
                PackageSpec {
                    namespace: "preview".into(),
                    name: name.into(),
                    version,
                },
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
                            .filter_map(|diag| {
                                dbg!(&diag);
                                let label = diag.labels.get(0)?;
                                dbg!(label);
                                let start_line =
                                    world.line_index(label.file_id, label.range.start).ok()?;
                                let end_line =
                                    world.line_index(label.file_id, label.range.end).ok()?;
                                dbg!(start_line, end_line);
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
                                dbg!(start_column, end_column);
                                Some(Annotation {
                                    path: Path::new("packages")
                                        .join("preview")
                                        .join(name)
                                        .join(version.to_string())
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
                            })
                            .collect::<Vec<_>>(),
                    },
                )
                .await
                .unwrap();
        }
    });

    Ok(())
}

enum HookPayload {
    Installation(InstallationPayload),
    CheckSuite(CheckSuitePayload),
    CheckRun(CheckRunPayload),
}

impl HookPayload {
    fn installation(&self) -> &Installation {
        match self {
            HookPayload::CheckSuite(cs) => &cs.installation,
            HookPayload::Installation(i) => &i.installation,
            HookPayload::CheckRun(cr) => &cr.installation,
        }
    }
}

#[derive(Deserialize)]
struct InstallationPayload {
    installation: Installation,
}

#[derive(Deserialize)]
struct CheckSuitePayload {
    action: CheckSuiteAction,
    installation: Installation,
    repository: Repository,
    check_suite: CheckSuite,
}

#[derive(Deserialize)]
struct CheckRunPayload {
    installation: Installation,
}

#[derive(Deserialize)]
struct CheckSuite {
    head_sha: String,
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
}

impl Repository {
    fn owner(&self) -> OwnerId {
        OwnerId(self.full_name.split_once('/').unwrap().0.to_owned())
    }

    fn name(&self) -> RepoId {
        RepoId(self.full_name.split_once('/').unwrap().1.to_owned())
    }
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CheckSuiteAction {
    // TODO: update this doc comment
    /// App was installed by a new user/org
    Created,
    /// A check suite was requested (when code is pushed)
    Requested,
    /// A check suite was re-requested (when re-running on code that was previously pushed)
    Rerequested,
    /// A check suite has finished running
    Completed,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CheckRunAction {
    Created,
    RequestedAction,
    Rerequested,
    Completed,
}

#[derive(Deserialize)]
struct InstallationToken {
    token: String,
}

#[derive(Clone)]
struct AppState {
    webhook_secret: Vec<u8>,
    private_key: String,
    app_id: String,
    git_dir: String,
}

#[async_trait::async_trait]
impl FromRequest<AppState> for HookPayload {
    type Rejection = (StatusCode, &'static str);

    async fn from_request<'s>(req: Request, state: &'s AppState) -> Result<Self, Self::Rejection> {
        let Some(their_signature_header) = req.headers().get("X-Hub-Signature") else {
            return Err((StatusCode::UNAUTHORIZED, "X-Hub-Signature is missing"));
        };
        let event_type = req
            .headers()
            .get("X-GitHub-Event")
            .map(|v| v.as_bytes().to_owned());
        let their_signature_header = their_signature_header
            .to_str()
            .unwrap_or_default()
            .to_owned();
        let raw_payload = String::from_request(req, state).await.unwrap();
        let (method, their_digest) = their_signature_header.split_once('=').unwrap();

        if method != "sha1" {
            warn!(
                "A hook with a {} signature was received, and rejected",
                method
            );
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unsupported signature type",
            ));
        }

        let our_digest = {
            let mut mac = hmac::Hmac::<sha1::Sha1>::new_from_slice(&state.webhook_secret).unwrap();
            mac.update(raw_payload.as_bytes());
            mac
        };
        let parsed_digest: Vec<_> = (0..their_digest.len() / 2)
            .filter_map(|idx| {
                let slice = &their_digest[idx * 2..idx * 2 + 2];
                u8::from_str_radix(slice, 16).ok()
            })
            .collect();
        if our_digest.verify_slice(&parsed_digest).is_err() {
            debug!("Invalid hook signature");
            return Err((StatusCode::UNAUTHORIZED, "Invalid hook signature"));
        }

        match event_type.as_deref() {
            Some(b"installation") => Ok(HookPayload::Installation(
                serde_json::from_str(&raw_payload).unwrap(),
            )),
            Some(b"check_suite") => Ok(HookPayload::CheckSuite(
                serde_json::from_str(&raw_payload).unwrap(),
            )),
            Some(b"check_run") => Ok(HookPayload::CheckRun(
                serde_json::from_str(&raw_payload).unwrap(),
            )),
            Some(x) => {
                debug!(
                    "Uknown event type: {}",
                    std::str::from_utf8(x).unwrap_or("ERR")
                );
                debug!("Payload was: {}", raw_payload);
                Err((StatusCode::BAD_REQUEST, "Unknown event type"))
            }
            None => Err((StatusCode::BAD_REQUEST, "Unspecified event type")),
        }
    }
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

#[derive(Debug)]
enum ApiError {
    #[allow(dead_code)]
    Reqwest(reqwest::Error),
}

impl From<reqwest::Error> for ApiError {
    fn from(value: reqwest::Error) -> Self {
        ApiError::Reqwest(value)
    }
}

type ApiResult<T> = Result<T, ApiError>;

struct GitHub {
    jwt: String,
    req: reqwest::Client,
}

struct OwnerId(String);
struct RepoId(String);

#[derive(Deserialize, Clone, Copy)]
#[serde(transparent)]
struct CheckRunId(u64);

impl Display for OwnerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Display for CheckRunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Deserialize)]
struct CheckRun {
    id: CheckRunId,
}

#[derive(Debug, Serialize)]
struct CheckRunOutput<'a> {
    title: &'a str,
    summary: &'a str,
    annotations: &'a [Annotation],
}

#[derive(Debug, Serialize)]
struct Annotation {
    path: String,
    start_line: usize,
    end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_column: Option<usize>,
    annotation_level: AnnotationLevel,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum AnnotationLevel {
    Warning,
    Failure,
}

impl GitHub {
    async fn auth_installation(&mut self, payload: &HookPayload) -> ApiResult<()> {
        let installation = &payload.installation().id;
        let res = self
            .post(format!("app/installations/{installation}/access_tokens"))
            .text()
            .await?;
        let res: InstallationToken = serde_json::from_str(&res).unwrap();
        self.jwt = res.token;

        Ok(())
    }

    async fn create_check_run(
        &self,
        owner: OwnerId,
        repo: RepoId,
        check_run_name: String,
        head_sha: String,
    ) -> ApiResult<CheckRun> {
        let result = self
            .post(format!("repos/{owner}/{repo}/check-runs"))
            .body(
                serde_json::to_string(&serde_json::json!({
                    "name": check_run_name,
                    "head_sha": head_sha,
                    "status": "in_progress",
                }))
                .unwrap(),
            )
            .send()
            .await?
            .json()
            .await?;
        Ok(result)
    }

    async fn update_check_run<'a>(
        &self,
        owner: OwnerId,
        repo: RepoId,
        check_run: CheckRunId,
        success: bool,
        output: CheckRunOutput<'a>,
    ) -> ApiResult<()> {
        dbg!(&output);
        let res = self
            .patch(format!("repos/{owner}/{repo}/check-runs/{check_run}"))
            .body(
                serde_json::to_string(&serde_json::json!({
                    "status": "completed",
                    "conclusion": if success { "success" } else { "failure" },
                    "output": output,
                }))
                .unwrap(),
            )
            .send()
            .await?
            .text()
            .await?;
        debug!("GitHub said: {}", res);
        Ok(())
    }

    fn patch(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.with_headers(self.req.patch(Self::url(url)))
    }

    fn post(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.with_headers(self.req.post(Self::url(url)))
    }

    fn with_headers(&self, req: RequestBuilder) -> RequestBuilder {
        req.bearer_auth(&self.jwt)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Typst package check")
    }

    fn url<S: AsRef<str>>(path: S) -> String {
        format!("https://api.github.com/{}", path.as_ref())
    }
}

#[async_trait::async_trait]
trait BodyOnly {
    async fn text(self) -> reqwest::Result<String>;
}

#[async_trait::async_trait]
impl BodyOnly for RequestBuilder {
    async fn text(self) -> reqwest::Result<String> {
        self.send().await?.text().await
    }
}

#[async_trait::async_trait]
impl FromRequestParts<AppState> for GitHub {
    type Rejection = StatusCode;

    async fn from_request_parts<'a, 's>(
        _parts: &'a mut Parts,
        state: &'s AppState,
    ) -> Result<Self, Self::Rejection> {
        let private_key = RS256KeyPair::from_pem(&state.private_key).unwrap();
        let claims = Claims::create(Duration::from_mins(10)).with_issuer(&state.app_id);
        let token = private_key.sign(claims).unwrap();

        Ok(Self {
            jwt: token,
            req: reqwest::Client::new(),
        })
    }
}
