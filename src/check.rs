use std::path::PathBuf;

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::{
    syntax::{package::PackageSpec, FileId, Span, VirtualPath},
    WorldExt,
};

use crate::world::SystemWorld;

mod compile;
mod file_size;
mod imports;
mod kebab_case;
mod manifest;

#[derive(Default)]
pub struct Diagnostics {
    pub warnings: Vec<Diagnostic<FileId>>,
    pub errors: Vec<Diagnostic<FileId>>,
}

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

        let fix_labels = |diag: &mut Diagnostic<FileId>| {
            for label in diag.labels.iter_mut() {
                label.file_id = FileId::new(
                    label.file_id.package().cloned(),
                    VirtualPath::new(template_dir.join(label.file_id.vpath().as_rootless_path())),
                )
            }
        };

        template_diags.errors.iter_mut().for_each(fix_labels);
        diags.errors.extend(template_diags.errors);

        template_diags.warnings.iter_mut().for_each(fix_labels);
        diags.warnings.extend(template_diags.warnings);
    }
    kebab_case::check(&mut diags, &worlds.package);
    imports::check(&mut diags, &package_dir, &worlds.package);

    (worlds.package, diags)
}
/// Create a label for a span.
fn label(world: &SystemWorld, span: Span) -> Option<Label<FileId>> {
    Some(Label::primary(span.id()?, world.range(span)?))
}
