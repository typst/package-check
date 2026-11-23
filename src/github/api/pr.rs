use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

use crate::github::api::{ApiError, AuthInstallation, GitHub, RepositoryId, ResponseExt};

#[derive(Deserialize)]
pub struct PullRequestEvent {
    pub pull_request: PullRequest,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PullRequest {
    pub number: usize,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub user: User,
    pub head: Commit,
    pub labels: Vec<Label>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Commit {
    pub sha: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Serialize)]
pub struct PullRequestUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<IndexSet<String>>,
}

#[derive(Deserialize)]
pub struct Comment {
    pub id: CommentId,
    pub user: User,
    pub body: String,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(transparent)]
pub struct CommentId(u64);

impl GitHub<AuthInstallation> {
    pub async fn update_pull_request(
        &self,
        repo: &RepositoryId,
        pr_number: usize,
        update: PullRequestUpdate,
    ) -> Result<(), ApiError> {
        self.patch(format!("repos/{repo}/issues/{pr_number}"))
            .json(&update)
            .send()
            .await?
            .expect_success()
            .await?;

        Ok(())
    }

    pub async fn prs_for_commit(
        &self,
        repo: &RepositoryId,
        commit: &str,
    ) -> Result<Vec<PullRequest>, ApiError> {
        let prs = self
            .get(format!("repos/{repo}/commits/{commit}/pulls"))
            .send()
            .await?
            .json()
            .await?;

        Ok(prs)
    }

    pub async fn delete_pr_comment(
        &self,
        repo: &RepositoryId,
        pr: usize,
        CommentId(id): CommentId,
    ) -> Result<(), ApiError> {
        self.delete(format!("repos/{repo}/issues/{pr}/comments/{id}"))
            .send()
            .await?
            .expect_success()
            .await?;

        Ok(())
    }

    pub async fn post_pr_comment(
        &self,
        repo: &RepositoryId,
        pr: usize,
        message: String,
    ) -> Result<(), ApiError> {
        self.post(format!("repos/{repo}/issues/{pr}/comments"))
            .json(&serde_json::json!({
                "body": message
            }))
            .send()
            .await?
            .expect_success()
            .await?;

        Ok(())
    }

    pub async fn list_pr_comments(
        &self,
        repo: &RepositoryId,
        pr: usize,
    ) -> Result<Vec<Comment>, ApiError> {
        let mut comments = Vec::new();

        // Deal with pagination:
        // https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api?apiVersion=2026-03-10
        for p in 1.. {
            let resp = self
                .get(format!(
                    "repos/{repo}/issues/{pr}/comments?per_page=100&page={p}"
                ))
                .send()
                .await?;

            let mut has_next = false;
            if let Some(link) = resp.headers().get("link") {
                has_next = link.to_str()?.contains("rel=\"next\"");
            };

            let page = resp.json::<Vec<Comment>>().await?;
            comments.extend(page);

            if !has_next {
                break;
            }
        }

        Ok(comments)
    }
}
