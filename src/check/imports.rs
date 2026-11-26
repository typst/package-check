use std::path::Path;
use std::str::FromStr;

use typst::World;
use typst::ecow::eco_format;
use typst::syntax::VirtualPath;
use typst::syntax::ast::{self, AstNode, ModuleImport};
use typst::syntax::package::{PackageSpec, PackageVersion, VersionlessPackageSpec};

use crate::check::codes::Code;
use crate::check::diagnostics::{Diagnostic, Docs, Label};
use crate::check::path::PackagePath;
use crate::check::{Diagnostics, TryExt, files};
use crate::world::SystemWorld;

pub fn check(
    diags: &mut Diagnostics,
    package_dir: &Path,
    world: &SystemWorld,
) -> crate::check::Result<()> {
    for ch in files::walk(package_dir).flatten() {
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
                .error(Code::Io, "Can't read source file")?;
            check_ast(diags, world, source.root(), path.relative());
        }
    }

    Ok(())
}

pub fn check_ast(
    diags: &mut Diagnostics,
    world: &SystemWorld,
    node: &typst::syntax::SyntaxNode,
    relative_path: &Path,
) {
    let imports = node.children().filter_map(|ch| ch.cast::<ModuleImport>());
    for import in imports {
        let ast::Expr::Str(source_str) = import.source() else {
            continue;
        };

        // Normalize the import path using the virutal path constructor.
        let import_path = VirtualPath::new(
            relative_path
                .parent()
                .unwrap_or(Path::new(""))
                .join(source_str.get().as_str())
                .to_str()
                .expect("This should be valid UTF-8"),
        );
        if let Ok(import_path) = import_path
            && world.root().is_package()
            && &import_path == world.main().vpath()
        {
            Diagnostic::warn(Code::ImportRelative)
                .with_message(
                    "This import should use the package specification, \
                     not a relative path.",
                )
                .with_docs_link(Docs::SPEC_IMPORTS)
                .with_label(Label::for_span(world, import.span()))
                .emit(diags);
        }

        if let Some(all_packages) = world.root().all_packages()
            && let Ok(import_spec) = PackageSpec::from_str(source_str.get().as_str())
            && let Some(latest_version) =
                latest_package_version(all_packages, import_spec.versionless())
            && latest_version > import_spec.version
        {
            // Generate an error if an old version of the package is imported.
            // For other packages this usually isn't an issue, so only notify
            // package authors.
            match world.package_spec() {
                Some(spec) if import_spec.versionless() == spec.versionless() => {
                    Diagnostic::error(Code::ImportOutdatedSelf)
                        .with_message("This import seems to use an older version of the package.")
                        .with_label(Label::for_span(world, import.span()))
                        .emit(diags);
                }
                _ => {
                    Diagnostic::note(Code::ImportOutdatedDependency(eco_format!("{import_spec}")))
                        .with_message("This import seems to use an older version of the package.")
                        .with_label(Label::for_span(world, import.span()))
                        .emit(diags);
                }
            }
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
