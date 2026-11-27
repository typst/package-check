use std::io::Write;

use codespan_reporting::files::Files;
use serde::Serialize;
use tracing::error;

use crate::check::codes::Output;
use crate::check::diagnostics::Diagnostic;
use crate::world::SystemWorld;

#[derive(Serialize)]
struct JsonDiagnostic<'a> {
    kind: &'a str,
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    hints: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<Location>,
}

#[derive(Serialize)]
struct Location {
    file: String,
    from: LineCol,
    to: LineCol,
}

#[derive(Serialize)]
struct LineCol {
    line: usize,
    column: usize,
}

/// Print output as a JSON object.
pub fn write_output(
    mut w: impl Write,
    world: &SystemWorld,
    output: &Output,
) -> std::io::Result<()> {
    w.write_all(br#"{"denied":["#)?;
    for (i, diag) in output.denied.all().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        write_diagnostic(&mut w, world, diag)?;
    }
    w.write_all(br#"],"allowed":["#)?;
    for (i, diag) in output.allowed.all().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        write_diagnostic(&mut w, world, diag)?;
    }
    w.write_all(b"]}\n")?;

    Ok(())
}

/// Emit a single diagnostic as JSON object.
pub fn write_diagnostic(
    w: impl Write,
    world: &SystemWorld,
    diag: &Diagnostic,
) -> std::io::Result<()> {
    let location = diag
        .label
        .map(|label| -> Result<_, codespan_reporting::files::Error> {
            let file = label.file;
            let start_line = world.line_index(file, label.range.start)?;
            let end_line = world.line_index(file, label.range.end)?;

            Ok(Location {
                file: world.name(file)?.to_string(),
                from: LineCol {
                    line: world.line_number(file, start_line)?,
                    column: world.column_number(file, start_line, label.range.start)?,
                },
                to: LineCol {
                    line: world.line_number(file, end_line)?,
                    column: world.column_number(file, end_line, label.range.end)?,
                },
            })
        });

    let location = location.and_then(|res| {
        if let Err(err) = &res {
            error!("Failed to resolve label ({err}):\n{diag:#?}");
        }
        res.ok()
    });
    write_json(
        w,
        &JsonDiagnostic {
            kind: diag.severity.as_str(),
            location,
            message: &diag.message,
            hints: diag.hints().collect(),
            code: diag.code.ty().as_str(),
        },
    )
}

#[derive(Serialize)]
struct FatalError<'a> {
    code: &'a str,
    message: &'a str,
}

/// Print this diagnostic as a fatal error to the writer.
pub fn write_fatal_error(mut w: impl Write, diag: &Diagnostic) -> std::io::Result<()> {
    let fatal = FatalError {
        code: diag.code.ty().as_str(),
        message: &diag.format_message_with_hints(),
    };
    w.write_all(br#"{"fatal":"#)?;
    write_json(&mut w, &fatal)?;
    w.write_all(b"}\n")
}

fn write_json(w: impl Write, value: &impl Serialize) -> std::io::Result<()> {
    serde_json::to_writer(w, value).map_err(|err| err.io_error_kind().unwrap().into())
}
