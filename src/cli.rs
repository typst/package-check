use std::path::Path;

use codespan_reporting::term;
use termcolor::{Color, ColorSpec, WriteColor};
use tracing::error;
use typst::syntax::VirtualRoot;
use typst::syntax::{FileId, Source};

use crate::check::Diagnostics;
use crate::check::diagnostics::Diagnostic;
use crate::world::SystemWorld;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Show {
    Denied,
    Allowed,
    All,
}

impl Show {
    fn allowed(self) -> bool {
        match self {
            Show::Allowed | Show::All => true,
            Show::Denied => false,
        }
    }

    fn denied(self) -> bool {
        match self {
            Show::Denied | Show::All => true,
            Show::Allowed => false,
        }
    }
}

/// Print a check header for a package to the writer.
pub fn write_header(mut w: impl WriteColor, package_dir: &Path, show: Show) -> std::io::Result<()> {
    w.set_color(
        ColorSpec::new()
            .set_bold(true)
            .set_intense(true)
            .set_fg(Some(Color::Green)),
    )?;
    w.write_all(b"Checking ")?;
    w.reset()?;
    writeln!(w, "`{}`", package_dir.display())?;

    if show == Show::Allowed {
        w.set_color(
            ColorSpec::new()
                .set_intense(true)
                .set_fg(Some(Color::Black)),
        )?;
        writeln!(w, "  (only showing allowed diagnostics)")?;
        w.reset()?;
    }

    Ok(())
}

/// Print output to the writer.
pub fn write_output(
    mut w: impl WriteColor,
    world: &SystemWorld,
    diags: &Diagnostics,
    allowed: &[Diagnostic],
    show: Show,
) -> std::io::Result<()> {
    if show.allowed() {
        for diag in allowed.iter().rev() {
            write_diagnostic(&mut w, world, diag, true)?;
        }
    }

    if show.denied() {
        for diag in diags.all().rev() {
            write_diagnostic(&mut w, world, diag, false)?;
        }
    }

    Ok(())
}

/// Print output to the writer.
pub fn write_diagnostic(
    mut w: impl WriteColor,
    world: &SystemWorld,
    diag: &Diagnostic,
    allowed: bool,
) -> std::io::Result<()> {
    if allowed {
        w.set_color(ColorSpec::new().set_bold(true).set_fg(Some(Color::Cyan)))?;
        w.write_all(b"[allowed] ")?;
        w.reset()?;
    }

    let config = term::Config {
        tab_width: 2,
        ..Default::default()
    };

    let mut diag = diag.into();
    if let Err(err) = term::emit_to_write_style(&mut w, &config, world, &diag) {
        if let CodespanError::Io(io) = err {
            return Err(io);
        }

        // Formatting diagnostics should only fail, if there was an
        // issue reading the file or the span of inside the file.
        error!("Failed to resolve label ({err}):\n{diag:#?}");
        diag.labels.clear();
        if let Err(CodespanError::Io(err)) =
            term::emit_to_write_style(&mut w, &config, world, &diag)
        {
            return Err(err);
        }
    }

    Ok(())
}

/// Print this diagnostic as a fatal error to the writer.
pub fn write_fatal_error(mut w: impl WriteColor, diag: &Diagnostic) -> std::io::Result<()> {
    w.set_color(ColorSpec::new().set_bold(true).set_fg(Some(Color::Red)))?;
    write!(w, "fatal error [{}]", diag.code.ty())?;
    w.reset()?;
    writeln!(w, ": {}", diag.format_message_with_hints())
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
