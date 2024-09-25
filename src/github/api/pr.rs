use serde::{Deserialize, Serialize};

use super::{ApiError, GitHub, OwnerId, RepoId};

#[derive(Clone, Deserialize)]
pub struct MinimalPullRequest {
    pub number: usize,
}

impl MinimalPullRequest {
    pub async fn get_full(
        &self,
        api: &GitHub,
        owner: OwnerId,
        repo: RepoId,
    ) -> Result<PullRequest, ApiError> {
        Ok(api
            .get(format!(
                "repos/{owner}/{repo}/pulls/{pull_number}",
                owner = owner,
                repo = repo,
                pull_number = self.number
            ))
            .send()
            .await?
            .json()
            .await?)
    }
}

#[derive(Clone, Deserialize)]
pub struct PullRequest {
    pub number: usize,
    pub head: Commit,
    pub title: String,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum AnyPullRequest {
    Full(PullRequest),
    Minimal(MinimalPullRequest),
}

impl AnyPullRequest {
    pub async fn get_full(
        self,
        api: &GitHub,
        owner: OwnerId,
        repo: RepoId,
    ) -> Result<PullRequest, ApiError> {
        match self {
            AnyPullRequest::Full(pr) => Ok(pr),
            AnyPullRequest::Minimal(pr) => pr.get_full(api, owner, repo).await,
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct Commit {
    pub sha: String,
}

#[derive(Serialize)]
pub struct PullRequestUpdate {
    pub title: String,
}

impl GitHub {
    pub async fn update_pull_request(
        &self,
        owner: OwnerId,
        repo: RepoId,
        pr: usize,
        update: PullRequestUpdate,
    ) -> Result<PullRequest, ApiError> {
        Ok(self
            .patch(format!("{}/{}/pulls/{}", owner, repo, pr))
            .json(&update)
            .send()
            .await?
            .json()
            .await?)
    }
}
