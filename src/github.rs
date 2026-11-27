use std::fmt::Write;
use std::path::{Path, PathBuf};

use codespan_reporting::files::Files;
use indexmap::map::Entry;
use indexmap::{IndexMap, IndexSet};
use jwt_simple::prelude::*;
use tracing::{debug, warn};
use typst::syntax::VirtualRoot;
use typst::syntax::package::{PackageSpec, PackageVersion, VersionlessPackageSpec};

use crate::check::codes::{self, Code, Output};
use crate::check::diagnostics::{Diagnostic, Docs, Severity};
use crate::check::{self, Result, TryExt};
use crate::github::api::check::CheckRun;
use crate::github::api::pr::{Comment, PullRequest, PullRequestUpdate};
use crate::github::api::{AuthInstallation, AuthJwt, GitHub, RepositoryId};
use crate::workspace::{SystemPackages, Workspace};
use crate::world::SystemWorld;

pub mod api;
pub mod git;

use self::{
    api::check::{Annotation, AnnotationLevel, CheckRunOutput},
    git::GitRepo,
};

const MAX_ANNOTATIONS: usize = 50;
const COMMENT_TAG_AUTHOR: &str = "<!-- package-check-comment-author -->";
const COMMENT_TAG_REVIEW: &str = "<!-- package-check-comment-review -->";

/// Application configuration, read from .env file.
#[derive(Clone)]
pub struct AppState {
    private_key: String,
    app_id: String,
    pub workspace: Workspace,
}

impl AppState {
    pub fn read() -> AppState {
        AppState {
            private_key: std::env::var("GITHUB_PRIVATE_KEY")
                .expect("GITHUB_PRIVATE_KEY is not set.")
                .replace('&', "\n"),
            app_id: std::env::var("GITHUB_APP_IDENTIFIER")
                .expect("GITHUB_APP_IDENTIFIER is not set."),
            workspace: Workspace::new(
                std::env::var("PACKAGES_DIR")
                    .or_else(|_| std::env::var("GITHUB_WORKSPACE"))
                    .expect("PACKAGES_DIR is not set.")
                    .into(),
                // Ignore system wide installed packages.
                SystemPackages::empty(),
            ),
        }
    }

