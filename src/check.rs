use std::path::PathBuf;

use codespan_reporting::diagnostic::{Diagnostic, Label};
use typst::{
    diag::{Severity, SourceDiagnostic},
    syntax::{package::PackageSpec, FileId, Span},
    WorldExt,
};

use crate::world::SystemWorld;

mod compile;
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
    let (_, _, world) = manifest::check(packages_root, &mut diags, package_spec);
    compile::check(&mut diags, &world);
    kebab_case::check(&mut diags, &world);

    (world, diags)
}

fn convert_diagnostics<'a>(
    world: &'a SystemWorld,
    iter: impl IntoIterator<Item = SourceDiagnostic> + 'a,
) -> impl Iterator<Item = Diagnostic<FileId>> + 'a {
    iter.into_iter().map(|diagnostic| {
        match diagnostic.severity {
            Severity::Error => Diagnostic::error(),
            Severity::Warning => Diagnostic::warning(),
        }
        .with_message(format!(
            "The following error was reported by the Typst compiler: {}",
            diagnostic.message
        ))
        .with_labels(label(world, diagnostic.span).into_iter().collect())
    })
}

/// Create a label for a span.
fn label(world: &SystemWorld, span: Span) -> Option<Label<FileId>> {
    Some(Label::primary(span.id()?, world.range(span)?))
}
