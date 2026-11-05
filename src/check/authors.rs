use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::syntax::{FileId, VirtualPath, package::PackageSpec};
use typst::syntax::{RootedPath, VirtualRoot};

use crate::github::git;
use crate::workspace::Workspace;

use super::Diagnostics;

pub fn check(diags: &mut Diagnostics, workspace: &Workspace, spec: &PackageSpec) -> Option<()> {
    if authors_are_differents(workspace, spec).unwrap_or(false) {
        let manifest = FileId::new(RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new("typst.toml").ok()?,
        ));

        diags.emit(
                Diagnostic::warning()
                    .with_labels(vec![Label::primary(manifest, 0..0)])
                    .with_message(
                        "The authors of this version are not the same as those of the previous one (according to Git)."
                    )
                    .with_code("authors/changed")
            );
    }

    Some(())
}

pub fn commit_for_previous_version(workspace: &Workspace, spec: &PackageSpec) -> Option<String> {
    let prev_version = workspace.find_previous_version(spec)?;
    let last_manifest = workspace.path_in_repo(&prev_version).join("typst.toml");

    let repo = git::GitRepo::open(&workspace.packages_repo_dir).ok()?;

    repo.commit_for_file(&last_manifest)
}

pub fn authors_are_differents(workspace: &Workspace, spec: &PackageSpec) -> Option<bool> {
    let prev_version = workspace.find_previous_version(spec)?;
    let last_manifest = workspace.path_in_repo(&prev_version).join("typst.toml");
    let new_manifest = workspace.path_in_repo(spec).join("typst.toml");

    let repo = git::GitRepo::open(&workspace.packages_repo_dir).ok()?;

    let last_authors = repo.authors_of(&last_manifest)?;
    let new_authors = repo.authors_of(&new_manifest)?;
    Some(
        !last_authors.is_empty()
            && !new_authors.is_empty()
            && last_authors.intersection(&new_authors).next().is_none(),
    )
}
