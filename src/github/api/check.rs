use std::fmt::Display;

use http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::github::api::{ApiError, ApiResult, AuthInstallation, GitHub, RepositoryId};

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(transparent)]
pub struct CheckRunId(u64);

impl Display for CheckRunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct CheckRun {
    pub id: CheckRunId,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CheckRunOutput {
    pub title: String,
    pub summary: String,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Serialize)]
pub struct Annotation {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<usize>,
    pub annotation_level: AnnotationLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationLevel {
    Notice,
    Warning,
    Failure,
}

impl GitHub<AuthInstallation> {
    #[tracing::instrument(skip(self))]
    pub async fn create_check_run(
        &self,
        repo: &RepositoryId,
        check_run_name: String,
        head_sha: &str,
        success: bool,
        output: CheckRunOutput,
    ) -> ApiResult<CheckRun> {
        let response = self
            .post(format!("repos/{repo}/check-runs"))
            .json(&serde_json::json!({
                "name": check_run_name,
                "head_sha": head_sha,
                "status": "completed",
                "conclusion": if success { "success" } else { "failure" },
                "output": output,
            }))
            .send()
            .await?;

        if response.status() != StatusCode::CREATED {
            return Err(ApiError::UnexpectedResponse(response.text().await?));
        }

        Ok(response.json().await?)
    }
}
