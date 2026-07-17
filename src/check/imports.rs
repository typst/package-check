use std::path::{Path, PathBuf};
use std::str::FromStr;

use codespan_reporting::diagnostic::{Diagnostic, Severity};
use typst::syntax::ast::{self, AstNode, ModuleImport};
use typst::syntax::package::{PackageSpec, PackageVersion, VersionlessPackageSpec};
use walkdir::WalkDir;

use crate::check::path::PackagePath;
use crate::check::{Diagnostics, Result, TryExt, label};
use crate::world::SystemWorld;

pub fn check(diags: &mut Diagnostics, package_dir: &Path, world: &SystemWorld) -> Result<()> {
    let all_packages = world.root().all_packages();
    let entrypoint = world.root().is_package().then(|| world.entrypoint());

    for ch in WalkDir::new(package_dir).into_iter().flatten() {
        let Ok(meta) = ch.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }

        let path = PackagePath::from_full(package_dir, ch.path());
        if path.extension().is_some_and(|ext| ext == "typ") {
            let source = world
                .lookup(path.file_id())
                .error("io", "Can't read source file")?;
            check_ast(
                diags,
                world,
                source.root(),
                path.full(),
                entrypoint.as_deref(),
                all_packages,
            );
        }
    }

    Ok(())
}

pub fn check_ast(
    diags: &mut Diagnostics,
    world: &SystemWorld,
    node: &typst::syntax::SyntaxNode,
    path: &Path,
    package_entrypoint: Option<&Path>,
    all_packages: Option<&Path>,
) {
    let imports = node.children().filter_map(|ch| ch.cast::<ModuleImport>());
    for import in imports {
        let ast::Expr::Str(source_str) = import.source() else {
            continue;
        };
        let import_path = path
            .parent()
            .unwrap_or(&PathBuf::new())
            .join(source_str.get().as_str())
            .canonicalize()
            .ok();
        if package_entrypoint == import_path.as_deref() {
            diags.emit(
                Diagnostic::warning()
                    .with_labels(label(world, import.span()).into_iter().collect())
                    .with_code("import/relative")
                    .with_message(
                        "This import should use the package specification, not a relative path.",
                    ),
            )
        }

        if let Some(all_packages) = all_packages
            && let Ok(import_spec) = PackageSpec::from_str(source_str.get().as_str())
            && let Some(latest_version) =
                latest_package_version(all_packages, import_spec.versionless())
            && latest_version > import_spec.version
        {
            // Generate an error if an old version of the package is imported.
            // For other packages this usually isn't an issue, so only notify
            // package authors.
            let severity = match world.package_spec() {
                Some(spec) if import_spec.versionless() == spec.versionless() => Severity::Error,
                _ => Severity::Note,
            };
            diags.emit(
                Diagnostic::new(severity)
                    .with_labels(label(world, import.span()).into_iter().collect())
                    .with_code("import/outdated")
                    .with_message("This import seems to use an older version of the package."),
            )
        }
    }
}

fn latest_package_version(dir: &Path, spec: VersionlessPackageSpec) -> Option<PackageVersion> {
    std::fs::read_dir(dir.join(&spec.namespace[..]).join(&spec.name[..]))
        .ok()
        .and_then(|dir| {
            dir.filter_map(|child| PackageVersion::from_str(child.ok()?.file_name().to_str()?).ok())
                .max()
        })
}
