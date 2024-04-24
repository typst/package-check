use std::path::PathBuf;

use codespan_reporting::diagnostic::{Diagnostic, Label};
use ecow::eco_format;
use tracing::debug;
use typst::{
    syntax::{
        package::{PackageManifest, PackageSpec},
        FileId, VirtualPath,
    },
    World,
};

use crate::world::SystemWorld;

use super::Diagnostics;

pub fn check(
    packages_root: PathBuf,
    diags: &mut Diagnostics,
    package_spec: &PackageSpec,
) -> (PathBuf, PackageManifest, SystemWorld) {
    let package_dir = packages_root
        .join(package_spec.namespace.to_string())
        .join(package_spec.name.to_string())
        .join(package_spec.version.to_string());
    let manifest_path = package_dir.join("typst.toml");
    debug!("Reading manifest at {}", &manifest_path.display());
    let manifest_contents = std::fs::read_to_string(manifest_path).unwrap();
    let manifest: PackageManifest = toml::from_str(&manifest_contents).unwrap();

    let entrypoint = package_dir.join(manifest.package.entrypoint.to_string());
    let world = SystemWorld::new(entrypoint, package_dir.clone())
        .map_err(|err| eco_format!("{err}"))
        .unwrap();

    let manifest_file_id = FileId::new(None, VirtualPath::new("typst.toml"));
    world.file(manifest_file_id).ok(); // TODO: is this really necessary?

    let mut manifest_lines = manifest_contents.lines().scan(0, |start, line| {
        let end = *start + line.len();
        let range = *start..end;
        *start = end + 1;
        Some((line, range))
    });
    let name_line = manifest_lines
        .clone()
        .find(|(l, _)| l.trim().starts_with("name ="));
    let version_line = manifest_lines.find(|(l, _)| l.trim().starts_with("version ="));

    if manifest.package.name != package_spec.name {
        diags.errors.push(
            Diagnostic::error()
                .with_labels(vec![Label::new(
                    codespan_reporting::diagnostic::LabelStyle::Primary,
                    manifest_file_id,
                    name_line.map(|l| l.1).unwrap_or_default(),
                )])
                .with_message(format!(
                    "Unexpected package name. `{name}` was expected. If you want to publish a new package, create a new directory in `packages/{namespace}/`.",
                    name = package_spec.name,
                    namespace = package_spec.namespace,
                )),
        )
    }

    if manifest.package.version != package_spec.version {
        diags.errors.push(
            Diagnostic::error()
                .with_labels(vec![Label::new(
                    codespan_reporting::diagnostic::LabelStyle::Primary,
                    manifest_file_id,
                    version_line.map(|l| l.1).unwrap_or_default(),
                )])
                .with_message(format!(
                    "Unexpected version number. `{version}` was expected. If you want to publish a new version, create a new directory in `packages/{namespace}/{name}`.",
                    version = package_spec.version,
                    name = package_spec.name,
                    namespace = package_spec.namespace,
                )),
        )
    }

    // TODO: other common checks

    (package_dir, manifest, world)
}