    #[expect(clippy::result_unit_err)]
    pub fn github_api_client(&self) -> std::result::Result<GitHub<AuthJwt>, ()> {
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

pub async fn run_action(
    workspace: &Workspace,
    api_client: &GitHub<AuthInstallation>,
    repo: &RepositoryId,
    pr: &PullRequest,
) -> Result<()> {
    let kind = check_pr(workspace, api_client, repo, pr).await?;

    let previous_comments = api_client
        .list_pr_comments(repo, pr.number)
        .await
        .map_err(|err| {
            Diagnostic::error(Code::GithubApiPullCommentsList).with_message(format!(
                "Failed to fetch comments for #{}: {err:?}",
                pr.number,
            ))
        })?;

    // Update PR
    update_pr(api_client, repo, pr, &kind).await?;

    // Review comment
    post_review_comment(api_client, repo, pr, &previous_comments, &kind).await?;

    // Authors comment
    if let PrKind::Packages {
        different_authors, ..
    } = kind
    {
        post_different_authors_comment(api_client, repo, pr, &previous_comments, different_authors)
            .await?;
    }

    Ok(())
}

struct Touched {
    outside_of_packages: bool,
    /// Files that are withing `packages/`, but not recognized as package files.
    misplaced_package_files: IndexSet<PathBuf>,
    packages: IndexSet<PackageSpec>,
}

impl Touched {
    pub fn inside_of_packages(&self) -> bool {
        !self.misplaced_package_files.is_empty() || !self.packages.is_empty()
    }
}

fn touched(git_repo: &GitRepo) -> Result<Touched> {
    let touched_files = git_repo.files_touched_by("HEAD")?;

    let mut outside_of_packages = false;
    let mut misplaced_package_files = IndexSet::new();
    let mut packages = touched_files
        .into_iter()
        .filter_map(|file| {
            touched_package(&file)
                .map_err(|err| match err {
                    Some(misplaced) => {
                        misplaced_package_files.insert(misplaced);
                    }
                    None => outside_of_packages = true,
                })
                .ok()
        })
        .collect::<IndexSet<_>>();

    misplaced_package_files.sort();
    packages.sort_by_key(|spec| (spec.name.clone(), spec.version));

    Ok(Touched {
        outside_of_packages,
        misplaced_package_files,
        packages,
    })
}

fn touched_package(file: &Path) -> std::result::Result<PackageSpec, Option<PathBuf>> {
    let mut i = 0;
    let mut components = file.components();
    let mut next = || {
        components.next().map(|c| {
            i += 1;
            c.as_os_str()
                .to_str()
                .expect("git output has been parsed as a string")
        })
    };
    let misplaced = |i| Err(Some(file.components().take(i).collect()));

    let Some("packages") = next() else {
        return Err(None);
    };
    let Some(namespace @ "preview") = next() else {
        return misplaced(i);
    };

    let Some(name) = next() else {
        return misplaced(i);
    };

    let Some(version) = next().and_then(|v| v.parse().ok()) else {
        return misplaced(i);
    };
    // Version must be a directory.
    if components.next().is_none() {
        return misplaced(i);
    }

    Ok(PackageSpec {
        namespace: namespace.into(),
        name: name.into(),
        version,
    })
}

enum PrKind {
    /// No changes at all.
    Empty,
    /// Changes only outside of the `packages/` directory.
    Meta,
    /// Changes both inside and outside of the `packages/` directory.
    Mixed(Touched),
    /// Changes inside the `packages/` directory, but only misplaced files.
    Misplaced(IndexSet<PathBuf>),
    /// Changes inside the `packages/` directory, with at least one correctly
    /// placed package.
    Packages {
        touched: Touched,
        other_versions: OtherPackageVersions,
        different_authors: DifferentPackageAuthors,
        checks: Vec<CheckedPackage>,
    },
}
impl PrKind {
    /// The PR in its current state is invalid. Either because it changes too
    /// many things, or because it adds files in locations that aren't allowed.
    fn is_invalid(&self) -> bool {
        match self {
            PrKind::Empty => false,
            PrKind::Meta => false,
            PrKind::Mixed(_) => true,
            PrKind::Misplaced(_) => true,
            PrKind::Packages { touched, .. } => !touched.misplaced_package_files.is_empty(),
        }
    }
}

async fn check_pr(
    workspace: &Workspace,
    api_client: &GitHub<AuthInstallation>,
    repo: &RepositoryId,
    pr: &PullRequest,
) -> Result<PrKind> {
    let git_repo = GitRepo::open(workspace.repo().dir().to_owned())?;
    let touched = touched(&git_repo)?;
    debug!(
        "This commit touched the following packages: {:#?}",
        touched.packages
    );
    debug!("This commit is linked to PR {}", pr.number);

    if touched.outside_of_packages {
        return if touched.inside_of_packages() {
            Ok(PrKind::Mixed(touched))
        } else {
            Ok(PrKind::Meta)
        };
    } else if touched.packages.is_empty() {
        return if touched.misplaced_package_files.is_empty() {
            Ok(PrKind::Empty)
        } else {
            Ok(PrKind::Misplaced(touched.misplaced_package_files))
        };
    }

    let other_versions = other_package_versions(workspace, &touched)?;
    let different_authors =
        different_package_authors(workspace, api_client, &git_repo, pr, repo, &other_versions)
            .await?;

    // Run checks in parallel.
    let mut tasks = Vec::with_capacity(touched.packages.len());
    for package in touched.packages.iter().cloned() {
        let workspace = workspace.clone();
        tasks.push(tokio::task::spawn(check_package(workspace, package)));
    }

    let mut checks = Vec::with_capacity(touched.packages.len());
    for (package, task) in touched.packages.iter().zip(tasks) {
        let (output, check_run_output) = task.await.unwrap();
        let success = output.as_ref().is_some_and(|o| o.passed());

        // Directly create the concluded check run, because sometimes newly
        // created runs aren't found by the API. Running checks for a
        // package doesn't take very long, compared to checking out the
        // repo, so this isn't really perceivable.
        debug!("Creating check run, success: {success}, output: {check_run_output:?}");
        let run = api_client
            .create_check_run(
                repo,
                package.to_string(),
                &pr.head.sha,
                success,
                check_run_output,
            )
            .await
            .error(
                Code::GithubApiCheckRunCreate,
                "Failed to create a new check run",
            )?;

        checks.push(CheckedPackage { run, output });
    }

    Ok(PrKind::Packages {
        touched,
        other_versions,
        different_authors,
        checks,
    })
}

#[derive(Default)]
struct OtherPackageVersions(IndexMap<VersionlessPackageSpec, IndexSet<PackageVersion>>);

impl OtherPackageVersions {
    fn has_updated_package(&self) -> bool {
        self.0.iter().any(|(_, other)| !other.is_empty())
    }

    fn has_new_package(&self) -> bool {
        self.0.iter().any(|(_, other)| other.is_empty())
    }

    fn iter(&self) -> impl Iterator<Item = (&VersionlessPackageSpec, &IndexSet<PackageVersion>)> {
        self.0.iter()
    }
}

/// Package versions that haven't been added in this PR.
fn other_package_versions(
    workspace: &Workspace,
    touched: &Touched,
) -> Result<OtherPackageVersions> {
    let mut other_package_versions = IndexMap::new();

    // Read all package versions.
    for package in touched.packages.iter() {
        if let Entry::Vacant(vacant) = other_package_versions.entry(package.versionless()) {
            let versions = workspace.repo().find_versions(package)?;
            vacant.insert(versions);
        }
    }

    // Filter out packages that have been touched.
    for touched in touched.packages.iter() {
        let versions = &mut other_package_versions[&touched.versionless()];
        versions.shift_remove(&touched.version);
    }

    Ok(OtherPackageVersions(other_package_versions))
}

type DifferentPackageAuthors = IndexMap<String, Vec<PackageSpec>>;

/// Packages that have previously been published by different authors.
async fn different_package_authors(
    workspace: &Workspace,
    api_client: &GitHub<AuthInstallation>,
    git_repo: &GitRepo,
    pr: &PullRequest,
    repository: &RepositoryId,
    other_versions: &OtherPackageVersions,
) -> Result<DifferentPackageAuthors> {
    let mut different_authors = DifferentPackageAuthors::default();
    for (package, versions) in other_versions.iter() {
        let Some(version) = versions.last() else {
            continue;
        };

        let newest_spec = package.clone().at(*version);
        let manifest = workspace.repo().manifest_path_of(&newest_spec);
        let Some(previous_commit) = git_repo.commit_for_file(&manifest) else {
            continue;
        };
        debug!("Found previous commit: {previous_commit}");

        let previous_prs = api_client
            .prs_for_commit(repository, &previous_commit)
            .await
            .error(Code::GithubApiCommitPulls, "Failed to get PR for commit")?;

        if let Some(previous_pr) = previous_prs.into_iter().next() {
            debug!(
                "Found previous PR: #{} (author: {})",
                previous_pr.number, previous_pr.user.login
            );

            // TODO: Handle github organizations?
            let prev_author = previous_pr.user.login;
            if prev_author != pr.user.login {
                different_authors
                    .entry(prev_author)
                    .or_default()
                    .push(newest_spec);
            }
        }
    }

    Ok(different_authors)
}

async fn update_pr(
    api_client: &GitHub<AuthInstallation>,
    repo: &RepositoryId,
    pr: &PullRequest,
    kind: &PrKind,
) -> Result<()> {
    // Update title
    let mut title = None;
    if let PrKind::Packages { touched, .. } = &kind {
        let mut expected = String::new();
        for (i, p) in touched.packages.iter().enumerate() {
            if i > 0 {
                let last = i == touched.packages.len() - 1;
                expected.push_str(if last { " and " } else { ", " });
            }

            write!(expected, "{}:{}", p.name, p.version).ok();
        }

        if pr.title != expected {
            title = Some(expected);
        }
    }

    // Update labels
    let mut has_new_packages = false;
    let mut has_updated_packages = false;
    let mut has_staff_override = false;
    let mut waiting_on_review = false;
    let mut waiting_on_author = None;
    match kind {
        PrKind::Empty | PrKind::Meta => {}
        PrKind::Mixed(_) | PrKind::Misplaced(_) => {
            waiting_on_author = Some(true);
        }
        PrKind::Packages {
            touched,
            other_versions,
            checks,
            ..
        } => {
            has_new_packages = other_versions.has_new_package();
            has_updated_packages = other_versions.has_updated_package();

            has_staff_override = checks.iter().any(|c| c.has_staff_override());

            let no_misplaced = !touched.misplaced_package_files.is_empty();
            let all_passed = checks.iter().all(|c| c.passed());
            waiting_on_review = no_misplaced && all_passed;
            waiting_on_author = Some(!waiting_on_review);
        }
    }

    let labels = {
        let mut labels = IndexSet::from_iter(pr.labels.iter().map(|l| l.name.clone()));

        // Make sure the managed labels are ordered the same way.
        let mut i = 0;
        let mut set_label = |label: &str, insert| {
            labels.shift_remove(label);
            if insert {
                labels.shift_insert(i, label.to_owned());
                i += 1;
            }
        };
        set_label("invalid", kind.is_invalid());
        set_label("meta", matches!(kind, PrKind::Meta));
        set_label("new", has_new_packages);
        set_label("update", has_updated_packages);
        set_label("staff-override", has_staff_override);
        set_label("waiting-on-review", waiting_on_review);
        if let Some(waiting) = waiting_on_author {
            set_label("waiting-on-author", waiting);
        }

        let changed = pr.labels.iter().map(|l| &l.name).ne(labels.iter());
        changed.then_some(labels)
    };

    debug!("Updating PR metadata, expected title: {title:?}, expected labels: {labels:?}");
    // Only update the PR, if needed.
    if title.is_some() || labels.is_some() {
        api_client
            .update_pull_request(repo, pr.number, PullRequestUpdate { title, labels })
            .await
            .error(Code::GithubApiPullUpdate, "Failed to update pull request")?;
    }

    Ok(())
}

/// Delete the previous review comment and post a new one.
async fn post_review_comment(
    api_client: &GitHub<AuthInstallation>,
    repo: &RepositoryId,
    pr: &PullRequest,
    previous_comments: &[Comment],
    kind: &PrKind,
) -> Result<()> {
    let comment = match kind {
        PrKind::Empty => None,
        PrKind::Meta => None,
        PrKind::Mixed(touched) => {
            let message = String::from(
                "This PR does too many things.\n\
                 A PR should either change `packages/`, \
                 or the rest of the repository, but not both.\n",
            );

            Some((message, &touched.misplaced_package_files))
        }
        PrKind::Misplaced(misplaced) => Some(("".to_string(), misplaced)),
        PrKind::Packages {
            touched,
            other_versions,
            checks,
            ..
        } => {
            let misplaced = &touched.misplaced_package_files;

            let mut message = String::new();
            if checks.iter().all(|c| c.passed()) {
                if misplaced.is_empty() {
                    message.push_str("All good! A human being will review this PR soon.");
                }
            } else {
                message.push_str(
                    "The automatic package-check failed.\n\
                     There are some issues with these packages:\n",
                );

                for check in checks {
                    if !check.passed() {
                        let pr = pr.number;
                        let name = &check.run.name;
                        let id = check.run.id;
                        writeln!(
                            message,
                            "- [{name}](https://github.com/{repo}/pull\
                             /{pr}/checks?check_run_id={id})"
                        )
                        .ok();
                    }
                }

                message.push_str(
                    "\nPlease take a look at the failing checks and deal with the errors and warnings.\n\
                     Once the automated checks pass a human being will review this PR.\n\n\
                     If you think there is an issue with the package check, please write \
                     a message and ping us by including `@typst/package-review`\n",
                );
            }

            let missing_explanation = other_versions.has_new_package()
                && (!pr.body.contains("  - Explanation:")
                    || pr.body.contains("`YOUR EXPLANATION`"));
            if missing_explanation {
                message.push_str(
                    "\nThis PR adds new packages, please provide an explanation for the package name:\n\
                     > Please write one or two sentences explaining:\n\
                     > 1. the name of your package, if necessary, e.g:\n\
                     >   > Pergamon was an ancient Greek city state in Asia Minor. \
                         Its library was second only to the Library of Alexandria around 200 BC.\n\
                     > 2. why it isn't too canonical/descriptive and complies \
                         with the naming rules, e.g:\n\
                     >   - The name is a related but creative term\n\
                     >   - The name is a neologism combining the words … and …\n\
                     >   - The name contains a non-descriptive prefix\n\
                     >   - The package is official ⇒ an email needs to be sent to hello@typst.app\n",
                );
            }

            Some((message, misplaced))
        }
    };

    // Add note about misplaced files.
    let comment = comment.map(|(mut message, misplaced)| {
        if !misplaced.is_empty() {
            message.push_str(
                "\nThere are misplaced files in the packages directory. \
                 Package files should be placed under `packages/preview/<name>/<version>/`.\n\
                 <details>\n\
                 <summary>Incorrectly placed files:</summary>\n\n",
            );
            for file in misplaced.iter() {
                writeln!(message, "  - `{}`", file.display()).ok();
            }
            message.push_str("</details>\n");
        }

        message
    });

    // Delete previous comment.
    let previous_comment = previous_comments.iter().find(|comment| {
        comment.user.login == "typst-package-check[bot]"
            && comment.body.trim().starts_with(COMMENT_TAG_REVIEW)
    });
    if let Some(comment) = previous_comment {
        debug!("Deleting old review comment, id: {:?}", comment.id);
        api_client
            .delete_pr_comment(repo, pr.number, comment.id)
            .await
            .error(
                Code::GithubApiPullCommentsDelete,
                "Failed to delete previous PR review comment",
            )?;
    }

    // Post new comment.
    if let Some(message) = comment {
        debug!("Posting review comment, message: {message:?}");
        api_client
            .post_pr_comment(repo, pr.number, format!("{COMMENT_TAG_REVIEW}\n{message}"))
            .await
            .error(
                Code::GithubApiPullCommentsPost,
                "Failed to post PR review comment",
            )?;
    }

    Ok(())
}

/// Ping authors that released the previous package versions, if not already
/// done for the PR.
///
/// This is tracked using by tagging comments made by the package check with
/// markdown comments in a specific format.
async fn post_different_authors_comment(
    api_client: &GitHub<AuthInstallation>,
    repo: &RepositoryId,
    pr: &PullRequest,
    previous_comments: &[Comment],
    mut different_authors: DifferentPackageAuthors,
) -> Result<()> {
    let mut pinged_author_packages = HashSet::new();
    for comment in previous_comments.iter() {
        if comment.user.login != "typst-package-check[bot]" {
            continue;
        }
        let Some(comment) = comment.body.trim().strip_prefix(COMMENT_TAG_AUTHOR) else {
            continue;
        };
        let list = comment
            .trim()
            .lines()
            .next()
            .and_then(|line| line.trim().strip_prefix("<!--")?.strip_suffix("-->"))
            .error(
                Code::GithubApiPullCommentsTag,
                "Expected comment tag with list of packages",
            )?;

        for package in list.trim().split(' ') {
            let spec = package.parse::<PackageSpec>().error(
                Code::GithubApiPullCommentsTag,
                "Invalid package spec in comment tag",
            )?;
            pinged_author_packages.insert(spec);
        }
    }

    // Filter out packages, for which an author has already been pinged.
    different_authors.retain(|_, packages| {
        packages.retain(|package| !pinged_author_packages.contains(package));
        !packages.is_empty()
    });

    if !different_authors.is_empty() {
        different_authors.sort_keys();

        // Tags
        let mut message = format!("{COMMENT_TAG_AUTHOR}\n<!--");
        for p in different_authors.values().flatten() {
            write!(message, " {p}").ok();
        }
        message.push_str(" -->\n");

        // Message
        for (author, packages) in different_authors.iter() {
            writeln!(message, "@{author} You released these packages:").ok();
            for package in packages.iter() {
                writeln!(message, "- {}:{}", package.name, package.version).ok();
            }
            message.push_str(
                "If you want this update to be merged, \
                 please leave a comment stating so. \
                 Without your permission, the pull request \
                 will not be merged.\n\n",
            );
        }

        debug!("Posting author comment, message: {message:?}");
        api_client
            .post_pr_comment(repo, pr.number, message)
            .await
            .error(
                Code::GithubApiPullCommentsPost,
                "Failed to post PR author comment",
            )?;
    }

    Ok(())
}

struct CheckedPackage {
    run: CheckRun,
    output: Option<Output>,
}

impl CheckedPackage {
    fn passed(&self) -> bool {
        self.output.as_ref().is_some_and(|o| o.passed())
    }

    fn has_staff_override(&self) -> bool {
        self.output.as_ref().is_some_and(|o| o.has_staff_override)
    }
}

async fn check_package(
    workspace: Workspace,
    package: PackageSpec,
) -> (Option<Output>, CheckRunOutput) {
    let res = check::all_checks(
        &workspace,
        Some(&package),
        PathBuf::from_iter([
            "packages",
            package.namespace.as_str(),
            package.name.as_str(),
            package.version.to_string().as_str(),
        ]),
        false,
    )
    .await;

    match res {
        Ok((checks, diags)) => {
            let output = codes::process_diagnostics(&checks, diags);

            // Diagnostics that can't be attached to a file (for instance because
            // they have no label or point to a directory) are listed in the
            // summary instead, so that they don't silently disappear.
            let mut annotations = Vec::new();
            let mut unplaced = Vec::new();

            let world = &checks.worlds.package;
            for diag in output.denied.all() {
                match diagnostic_to_annotation(world, &package, diag) {
                    Some(annotation) => annotations.push(annotation),
                    None => unplaced.push(diag),
                }
            }
            let suppressed = annotations.len().saturating_sub(MAX_ANNOTATIONS);
            annotations.truncate(MAX_ANNOTATIONS);

            let title = if output.denied.is_empty() {
                "All good!".to_owned()
            } else {
                let mut title = String::new();
                for severity in [Severity::Error, Severity::Warning, Severity::Note] {
                    if !title.is_empty() {
                        title.push_str(", ");
                    }
                    let num = output.denied.get(severity).len();
                    write!(title, "{num} {severity}{}", plural(num)).ok();
                }
                title
            };
            let check_run_output = CheckRunOutput {
                title,
                summary: summary(&package, &output, suppressed, &unplaced),
                annotations,
            };

            (Some(output), check_run_output)
        }
        Err(err) => {
            let check_run_output = CheckRunOutput {
                title: format!("Fatal error [{}]", err.code.ty()),
                summary: format!(
                    "The following error was encountered:\n\n{}",
                    err.format_message_with_hints()
                ),
                annotations: Vec::new(),
            };

            (None, check_run_output)
        }
    }
}

/// Builds the summary of the check run.
fn summary(
    package: &PackageSpec,
    output: &Output,
    suppressed: usize,
    unplaced: &[&Diagnostic],
) -> String {
    let errors = output.denied.errors().len();
    let warnings = output.denied.warnings().len();
    let notes = output.denied.notes().len();

    let mut summary = String::new();
    writeln!(
        summary,
        "Our bots have automatically run some checks on your packages. \
         They found {errors} error{}, {warnings} warning{}, and {notes} notes{}.\n",
        plural(errors),
        plural(warnings),
        plural(notes),
    )
    .ok();

    if !output.passed() {
        writeln!(
            summary,
            "Please either fix all errors and warnings, or explicitly \
             allow them in the `typst.toml` manifest using the \
             `[tool.package-check.lints]` configuration.\n\
             See: {}\n",
            Docs::PACKAGE_CHECK,
        )
        .ok();

        summary.push_str(
            "Once the automated checks pass, a human being will review the package.\n\
             If you think there is an issue with the package check, please write \
             a message and ping us by including `@typst/package-review`.\n",
        );
    } else {
        summary.push_str("A human being will review the package soon.\n");
    }

    if suppressed > 0 {
        writeln!(
            summary,
            "Only the first {MAX_ANNOTATIONS} annotations are shown. \
             {suppressed} additional annotation{} suppressed.\n",
            if suppressed == 1 { " was" } else { "s were" },
        )
        .ok();
    }

    if !unplaced.is_empty() {
        summary.push_str(
            "\nThere are a few diagnostics which we could \
             not show inline with your changes. These are listed here:\n",
        );
        for diag in unplaced {
            write_summary_diagnostic(&mut summary, package, diag).ok();
        }
    }

    if !output.allowed.is_empty() {
        writeln!(
            summary,
            "\n\n<details>\n\
             <summary>{} diagnostic{} were allowed</summary>\n\n",
            output.allowed.len(),
            plural(output.allowed.len()),
        )
        .ok();
        for diag in output.allowed.all() {
            write_summary_diagnostic(&mut summary, package, diag).ok();
        }
        summary.push_str("</details>\n");
    }

    summary
}

/// Emits diagnostics that could not be turned into annotations as part of the summary.
fn write_summary_diagnostic(
    mut f: impl Write,
    package: &PackageSpec,
    diag: &Diagnostic,
) -> std::fmt::Result {
    let severity = match diag.severity {
        Severity::Error => "Error",
        Severity::Warning => "Warning",
        Severity::Note => "Note",
    };
    let code = diag.code.ty().as_str();
    f.write_str("- ")?;
    if diag.code.data().is_some() {
        f.write_str("[overridable] ")?;
    };
    write!(f, "**{severity}** [`{code}`]")?;

    if let Some(label) = diag.label {
        let file_package = match label.file.root() {
            VirtualRoot::Package(p) => p,
            VirtualRoot::Project => package,
        };
        let path = label.file.vpath().get_without_slash();
        if file_package == package {
            write!(f, " in `{path}`")?;
        } else {
            write!(f, " in `{file_package}/{path}`")?;
        }
    }
    for (i, line) in diag.message.lines().enumerate() {
        let prefix = if i == 0 { ": " } else { "  " };
        writeln!(f, "{prefix}{line}")?;
    }
    for hint in diag.hints() {
        for (i, line) in hint.lines().enumerate() {
            let prefix = if i == 0 { "= hint: " } else { "  " };
            writeln!(f, "  {prefix}{line}")?;
        }
    }
    Ok(())
}

fn diagnostic_to_annotation(
    world: &SystemWorld,
    package: &PackageSpec,
    diag: &Diagnostic,
) -> Option<Annotation> {
    let label = diag.label?;
    let start_line = world.line_index(label.file, label.range.start).ok()?;
    let end_line = world.line_index(label.file, label.range.end).ok()?;
    let (start_column, end_column) = if start_line == end_line {
        let start = world
            .column_number(label.file, start_line, label.range.start)
            .ok();
        let end = world
            .column_number(label.file, start_line, label.range.end)
            .ok();
        (start, end)
    } else {
        (None, None)
    };
    let package = match label.file.root() {
        VirtualRoot::Package(p) => p,
        VirtualRoot::Project => package,
    };
    let message = diag.format_message_with_hints();
    Some(Annotation {
        path: Path::new("packages")
            .join(package.namespace.to_string())
            .join(package.name.to_string())
            .join(package.version.to_string())
            .join(label.file.vpath().get_without_slash())
            .to_str()?
            .to_owned(),
        // Lines are 1-indexed on GitHub but not for codespan
        start_line: start_line + 1,
        end_line: end_line + 1,
        start_column,
        end_column,
        annotation_level: match diag.severity {
            Severity::Note => AnnotationLevel::Notice,
            Severity::Warning => AnnotationLevel::Warning,
            Severity::Error => AnnotationLevel::Failure,
        },
        title: {
            let overridable = if diag.code.data().is_some() {
                "[overridable] "
            } else {
                ""
            };
            Some(format!("{overridable}{}", diag.code.ty()))
        },
        message,
    })
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
