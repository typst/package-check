use typst::diag::{Severity, SourceDiagnostic};
use typst::foundations::{Output, Target};
use typst_bundle::Bundle;
use typst_html::HtmlDocument;
use typst_layout::PagedDocument;

use crate::check::Diagnostics;
use crate::check::codes::Code;
use crate::check::diagnostics::{Diagnostic, Label};
use crate::check::manifest::Manifest;
use crate::world::SystemWorld;

pub fn check(diags: &mut Diagnostics, manifest: &Manifest, world: &SystemWorld) {
    for target in manifest.targets() {
        match target.val {
            Target::Paged => compile::<PagedDocument>(diags, world),
            Target::Html => compile::<HtmlDocument>(diags, world),
            Target::Bundle => compile::<Bundle>(diags, world),
        }
    }
}

fn compile<T: Output>(diags: &mut Diagnostics, world: &SystemWorld) {
    let result = typst::compile::<T>(world);
    diags.emit_many(convert_diagnostics(T::target(), world, result.warnings));

    if let Err(errors) = result.output {
        diags.emit_many(convert_diagnostics(T::target(), world, errors));
    }
}

fn convert_diagnostics<'a>(
    target: Target,
    world: &'a SystemWorld,
    iter: impl IntoIterator<Item = SourceDiagnostic> + 'a,
) -> impl Iterator<Item = Diagnostic> + 'a {
    const IGNORED: [&str; 3] = [
        "unknown font family:",
        "html export is under active development and incomplete",
        "bundle export is experimental",
    ];

    iter.into_iter()
        .filter(|d| !IGNORED.iter().any(|ignored| d.message.starts_with(ignored)))
        .map(move |diagnostic| {
            let severity = diagnostic.severity.into();
            let code = match diagnostic.severity {
                Severity::Error => Code::CompileError,
                Severity::Warning => Code::CompileWarning(diagnostic.message.clone()),
            };

            let target = match target {
                Target::Paged => "paged",
                Target::Html => "html",
                Target::Bundle => "bundle",
            };
            Diagnostic::new(severity, code)
                .with_message(format!(
                    "Compile {severity}[{target}]:\n{}",
                    diagnostic.message
                ))
                .with_hints(diagnostic.hints.iter().map(|s| s.v.to_string()))
                .with_label(Label::for_span(world, diagnostic.span))
        })
}
