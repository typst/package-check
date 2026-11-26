use std::path::PathBuf;

use typst::syntax::package::PackageSpec;

use crate::check::files::Files;
use crate::check::manifest::{Manifest, Worlds};
use crate::check::readme::Readme;
use crate::workspace::Workspace;

pub mod codes;
mod compile;
pub mod diagnostics;
mod files;
mod imports;
mod kebab_case;
mod manifest;
mod path;
mod readme;
mod toml;
pub mod urls;

pub use diagnostics::{Diagnostics, Result, TryExt};
pub use manifest::Exclude;

pub struct Checks {
    pub worlds: Worlds,
    pub manifest: Manifest,
    pub readme: Option<Readme>,
    pub files: Files,
}

pub async fn all_checks(
    workspace: &Workspace,
    package_spec: Option<&PackageSpec>,
    package_dir: PathBuf,
    offline: bool,
) -> Result<(Checks, Diagnostics)> {
    let (checks, mut diags) = offline_checks(workspace, package_spec, package_dir)?;
    if !offline {
        // TODO: consider checking a fixed number of readme links.
        urls::check(&mut diags, &checks.manifest).await;
    }

    diags.sort();

    Ok((checks, diags))
}

pub fn offline_checks(
    workspace: &Workspace,
    package_spec: Option<&PackageSpec>,
    package_dir: PathBuf,
) -> Result<(Checks, Diagnostics)> {
    let mut diags = Diagnostics::default();

    let (manifest, worlds) = manifest::check(workspace, &package_dir, &mut diags, package_spec)?;

    let res = readme::check(&worlds.package, &mut diags);
    let readme = diags.maybe_emit(res);

    let files = files::check(&mut diags, &package_dir, &manifest, &readme);

    compile::check(&mut diags, &manifest, &worlds.package);
    kebab_case::check_project(&worlds.package, &mut diags);
    if let Some(template_world) = &worlds.template {
        let mut template_diags = Diagnostics::default();
        compile::check(&mut template_diags, &manifest, template_world);
        kebab_case::check_project(template_world, &mut template_diags);

        let template_dir = template_world
            .root()
            .relative_template_dir()
            .expect("Template should be in a subfolder of the package");
        diags.extend(template_diags, template_dir);
    }

    let res = imports::check(&mut diags, &package_dir, &worlds.package);
    diags.maybe_emit(res);

    diags.sort();

    let checks = Checks {
        worlds,
        manifest,
        readme,
        files,
    };
    Ok((checks, diags))
}
