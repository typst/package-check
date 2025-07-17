use eyre::Context;

use crate::github::{
    api::{pr::MinimalPullRequest, GitHubAuth, Installation, Repository},
    run_github_check, AppState,
};

pub async fn main() {
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

    let pr = if let Ok(ref_name) = std::env::var("GITHUB_REF_NAME") {
        let pr_number = ref_name.trim_end_matches("/merge");
        let pr = MinimalPullRequest {
            number: pr_number.parse().expect("Invalid PR number"),
        };
        pr.get_full(&api_client, repository.owner(), repository.name())
            .await
            .ok()
    } else {
        None
    };

    let event = tokio::fs::read_to_string(
        std::env::var("GITHUB_EVENT_PATH").expect("This command should be run in GitHub Actions"),
    )
    .await
    .context("Failed to read event metadata")
    .unwrap();
    let event: serde_json::Value = serde_json::from_str(&event)
        .context("Invalid event JSON")
        .unwrap();

    run_github_check(
        &state.git_dir,
        std::env::var("GITHUB_SHA").expect("This command should be run in GitHub Actions"),
        event["pull_request"]["head"]["sha"]
            .as_str()
            .expect("Malformed GitHub event")
            .to_owned(),
        api_client,
        repository,
        None,
        pr,
    )
    .await
    .unwrap();
}
