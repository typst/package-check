use std::path::{Path, PathBuf};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::syntax::{
    package::{PackageSpec, PackageVersion},
    FileId, VirtualPath,
};

use crate::github::git;

use super::Diagnostics;

pub fn check(diags: &mut Diagnostics, spec: &PackageSpec) -> Option<()> {
    let all_versions_dir = PathBuf::new()
        .join(spec.namespace.as_str())
        .join(spec.name.as_str());
    let mut last_version = None;
    for version_dir in std::fs::read_dir(&all_versions_dir).ok()? {
        let Ok(version_dir) = version_dir else {
            continue;
        };

        let Some(version) = version_dir
            .file_name()
            .to_str()
            .and_then(|v| v.parse::<PackageVersion>().ok())
        else {
            continue;
        };

        if version == spec.version {
            continue;
        }

        if last_version.map(|last| last < version).unwrap_or(true) {
            last_version = Some(version);
        }
    }

    if let Some(last_version) = last_version {
        let repo_path = std::env::var("PACKAGES_DIR").unwrap_or("..".to_owned());
        let repo = git::GitRepo::open(Path::new(&repo_path));

        let last_manifest = all_versions_dir
            .join(last_version.to_string())
            .join("typst.toml");
        let new_manifest = all_versions_dir
            .join(spec.version.to_string())
            .join("typst.toml");

        let last_authors = repo.authors_of(&last_manifest)?;
        let new_authors = repo.authors_of(&new_manifest)?;
        if !last_authors.is_empty()
            && !new_authors.is_empty()
            && last_authors.intersection(&new_authors).next().is_none()
        {
            let manifest = FileId::new(None, VirtualPath::new("typst.toml"));

            diags
                .warnings
                .push(
                    Diagnostic::warning().with_labels(vec![Label::primary(manifest, 0..0)])
                    .with_message(
                        "The authors of this version are not the same as those of the previous one."
                )
            );
        }
    }

    Some(())
}
