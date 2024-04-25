use std::path::{Path, PathBuf};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::{
    syntax::{
        ast::{self, AstNode, ModuleImport},
        FileId, VirtualPath,
    },
    World, WorldExt,
};

use crate::world::SystemWorld;

use super::Diagnostics;

pub fn check(diags: &mut Diagnostics, package_dir: &Path, world: &SystemWorld) {
    check_dir(diags, package_dir, world);
}

pub fn check_dir(diags: &mut Diagnostics, dir: &Path, world: &SystemWorld) -> Option<()> {
    let root_path = world.root();
    let main_path = root_path
        .join(world.main().id().vpath().as_rootless_path())
        .canonicalize()
        .ok();

    for ch in std::fs::read_dir(dir).ok()? {
        let Ok(ch) = ch else {
            continue;
        };
        let Ok(meta) = ch.metadata() else {
            continue;
        };

        let path = dir.join(ch.file_name());
        if meta.is_dir() {
            return check_dir(diags, &path, world);
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("typ") {
            let fid = FileId::new(
                None,
                VirtualPath::new(&path.strip_prefix(&root_path).unwrap()),
            );
            let source = world.lookup(fid).ok()?;
            let imports = source
                .root()
                .children()
                .filter_map(|ch| ch.cast::<ModuleImport>());
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
                if main_path == import_path {
                    diags
                        .warnings
                        .push(Diagnostic::warning()
                            .with_labels(vec![Label::primary(
                                fid,
                                world.range(import.span()).unwrap(),
                            )])
                            .with_message(
                                "This import should use the package specification, not a relative path."
                            )
                        )
                }
            }
        }
    }

    Some(())
}
