use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use codespan_reporting::{
    diagnostic::{Diagnostic, Severity},
    files::Files,
};
use eyre::Context;
use jwt_simple::prelude::*;
use pr::{PullRequest, PullRequestUpdate};
use tracing::{debug, warn};
use typst::syntax::{package::PackageSpec, FileId};

use crate::{check, package::PackageExt, world::SystemWorld};

use api::{check::CheckRun, *};

pub mod api;
pub mod git;

use self::{
    api::check::{Annotation, AnnotationLevel, CheckRunOutput},
    git::GitRepo,
};

/// Application configuration, read from .env file.
#[derive(Clone)]
pub struct AppState {
    private_key: String,
    app_id: String,
    pub git_dir: String,
}

impl AppState {
    pub fn read() -> AppState {
        AppState {
            private_key: std::env::var("GITHUB_PRIVATE_KEY")
                .expect("GITHUB_PRIVATE_KEY is not set.")
                .replace('&', "\n"),
            app_id: std::env::var("GITHUB_APP_IDENTIFIER")
                .expect("GITHUB_APP_IDENTIFIER is not set."),
            git_dir: std::env::var("PACKAGES_DIR")
                .or_else(|_| std::env::var("GITHUB_WORKSPACE"))
                .expect("PACKAGES_DIR is not set."),
        }
    }

    pub fn as_github_api(&self) -> Result<GitHub<AuthJwt>, ()> {
        let Ok(private_key) = RS256KeyPair::from_pem(&self.private_key) else {
            warn!("The private key in the .env file cannot be parsed as PEM.");
            return Err(());
        };

        let claims = Claims::create(Duration::from_mins(10)).with_issuer(&self.app_id);
        let Ok(token) = private_key.sign(claims) else {
            warn!("Couldn't sign JWT claims.");
            return Err(());
        };

        Ok(GitHub {
            auth: AuthJwt(token),
            req: reqwest::Client::new(),
        })
    }
}

