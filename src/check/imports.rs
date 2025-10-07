use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use codespan_reporting::diagnostic::Diagnostic;
use typst::{
    syntax::{
        ast::{self, AstNode, ModuleImport},
        package::{PackageSpec, PackageVersion, VersionlessPackageSpec},
        FileId, VirtualPath,
    },
    World,
};

use crate::{
    check::{label, TryExt},
    world::SystemWorld,
};

use super::{Diagnostics, Result};

pub fn check(diags: &mut Diagnostics, package_dir: &Path, world: &SystemWorld) -> Result<()> {
    check_dir(diags, package_dir, world)
}

pub fn check_dir(diags: &mut Diagnostics, dir: &Path, world: &SystemWorld) -> Result<()> {
    let root_path = world.root();
    let main_path = root_path
        .join(world.main().vpath().as_rootless_path())
        .canonicalize()
        .ok();
    let all_packages = root_path
        .parent()
        .and_then(|package_dir| package_dir.parent())
        .and_then(|namespace_dir| namespace_dir.parent());

    for ch in std::fs::read_dir(dir).error("internal/io", "Can't read directory")? {
        let Ok(ch) = ch else {
            continue;
        };
        let Ok(meta) = ch.metadata() else {
            continue;
        };

        let path = dir.join(ch.file_name());
        if meta.is_dir() {
            check_dir(diags, &path, world)?;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("typ") {
            let fid = FileId::new(
                None,
                VirtualPath::new(
                    path.strip_prefix(root_path)
                        // Not actually true
                        .error(
                            "internal",
                            "Prefix striping failed even though `path` is built from `root_dir`",
                        )?,
                ),
            );
            let source = world.lookup(fid).error("io", "Can't read source file")?;
            check_ast(
                diags,
                world,
                source.root(),
                &path,
                main_path.as_deref(),
                all_packages,
            );
        }
    }

    Ok(())
}

pub fn check_ast(
    diags: &mut Diagnostics,
    world: &SystemWorld,
    root: &typst::syntax::SyntaxNode,
    path: &Path,
    main_path: Option<&Path>,
    all_packages: Option<&Path>,
) {
    let imports = root.children().filter_map(|ch| ch.cast::<ModuleImport>());
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
        if main_path == import_path.as_deref() {
            diags.emit(
                Diagnostic::warning()
                    .with_labels(label(world, import.span()).into_iter().collect())
                    .with_code("import/relative")
                    .with_message(
                        "This import should use the package specification, not a relative path.",
                    ),
            )
        }

        if let Some(all_packages) = all_packages {
            if let Ok(import_spec) = PackageSpec::from_str(source_str.get().as_str()) {
                if let Some(latest_version) =
                    latest_package_version(all_packages, import_spec.versionless())
                {
                    if latest_version != import_spec.version {
                        diags.emit(
                            Diagnostic::warning()
                                .with_labels(label(world, import.span()).into_iter().collect())
                                .with_code("import/outdated")
                                .with_message(
                                    "This import seems to use an older version of the package.",
                                ),
                        )
                    }
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
