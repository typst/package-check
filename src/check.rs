use std::path::{Path, PathBuf};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::{
    syntax::{package::PackageSpec, FileId, Span, VirtualPath},
    WorldExt,
};

use crate::world::SystemWorld;

mod authors;
mod compile;
mod file_size;
mod imports;
mod kebab_case;
mod manifest;

pub fn all_checks(
    package_spec: Option<&PackageSpec>,
    package_dir: PathBuf,
) -> (SystemWorld, Diagnostics) {
    let mut diags = Diagnostics::default();

    let worlds = manifest::check(&package_dir, &mut diags, package_spec);
    compile::check(&mut diags, &worlds.package);
    if let Some(template_world) = worlds.template {
        let mut template_diags = Diagnostics::default();
        compile::check(&mut template_diags, &template_world);
        let template_dir = template_world
            .root()
            .strip_prefix(worlds.package.root())
            .expect("Template should be in a subfolder of the package");
        diags.extend(template_diags, template_dir);
    }
    kebab_case::check(&mut diags, &worlds.package);
    imports::check(&mut diags, &package_dir, &worlds.package);
    if let Some(spec) = package_spec {
        authors::check(&mut diags, spec);
    }

    (worlds.package, diags)
}

#[derive(Default, Debug)]
pub struct Diagnostics {
    pub warnings: Vec<Diagnostic<FileId>>,
    pub errors: Vec<Diagnostic<FileId>>,
}

impl Diagnostics {
    fn extend(&mut self, mut other: Self, dir_prefix: &Path) {
        let fix_labels = |diag: &mut Diagnostic<FileId>| {
            for label in diag.labels.iter_mut() {
                if label.file_id.package().is_none() {
                    label.file_id = FileId::new(
                        None,
                        VirtualPath::new(dir_prefix.join(label.file_id.vpath().as_rootless_path())),
                    )
                }
            }
        };

        other.errors.iter_mut().for_each(fix_labels);
        self.errors.extend(other.errors);

        other.warnings.iter_mut().for_each(fix_labels);
        self.warnings.extend(other.warnings);
    }
}

/// Create a label for a span.
fn label(world: &SystemWorld, span: Span) -> Option<Label<FileId>> {
    Some(Label::primary(span.id()?, world.range(span)?))
}
