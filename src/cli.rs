use std::io::Write;

use codespan_reporting::term;
use typst::syntax::VirtualRoot;
use typst::syntax::{FileId, Source};

use crate::check::{Diagnostics, Exclude};
use crate::world::SystemWorld;

/// Print diagnostic messages to the terminal.
pub fn print_diagnostics(
    world: SystemWorld,
    diags: &Diagnostics,
    json: bool,
) -> Result<(), codespan_reporting::files::Error> {
    let config = term::Config {
        tab_width: 2,
        ..Default::default()
    };

    // We should be able to print diagnostics even on excluded files. If we
    // don't remove the exclusion, it will fail to read and display the file
    // contents.
    let mut world = world.exclude(Exclude::empty());

    let buf = if json {
        let mut buf = Vec::new();
        for diagnostic in diags.all().rev() {
            json::emit(&mut buf, &mut world, diagnostic)?;
        }
        buf
    } else {
        let mut buf =
            term::termcolor::BufferWriter::stdout(term::termcolor::ColorChoice::Auto).buffer();
        for diagnostic in diags.all().rev() {
            term::emit_to_write_style(&mut buf, &config, &world, &diagnostic.into())?;
        }
        buf.into_inner()
    };

    std::io::stdout().write_all(&buf)?;

    Ok(())
}

type CodespanResult<T> = Result<T, CodespanError>;
type CodespanError = codespan_reporting::files::Error;

impl<'a> codespan_reporting::files::Files<'a> for SystemWorld {
    type FileId = FileId;
    type Name = String;
    type Source = Source;

    fn name(&'a self, id: FileId) -> CodespanResult<Self::Name> {
        let vpath = id.vpath();
        Ok(if let VirtualRoot::Package(package) = id.root() {
            format!("{package}{}", vpath.get_with_slash())
        } else {
            vpath.get_without_slash().into()
        })
    }

    fn source(&'a self, id: FileId) -> CodespanResult<Self::Source> {
        match self.lookup(id) {
            Ok(x) => Ok(x),
            // Hack to be able to report errors on files that are not UTF-8. The
            // error range should always be 0..0 for this to work.
            Err(typst::diag::FileError::InvalidUtf8) => Ok(Source::new(id, String::new())),
            Err(e) => Err(CodespanError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e,
            ))),
        }
    }

    fn line_index(&'a self, id: FileId, given: usize) -> CodespanResult<usize> {
        let source = self.source(id)?;
        source
            .lines()
            .byte_to_line(given)
            .map(|line| line + self.virtual_line(id))
            .ok_or_else(|| CodespanError::IndexTooLarge {
                given,
                max: source.lines().len_bytes(),
            })
    }

    fn line_range(&'a self, id: FileId, given: usize) -> CodespanResult<std::ops::Range<usize>> {
        let source = self.source(id)?;
        let given = given - self.virtual_line(id);
        source
            .lines()
            .line_to_range(given)
            .ok_or_else(|| CodespanError::LineTooLarge {
                given,
                max: source.lines().len_lines(),
            })
    }

    fn column_number(&'a self, id: FileId, _: usize, given: usize) -> CodespanResult<usize> {
        let source = self.source(id)?;
        source.lines().byte_to_column(given).ok_or_else(|| {
            let max = source.lines().len_bytes();
            if given <= max {
                CodespanError::InvalidCharBoundary { given }
            } else {
                CodespanError::IndexTooLarge { given, max }
            }
        })
    }
}

mod json {
    use std::io::Write;

    use serde::Serialize;
    use typst::syntax::FileId;

    use crate::check::diagnostics::{Diagnostic, Severity};
    use crate::cli::CodespanResult;

    #[derive(Serialize)]
    struct JsonDiagnostic<'a> {
        kind: &'a str,
        message: &'a str,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        hints: Vec<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        location: Option<Location>,
        code: &'a str,
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

    pub fn emit<'a>(
        w: &mut impl Write,
        files: &'a mut impl codespan_reporting::files::Files<'a, FileId = FileId>,
        diag: &Diagnostic,
    ) -> CodespanResult<()> {
        let location = if let Some(label) = &diag.label {
            let file_id = label.file;
            let start_line = files.line_index(file_id, label.range.start)?;
            let end_line = files.line_index(file_id, label.range.end)?;

            Some(Location {
                file: files.name(file_id)?.to_string(),
                from: LineCol {
                    line: files.line_number(file_id, start_line)?,
                    column: files.column_number(file_id, start_line, label.range.start)?,
                },
                to: LineCol {
                    line: files.line_number(file_id, end_line)?,
                    column: files.column_number(file_id, end_line, label.range.end)?,
                },
            })
        } else {
            None
        };
        serde_json::to_writer(
            &mut *w,
            &JsonDiagnostic {
                kind: match diag.severity {
                    Severity::Note => "note",
                    Severity::Warning => "warning",
                    Severity::Error => "error",
                },
                location,
                message: &diag.message,
                hints: diag.hints().collect(),
                code: diag.code.as_str(),
            },
        )
        .unwrap();
        writeln!(w).unwrap();

        Ok(())
    }
}
