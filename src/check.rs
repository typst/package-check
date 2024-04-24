use std::path::PathBuf;

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::{
    syntax::{package::PackageSpec, FileId, Span},
    WorldExt,
};

use crate::world::SystemWorld;

mod compile;
mod file_size;
mod kebab_case;
mod manifest;

#[derive(Default)]
pub struct Diagnostics {
    pub warnings: Vec<Diagnostic<FileId>>,
    pub errors: Vec<Diagnostic<FileId>>,
}

pub fn all_checks(
    packages_root: PathBuf,
    package_spec: &PackageSpec,
) -> (SystemWorld, Diagnostics) {
    let mut diags = Diagnostics::default();
    let package_dir = packages_root
        .join(package_spec.namespace.to_string())
        .join(package_spec.name.to_string())
        .join(package_spec.version.to_string());

    let world = manifest::check(package_dir, &mut diags, package_spec);
    compile::check(&mut diags, &world);
    kebab_case::check(&mut diags, &world);

    (world, diags)
}
/// Create a label for a span.
fn label(world: &SystemWorld, span: Span) -> Option<Label<FileId>> {
    Some(Label::primary(span.id()?, world.range(span)?))
}
