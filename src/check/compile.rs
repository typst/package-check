use typst::diag::{Severity, SourceDiagnostic};
use typst_layout::PagedDocument;

use crate::check::diagnostics::{Code, Diagnostic, Label};
use crate::world::SystemWorld;

use super::Diagnostics;

pub fn check(diags: &mut Diagnostics, world: &SystemWorld) -> Option<PagedDocument> {
    let result = typst::compile(world);
    diags.emit_many(convert_diagnostics(world, result.warnings));

    match result.output {
        Ok(doc) => Some(doc),
        Err(errors) => {
            diags.emit_many(convert_diagnostics(world, errors));
            None
        }
    }
}

fn convert_diagnostics<'a>(
    world: &'a SystemWorld,
    iter: impl IntoIterator<Item = SourceDiagnostic> + 'a,
) -> impl Iterator<Item = Diagnostic> + 'a {
    iter.into_iter()
        .filter(|diagnostic| !diagnostic.message.starts_with("unknown font family:"))
        .map(|diagnostic| {
            let severity = diagnostic.severity.into();
            let code = match diagnostic.severity {
                Severity::Error => Code::CompileError,
                Severity::Warning => Code::CompileWarning,
            };

            Diagnostic::new(severity, code)
                .with_message(format!(
                    "The following {severity} was reported by the Typst compiler:\n{}",
                    diagnostic.message
                ))
                .with_hints(diagnostic.hints.iter().map(|s| s.v.to_string()))
                .with_label(Label::for_span(world, diagnostic.span))
        })
}