pub async fn run_github_check(
    git_dir: &String,
    head_sha: String,
    api_client: GitHub<AuthInstallation>,
    repository: Repository,
    previous_check_run: Option<CheckRun>,
    pr: Option<PullRequest>,
) -> eyre::Result<()> {
    let git_repo = GitRepo::open(Path::new(git_dir)).await?;
    git_repo.pull_main().await?;
    git_repo.fetch_commit(&head_sha).await?;
    let touched_files = git_repo.files_touched_by(&head_sha).await?;

    let mut touches_outside_of_packages = false;

    let touched_packages = touched_files
        .into_iter()
        .filter_map(|line| {
            let mut components = line.components();
            if components.next()?.as_os_str() != OsStr::new("packages") {
                touches_outside_of_packages = true;
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

    debug!(
        "This commit touched the following packages: {:#?}",
        touched_packages
    );

    if let Some(pr) = &pr {
        debug!("This commit is linked to PR {}", pr.number);

        // Update labels
        let mut has_new_packages = false;
        let mut has_updated_packages = false;
        for package in &touched_packages {
            if git_repo
                .has_previous_version(package)
                .await
                .unwrap_or(false)
            {
                has_updated_packages = true;
            } else {
                has_new_packages = true;
            }
        }
        let mut labels = Vec::new();
        if has_new_packages {
            labels.push("new".to_owned())
        }
        if has_updated_packages {
            labels.push("update".to_owned());
        }

        // Update checks in PR body if needed
        let mut body_changed = false;
        let new_body = pr
            .body
            .lines()
            .map(|l| {
                let line = l.trim();
                if line.starts_with("-") {
                    let marked = line.contains("[x]");
                    if line.ends_with("a new package") {
                        body_changed |= marked != has_new_packages;
                        if has_new_packages {
                            return "- [x] a new package";
                        } else {
                            return "- [ ] a new package";
                        }
                    }

                    if line.ends_with("an update for a package") {
                        body_changed |= marked != has_updated_packages;
                        if has_updated_packages {
                            return "- [x] an update for a package";
                        } else {
                            return "- [ ] an update for a package";
                        }
                    }
                }

                l
            })
            .fold(String::with_capacity(pr.body.len()), |body, line| {
                body + "\n" + line
            });
        let body = if body_changed { Some(new_body) } else { None };

        // Update title
        let mut package_names = touched_packages
            .iter()
            .map(|p| format!("{}:{}", p.name, p.version))
            .collect::<Vec<_>>();
        package_names.sort();
        let last_package = package_names.pop();
        let penultimate_package = package_names.pop();
        let expected_pr_title = if let Some((penultimate_package, last_package)) =
            penultimate_package.as_ref().zip(last_package.as_ref())
        {
            package_names.push(format!("{} and {}", penultimate_package, last_package));
            Some(package_names.join(", "))
        } else {
            last_package
        };

        debug!(
            "Updating PR metadata. Expected title : {:?}, expected labels: {:?}",
            expected_pr_title, labels
        );
        // Actually update the PR, if needed
        if let Some(expected_pr_title) = expected_pr_title {
            if pr.title != expected_pr_title || !labels.is_empty() || body.is_some() {
                api_client
                    .update_pull_request(
                        repository.owner(),
                        repository.name(),
                        pr.number,
                        PullRequestUpdate {
                            title: expected_pr_title,
                            labels,
                            body,
                        },
                    )
                    .await
                    .context("Failed to update pull request")?;
            }
        }
    }

    for ref package in touched_packages {
        let check_run_name = format!(
            "@{}/{}:{}",
            package.namespace, package.name, package.version
        );

        let check_run = if let Some(previous) = previous_check_run
            .as_ref()
            .filter(|p| p.name == check_run_name)
        {
            previous
        } else {
            debug!("Creating a new check run");
            &api_client
                .create_check_run(
                    repository.owner(),
                    repository.name(),
                    check_run_name,
                    &head_sha,
                )
                .await
                .context("Failed to create a new check run")?
        };

        if touches_outside_of_packages {
            api_client.update_check_run(
                repository.owner(),
                repository.name(),
                check_run.id,
                false,
                CheckRunOutput {
                    title: "This PR does too many things",
                    summary: "A PR should either change packages/, or the rest of the repository, but not both.",
                    annotations: &[],
                },
            ).await
            .context("Failed to cancel a check run because the branch does too many things")?;
            continue;
        }

        let checkout_dir = format!("checkout-{}", head_sha);
        git_repo
            .checkout_commit(&head_sha, &checkout_dir)
            .await
            .context("Failed to checkout commit")?;

        // Check that the author of this PR is the same as the one of
        // the previous version.
        if let Some(current_pr) = &pr {
            debug!("There is a current PR");
            if let Some(previous_commit) =
                check::authors::commit_for_previous_version(package).await
            {
                debug!("Found previous commit: {previous_commit}");
                if let Ok(Some(previous_pr)) = api_client
                    .prs_for_commit(repository.owner(), repository.name(), previous_commit)
                    .await
                    .map(|prs| prs.into_iter().next())
                {
                    debug!(
                        "Found previous PR: #{} (author: {})",
                        previous_pr.number, previous_pr.user.login
                    );
                    if previous_pr.user.login != current_pr.user.login {
                        if let Err(e) = api_client
                            .post_pr_comment(
                                repository.owner(),
                                repository.name(),
                                current_pr.number,
                                format!(
                                    "@{} You released {}:{}, so you probably \
                                    want to have a look at this pull request. \
                                    If you want this update to be merged, \
                                    please leave a comment stating so. \
                                    Without your permission, the pull request \
                                    will not be merged.",
                                    previous_pr.user.login,
                                    package.name,
                                    package.previous_version()
                                        .expect("If there is no previous version, this branch should not be reached")
                                        .version
                                ),
                            )
                            .await
                            {
                                warn!("Error while posting PR comment: {:?}", e)
                            }
                    }
                }
            }
        }

        let (world, diags) = match check::all_checks(
            Some(package),
            PathBuf::new()
                .join(&checkout_dir)
                .join("packages")
                .join(package.namespace.as_str())
                .join(package.name.as_str())
                .join(package.version.to_string()),
            false,
        )
        .await
        {
            Ok(x) => x,
            Err(e) => {
                api_client
                    .update_check_run(
                        repository.owner(),
                        repository.name(),
                        check_run.id,
                        false,
                        CheckRunOutput {
                            title: "Fatal error",
                            summary: &format!("The following error was encountered:\n\n{}", e),
                            annotations: &[],
                        },
                    )
                    .await
                    .context("Failed to report fatal error")?;
                return Err(e);
            }
        };

        let plural = |n| if n == 1 { "" } else { "s" };

        api_client
            .update_check_run(
                repository.owner(),
                repository.name(),
                check_run.id,
                diags.errors().is_empty() && diags.warnings().is_empty(),
                CheckRunOutput {
                    title: &if !diags.errors().is_empty() {
                        if diags.warnings().is_empty() {
                            format!(
                                "{} error{}",
                                diags.errors().len(),
                                plural(diags.errors().len())
                            )
                        } else {
                            format!(
                                "{} error{}, {} warning{}",
                                diags.errors().len(),
                                plural(diags.errors().len()),
                                diags.warnings().len(),
                                plural(diags.warnings().len())
                            )
                        }
                    } else if diags.warnings().is_empty() {
                        "All good!".to_owned()
                    } else {
                        format!(
                            "{} warning{}",
                            diags.warnings().len(),
                            plural(diags.warnings().len())
                        )
                    },
                    summary: &format!(
                        "Our bots have automatically run some checks on your packages. \
                                They found {} error{} and {} warning{}.\n\n\
                                Warnings are suggestions, your package can still be accepted even \
                                if you prefer not to fix them.\n\n\
                                A human being will soon review your package, too.",
                        diags.errors().len(),
                        plural(diags.errors().len()),
                        diags.warnings().len(),
                        plural(diags.warnings().len()),
                    ),
                    annotations: &diags
                        .errors()
                        .iter()
                        .chain(diags.warnings())
                        .filter_map(|diag| diagnostic_to_annotation(&world, package, diag))
                        .take(50)
                        .collect::<Vec<_>>(),
                },
            )
            .await
            .context("Failed to send report")?;

        tokio::fs::remove_dir_all(checkout_dir).await?;
    }

    Ok(())
}

fn diagnostic_to_annotation(
    world: &SystemWorld,
    package: &PackageSpec,
    diag: &Diagnostic<FileId>,
) -> Option<Annotation> {
    let label = diag.labels.first()?;
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
    let package = label.file_id.package().unwrap_or(package);
    Some(Annotation {
        path: Path::new("packages")
            .join(package.namespace.to_string())
            .join(package.name.to_string())
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
        message: diag.message.clone(),
    })
}
