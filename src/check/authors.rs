use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::syntax::{package::PackageSpec, FileId, VirtualPath};

use crate::{github::git, package::PackageExt};

use super::Diagnostics;

pub async fn check(diags: &mut Diagnostics, spec: &PackageSpec) -> Option<()> {
    if authors_are_differents(spec).await.unwrap_or(false) {
        let manifest = FileId::new(None, VirtualPath::new("typst.toml"));

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

pub async fn commit_for_previous_version(spec: &PackageSpec) -> Option<String> {
    let last_manifest = spec.previous_version()?.directory().join("typst.toml");

    let repo = git::repo_dir();
    let repo = git::GitRepo::open(&repo).await.ok()?;

    repo.commit_for_file(&last_manifest).await
}

pub async fn authors_are_differents(spec: &PackageSpec) -> Option<bool> {
    let last_manifest = spec.previous_version()?.directory().join("typst.toml");
    let new_manifest = spec.directory().join("typst.toml");

    let repo = git::repo_dir();
    let repo = git::GitRepo::open(&repo).await.ok()?;

    let last_authors = repo.authors_of(&last_manifest).await?;
    let new_authors = repo.authors_of(&new_manifest).await?;
    Some(
        !last_authors.is_empty()
            && !new_authors.is_empty()
            && last_authors.intersection(&new_authors).next().is_none(),
    )
}
