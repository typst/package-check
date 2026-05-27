use serde::{Deserialize, Serialize};

use super::{ApiError, AuthInstallation, GitHub, JsonExt, OwnerId, RepoId, user::User};

#[derive(Deserialize)]
pub struct PullRequestEvent {
    pub pull_request: PullRequest,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PullRequest {
    pub number: usize,
    pub title: String,
    pub body: Option<String>,
    pub user: User,
    pub head: Commit,
    pub labels: Vec<Label>,
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
    pub title: String,
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Deserialize)]
pub struct Comment {
    pub user: User,
}

impl GitHub<AuthInstallation> {
    pub async fn update_pull_request(
        &self,
        owner: OwnerId,
        repo: RepoId,
        pr: usize,
        update: PullRequestUpdate,
    ) -> Result<(), ApiError> {
        self.patch(format!("repos/{}/{}/issues/{}", owner, repo, pr))
            .json(&update)
            .send()
            .await?
            .parse_json::<serde_json::Value>()
            .await?;

        Ok(())
    }

    pub async fn prs_for_commit(
        &self,
        owner: OwnerId,
        repo: RepoId,
        commit: String,
    ) -> Result<Vec<PullRequest>, ApiError> {
        self.get(format!("repos/{owner}/{repo}/commits/{commit}/pulls"))
            .send()
            .await?
            .parse_json()
            .await
    }

    pub async fn post_pr_comment(
        &self,
        owner: OwnerId,
        repo: RepoId,
        pr: usize,
        message: String,
    ) -> Result<(), ApiError> {
        self.post(format!("repos/{owner}/{repo}/issues/{pr}/comments"))
            .json(&serde_json::json!({
                "body": message
            }))
            .send()
            .await?
            .parse_json::<serde_json::Value>()
            .await?;

        Ok(())
    }

    pub async fn list_pr_comments(
        &self,
        owner: OwnerId,
        repo: RepoId,
        pr: usize,
    ) -> Result<Vec<Comment>, ApiError> {
        self.get(format!("/repos/{owner}/{repo}/issues/{pr}/comments"))
            .send()
            .await?
            .parse_json()
            .await
    }
}
