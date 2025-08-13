use crate::{
    check::TryExt,
    github::{
        api::{pr::PullRequestEvent, GitHubAuth, Installation, Repository},
        run_github_check, AppState,
    },
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

    let event = tokio::fs::read_to_string(
        std::env::var("GITHUB_EVENT_PATH").expect("This command should be run in GitHub Actions"),
    )
    .await
    .error("github/actions/event", "Failed to read event metadata")
    .unwrap();
    let event: PullRequestEvent = serde_json::from_str(&event)
        .error("github/actions/event/invalid", "Invalid event JSON")
        .unwrap();

    run_github_check(
        &state.git_dir,
        event.pull_request.head.sha.clone(),
        api_client,
        repository,
        None,
        Some(event.pull_request),
    )
    .await
    .unwrap();
}
