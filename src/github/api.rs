//! Interact with the GitHub REST API.

use std::fmt::Display;

use axum::{extract::FromRequestParts, http::request::Parts};
use check::MinimalCheckSuite;
use eyre::Error;
use jwt_simple::{
    algorithms::{RS256KeyPair, RSAKeyPairLike},
    claims::Claims,
    reexports::coarsetime::Duration,
};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use tracing::{debug, warn};

use self::check::{CheckRun, CheckRunId, CheckRunOutput};

use super::AppState;

pub mod check;
pub mod hook;
pub mod pr;

#[derive(Debug)]
pub enum ApiError {
    #[allow(dead_code)]
    Reqwest(reqwest::Error),
    Json(serde_json::Error),
    UnexpectedResponse(String),
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ApiError::Reqwest(e) => Some(e),
            ApiError::Json(e) => Some(e),
            ApiError::UnexpectedResponse(_) => None,
        }
    }
}

impl Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Reqwest(e) => write!(f, "Network error: {:?}", e),
            ApiError::Json(e) => write!(f, "JSON ser/de error: {:?}", e),
            ApiError::UnexpectedResponse(e) => write!(f, "Unexpected response: {:?}", e),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(value: reqwest::Error) -> Self {
        ApiError::Reqwest(value)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(value: serde_json::Error) -> Self {
        ApiError::Json(value)
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Authentication for the GitHub API using a JWT token.
pub struct AuthJwt(String);

impl Display for AuthJwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Authentication for the GitHub API using an installation token, that
/// is scoped to a specific organization or set of repositories, but that
/// can do more than a [`AuthJwt`] token.
pub struct AuthInstallation(String);

impl Display for AuthInstallation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A GitHub API client
pub struct GitHub<A = AuthJwt> {
    auth: A,
    req: reqwest::Client,
}

impl<A: ToString> GitHub<A> {
    fn get(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.with_headers(self.req.get(Self::url(url)))
    }

    fn patch(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.with_headers(self.req.patch(Self::url(url)))
    }

    fn post(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.with_headers(self.req.post(Self::url(url)))
    }

    fn with_headers(&self, req: RequestBuilder) -> RequestBuilder {
        req.bearer_auth(self.auth.to_string())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Typst package check")
    }

    fn url<S: AsRef<str>>(path: S) -> String {
        let u = format!("https://api.github.com/{}", path.as_ref());
        debug!("API URL: {}", u);
        u
    }
}

pub trait GitHubAuth {
    async fn auth_installation(
        self,
        installation: &impl AsInstallation,
    ) -> ApiResult<GitHub<AuthInstallation>>;
}

impl GitHubAuth for GitHub<AuthJwt> {
    #[tracing::instrument(skip_all)]
    async fn auth_installation(
        self,
        installation: &impl AsInstallation,
    ) -> ApiResult<GitHub<AuthInstallation>> {
        let installation_id = installation.id();
        let installation_token: InstallationToken = self
            .post(format!("app/installations/{installation_id}/access_tokens"))
            .json(&serde_json::json!({
                "repositories": ["packages"],
                "permissions": {
                    "metadata": "read",
                    "issues": "write",
                    "pull_requests": "write",
                    "checks": "write",
                }
            }))
            .send()
            .await?
            .parse_json()
            .await?;

        Ok(GitHub {
            req: self.req,
            auth: AuthInstallation(installation_token.token),
        })
    }
}

impl GitHubAuth for GitHub<AuthInstallation> {
    async fn auth_installation(
        self,
        _installation: &impl AsInstallation,
    ) -> ApiResult<GitHub<AuthInstallation>> {
        Ok(self)
    }
}

impl GitHub<AuthInstallation> {
    #[tracing::instrument(skip(self))]
    pub async fn create_check_run(
        &self,
        owner: OwnerId,
        repo: RepoId,
        check_run_name: String,
        head_sha: &str,
    ) -> ApiResult<CheckRun<MinimalCheckSuite>> {
        let response = self
            .post(format!("repos/{owner}/{repo}/check-runs"))
            .body(serde_json::to_string(&serde_json::json!({
                "name": check_run_name,
                "head_sha": head_sha,
                "status": "in_progress",
            }))?)
            .send()
            .await?;

        if response.status() != StatusCode::CREATED {
            return Err(ApiError::UnexpectedResponse(response.text().await?));
        }

        let result = serde_json::from_str(&response.text().await?)?;
        Ok(result)
    }

    #[tracing::instrument(skip(self, output))]
    pub async fn update_check_run<'a>(
        &self,
        owner: OwnerId,
        repo: RepoId,
        check_run: CheckRunId,
        success: bool,
        output: CheckRunOutput<'a>,
    ) -> ApiResult<()> {
        let res = self
            .patch(format!("repos/{owner}/{repo}/check-runs/{check_run}"))
            .body(serde_json::to_string(&serde_json::json!({
                "status": "completed",
                "conclusion": if success { "success" } else { "failure" },
                "output": output,
            }))?)
            .send()
            .await?
            .text()
            .await?;
        debug!("GitHub said: {}", res);
        Ok(())
    }
}

#[async_trait::async_trait]
impl FromRequestParts<AppState> for GitHub {
    type Rejection = StatusCode;

    async fn from_request_parts<'a, 's>(
        _parts: &'a mut Parts,
        state: &'s AppState,
    ) -> Result<Self, Self::Rejection> {
        let Ok(private_key) = RS256KeyPair::from_pem(&state.private_key) else {
            warn!("The private key in the .env file cannot be parsed as PEM.");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        };

        let claims = Claims::create(Duration::from_mins(10)).with_issuer(&state.app_id);
        let Ok(token) = private_key.sign(claims) else {
            warn!("Couldn't sign JWT claims.");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        };

        Ok(Self {
            auth: AuthJwt(token),
            req: reqwest::Client::new(),
        })
    }
}

#[derive(Debug)]
pub struct OwnerId(String);

#[derive(Debug)]
pub struct RepoId(String);

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

#[derive(Debug, Deserialize)]
pub struct Repository {
    full_name: String,
}

impl Repository {
    pub fn new(name: &str) -> eyre::Result<Self> {
        if !name.contains('/') {
            return Err(Error::msg("Invalid repository path"));
        }

        Ok(Self {
            full_name: name.to_owned(),
        })
    }

    pub fn owner(&self) -> OwnerId {
        OwnerId(
            self.full_name
                .split_once('/')
                .expect("Repository path must contain a /")
                .0
                .to_owned(),
        )
    }

    pub fn name(&self) -> RepoId {
        RepoId(
            self.full_name
                .split_once('/')
                .expect("Repository path must contain a /")
                .1
                .to_owned(),
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct Installation {
    pub id: u64,
}

pub trait AsInstallation {
    fn id(&self) -> u64;
}

impl AsInstallation for Installation {
    fn id(&self) -> u64 {
        self.id
    }
}

#[derive(Deserialize)]
struct InstallationToken {
    token: String,
}

trait JsonExt {
    async fn parse_json<T: for<'a> Deserialize<'a>>(self) -> Result<T, ApiError>;
}

impl JsonExt for Response {
    async fn parse_json<T: for<'a> Deserialize<'a>>(self) -> Result<T, ApiError> {
        let bytes = self.bytes().await?;

        debug!(
            "Parsing JSON: {}",
            std::str::from_utf8(&bytes).unwrap_or("[INVALID UTF8]")
        );

        Ok(serde_json::from_slice(&bytes)?)
    }
}
