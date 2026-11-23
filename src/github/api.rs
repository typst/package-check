//! Interact with the GitHub REST API.

use std::fmt::Display;

use reqwest::{RequestBuilder, Response};
use serde::Deserialize;
use tracing::debug;

pub mod check;
pub mod pr;

#[derive(Debug)]
pub enum ApiError {
    #[allow(dead_code)]
    Reqwest(reqwest::Error),
    Json(serde_json::Error),
    UnexpectedResponse(String),
    InvalidHeader(http::header::ToStrError),
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ApiError::Reqwest(e) => Some(e),
            ApiError::Json(e) => Some(e),
            ApiError::UnexpectedResponse(_) => None,
            ApiError::InvalidHeader(e) => Some(e),
        }
    }
}

impl Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Reqwest(e) => write!(f, "Network error: {:?}", e),
            ApiError::Json(e) => write!(f, "JSON ser/de error: {:?}", e),
            ApiError::UnexpectedResponse(e) => write!(f, "Unexpected response: {:?}", e),
            ApiError::InvalidHeader(e) => write!(f, "Invalid header: {:?}", e),
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

impl From<http::header::ToStrError> for ApiError {
    fn from(v: http::header::ToStrError) -> Self {
        Self::InvalidHeader(v)
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Authentication for the GitHub API using a JWT token.
pub struct AuthJwt(pub String);

impl Display for AuthJwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Authentication for the GitHub API using an installation token, that
/// is scoped to a specific organization or set of repositories, but that
/// can do more than a [`AuthJwt`] token.
#[derive(Clone)]
pub struct AuthInstallation(String);

impl Display for AuthInstallation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A GitHub API client
#[derive(Clone)]
pub struct GitHub<A = AuthJwt> {
    pub(crate) auth: A,
    pub(crate) req: reqwest::Client,
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

    fn delete(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.with_headers(self.req.delete(Self::url(url)))
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

impl GitHub<AuthJwt> {
    #[tracing::instrument(skip_all)]
    pub async fn auth_installation(
        self,
        installation: &Installation,
    ) -> ApiResult<GitHub<AuthInstallation>> {
        let installation_id = installation.id;
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
            .json()
            .await?;

        Ok(GitHub {
            req: self.req,
            auth: AuthInstallation(installation_token.token),
        })
    }
}

/// Repository ID in the form of `owner/name`.
#[derive(Debug, Clone, Deserialize)]
pub struct RepositoryId {
    full_name: String,
}

impl RepositoryId {
    pub fn new(full_name: String) -> Result<Self, &'static str> {
        if full_name.chars().filter(|c| *c == '/').count() != 1 {
            return Err("Invalid repository id (path must contain exactly one `/`)");
        };
        Ok(Self { full_name })
    }
}

impl Display for RepositoryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.full_name)
    }
}

#[derive(Debug, Deserialize)]
pub struct Installation {
    pub id: u64,
}

#[derive(Deserialize)]
struct InstallationToken {
    token: String,
}

trait ResponseExt: Sized {
    async fn expect_success(self) -> Result<Self, ApiError>;
}

impl ResponseExt for Response {
    async fn expect_success(self) -> Result<Self, ApiError> {
        if !self.status().is_success() {
            let text = self.text().await?;
            return Err(ApiError::UnexpectedResponse(text));
        }

        Ok(self)
    }
}
