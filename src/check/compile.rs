use codespan_reporting::diagnostic::Diagnostic;
use typst::{
    diag::{Severity, SourceDiagnostic},
    eval::Tracer,
    model::Document,
    syntax::FileId,
};

use crate::world::SystemWorld;

use super::{label, Diagnostics};

pub fn check(diags: &mut Diagnostics, world: &SystemWorld) -> Option<Document> {
    let mut tracer = Tracer::new();
    let result = typst::compile(world, &mut tracer);
    diags
        .warnings
        .extend(convert_diagnostics(world, tracer.warnings()));

    match result {
        Ok(doc) => Some(doc),
        Err(errors) => {
            diags.errors.extend(convert_diagnostics(world, errors));
            None
        }
    }
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
