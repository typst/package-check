use std::collections::HashSet;

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::syntax::package::PackageVersion;
use typst::syntax::{FileId, VirtualPath, package::PackageSpec};
use typst::syntax::{RootedPath, VirtualRoot};

use crate::display;
use crate::github::git;
use crate::workspace::Workspace;

use super::Diagnostics;

pub fn check(diags: &mut Diagnostics, workspace: &Workspace, spec: &PackageSpec) -> Option<()> {
    if let Some((version, [a, b])) = authors_are_differents(workspace, spec) {
        let manifest = FileId::new(RootedPath::new(
            VirtualRoot::Project,
            VirtualPath::new("typst.toml").ok()?,
        ));

        // TODO: Add test for changed name but same email or vice versa.
        let a = display::sorted_list(a);
        let b = display::sorted_list(b);
        diags.emit(
            Diagnostic::warning()
                .with_labels(vec![Label::primary(manifest, 0..0)])
                .with_message(format!(
                    "The authors of this version are not the same as those \
                     of the previous version `{version}` (according to Git):\n\
                     previous: {a}\n\
                     current:  {b}"
                ))
                .with_code("authors/changed"),
        );
    }

    Some(())
}

pub fn commit_for_previous_version(workspace: &Workspace, spec: &PackageSpec) -> Option<String> {
    let prev_version = workspace.find_previous_version(spec)?;
    let last_manifest = workspace.path_in_repo(&prev_version).join("typst.toml");

    let repo = git::GitRepo::open(workspace.repo_dir()).ok()?;

    repo.commit_for_file(&last_manifest)
}

pub fn authors_are_differents(
    workspace: &Workspace,
    spec: &PackageSpec,
) -> Option<(PackageVersion, [HashSet<String>; 2])> {
    let prev_version = workspace.find_previous_version(spec)?;
    let last_manifest = workspace.path_in_repo(&prev_version).join("typst.toml");
    let new_manifest = workspace.path_in_repo(spec).join("typst.toml");

    let repo = git::GitRepo::open(workspace.repo_dir()).ok()?;

    let last_authors = repo.authors_of(&last_manifest)?;
    let new_authors = repo.authors_of(&new_manifest)?;

    let changed = !last_authors.is_empty()
        && !new_authors.is_empty()
        && last_authors.is_disjoint(&new_authors);
    changed.then_some((prev_version.version, [last_authors, new_authors]))
}
