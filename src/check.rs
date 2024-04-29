use std::path::PathBuf;

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::{
    syntax::{package::PackageSpec, FileId, Span},
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

    let world = manifest::check(&package_dir, &mut diags, package_spec);
    compile::check(&mut diags, &world);
    kebab_case::check(&mut diags, &world);
    imports::check(&mut diags, &package_dir, &world);

    (world, diags)
}
/// Create a label for a span.
fn label(world: &SystemWorld, span: Span) -> Option<Label<FileId>> {
    Some(Label::primary(span.id()?, world.range(span)?))
}
