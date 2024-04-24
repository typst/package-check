use std::path::{Path, PathBuf};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use ecow::eco_format;
use globset::{Glob, GlobSet};
use serde::Deserialize;
use tracing::debug;
use typst::{
    syntax::{
        package::{PackageSpec, PackageVersion},
        FileId, VirtualPath,
    },
    World,
};

use crate::{check::file_size, world::SystemWorld};

use super::Diagnostics;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Discipline {
    Agriculture,
    Anthropology,
    Archaeology,
    Architecture,
    Biology,
    Business,
    Chemistry,
    Communication,
    ComputerScience,
    Design,
    Drawing,
    Economics,
    Education,
    Engineering,
    Fashion,
    Film,
    Geography,
    Geology,
    History,
    Journalism,
    Law,
    Linguistics,
    Literature,
    Mathematics,
    Medicine,
    Music,
    Painting,
    Philosophy,
    Photography,
    Physics,
    Politics,
    Psychology,
    Sociology,
    Theater,
    Theology,
    Transportation,
}

#[derive(Deserialize)]
pub enum Category {
    Components,
    Visualization,
    Model,
    Layout,
    Text,
    Languages,
    Scripting,
    Integration,
    Utility,
    Fun,
    Book,
    Report,
    Paper,
    Thesis,
    Poster,
    Flyer,
    Presentation,
    Cv,
    Office,
}

pub fn check(
    package_dir: PathBuf,
    diags: &mut Diagnostics,
    package_spec: &PackageSpec,
) -> SystemWorld {
    let manifest_path = package_dir.join("typst.toml");
    debug!("Reading manifest at {}", &manifest_path.display());
    let manifest_contents = std::fs::read_to_string(manifest_path).unwrap();
    let manifest = toml_edit::ImDocument::parse(&manifest_contents).unwrap();

    let entrypoint = package_dir.join(manifest["package"]["entrypoint"].as_str().unwrap());
    let world = SystemWorld::new(entrypoint, package_dir.clone())
        .map_err(|err| eco_format!("{err}"))
        .unwrap();

    let manifest_file_id = FileId::new(None, VirtualPath::new("typst.toml"));
    world.file(manifest_file_id).ok(); // TODO: is this really necessary?

    let name = &manifest["package"]["name"];
    if name.as_str().unwrap() != package_spec.name {
        diags.errors.push(
            Diagnostic::error()
                .with_labels(vec![Label::new(
                    codespan_reporting::diagnostic::LabelStyle::Primary,
                    manifest_file_id,
                    name.span().unwrap_or_default()
                )])
                .with_message(format!(
                    "Unexpected package name. `{name}` was expected. If you want to publish a new package, create a new directory in `packages/{namespace}/`.",
                    name = package_spec.name,
                    namespace = package_spec.namespace,
                )),
        )
    }

    let version = &manifest["package"]["version"];
    if version.as_str().unwrap().parse::<PackageVersion>().unwrap() != package_spec.version {
        diags.errors.push(
            Diagnostic::error()
                .with_labels(vec![Label::new(
                    codespan_reporting::diagnostic::LabelStyle::Primary,
                    manifest_file_id,
                    version.span().unwrap_or_default(),
                )])
                .with_message(format!(
                    "Unexpected version number. `{version}` was expected. If you want to publish a new version, create a new directory in `packages/{namespace}/{name}`.",
                    version = package_spec.version,
                    name = package_spec.name,
                    namespace = package_spec.namespace,
                )),
        )
    }

    exclude_large_files(diags, &package_dir, &manifest);

    // TODO: other common checks

    world
}

fn exclude_large_files(
    diags: &mut Diagnostics,
    package_dir: &Path,
    manifest: &toml_edit::ImDocument<&String>,
) {
    let empty_array = toml_edit::Array::new();
    let mut exclude_globs = GlobSet::builder();
    let exclude = manifest["package"]
        .get("exclude")
        .and_then(|item| item.as_array())
        .unwrap_or(&empty_array);
    for glob in exclude {
        exclude_globs.add(Glob::new(glob.as_str().unwrap()).unwrap());
    }
    let exclude_globs = exclude_globs.build().unwrap();

    const REALLY_LARGE: u64 = 50 * 1024 * 1024;

    let large_files = file_size::find_large_files(package_dir);
    for (path, size) in large_files {
        let fid = FileId::new(None, VirtualPath::new(&path));

        let message = if size > REALLY_LARGE {
            format!(
                "This file is really large ({size}MB). If possible, do not include it in this repository at all.",
                size = size / 1024 / 1024
            )
        } else if !exclude_globs.is_match(path) {
            format!(
                "This file is quite large ({size}MB). If it is not required to use the package (i.e. it is a documentation file, or part of an example), it should be added to `exclude` in your `typst.toml`.",
                size = size / 1024 / 1024
            )
        } else {
            continue;
        };

        diags.warnings.push(
            Diagnostic::warning()
                .with_labels(vec![Label::new(
                    codespan_reporting::diagnostic::LabelStyle::Primary,
                    fid,
                    0..0,
                )])
                .with_message(message),
        )
    }
}
