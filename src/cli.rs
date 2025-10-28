use std::path::PathBuf;
use std::process::exit;

use codespan_reporting::{diagnostic::Diagnostic, term};
use ignore::overrides::Override;
use tracing::error;
use typst::syntax::{package::PackageSpec, FileId, Source};

use crate::{check::all_checks, package::PackageExt, world::SystemWorld};

pub async fn main(spec_or_path: String, json_output: bool) {
    let package_spec: Option<PackageSpec> = spec_or_path.parse().ok();
    let package_dir = if let Some(ref package_spec) = package_spec {
        package_spec.directory()
    } else {
        PathBuf::from(spec_or_path)
    };

    match all_checks(package_spec.as_ref(), package_dir, true).await {
        Ok((mut world, diags)) => {
            if let Err(err) =
                print_diagnostics(&mut world, diags.errors(), diags.warnings(), json_output)
            {
                error!("failed to print diagnostics ({err})");
                error!(
                    "Raw diagnostics: {:#?}\n{:#?}",
                    diags.errors(),
                    diags.warnings()
                );
            }

            if !diags.errors().is_empty() {
                exit(1)
            }

            if !diags.warnings().is_empty() {
                exit(2)
            }
        }
        Err(e) => {
            println!("Fatal error: {}", e.message);
            exit(1)
        }
    }
}

/// Print diagnostic messages to the terminal.
pub fn print_diagnostics(
    world: &mut SystemWorld,
    errors: &[Diagnostic<FileId>],
    warnings: &[Diagnostic<FileId>],
    json: bool,
) -> Result<(), codespan_reporting::files::Error> {
    let config = term::Config {
        tab_width: 2,
        ..Default::default()
    };

    // We should be able to print diagnostics even on excluded files. If we
    // don't remove the exclusion, it will fail to read and display the file
    // contents.
    world.exclude(Override::empty());

    for diagnostic in warnings.iter().chain(errors) {
        if json {
            json::emit(&mut std::io::stdout(), world, diagnostic)?;
        } else {
            term::emit_to_write_style(
                &mut term::termcolor::StandardStream::stdout(term::termcolor::ColorChoice::Always),
                &config,
                world,
                diagnostic,
            )?;
        }
    }

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
        Ok(if let Some(package) = id.package() {
            format!("{package}{}", vpath.as_rooted_path().display())
        } else {
            // Try to express the path relative to the working directory.
            vpath
                .resolve(self.root())
                .and_then(|abs| pathdiff::diff_paths(abs, self.workdir()))
                .as_deref()
                .unwrap_or_else(|| vpath.as_rootless_path())
                .to_string_lossy()
                .into()
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

    use codespan_reporting::diagnostic::{Diagnostic, Severity};
    use serde::Serialize;

    use crate::cli::CodespanResult;

    #[derive(Serialize)]
    struct JsonDiagnostic<'a> {
        kind: &'a str,
        message: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<&'a str>,
    }

    pub fn emit<'a, F: Copy>(
        w: &mut impl Write,
        files: &'a mut impl codespan_reporting::files::Files<'a, FileId = F>,
        diag: &Diagnostic<F>,
    ) -> CodespanResult<()> {
        let file = if diag.labels.is_empty() {
            None
        } else {
            Some(files.name(diag.labels[0].file_id)?.to_string())
        };
        let code = diag.code.as_deref();
        serde_json::to_writer(
            &mut *w,
            &JsonDiagnostic {
                kind: if diag.severity == Severity::Error {
                    "error"
                } else {
                    "warning"
                },
                file: file.as_deref(),
                message: &diag.message,
                code,
            },
        )
        .unwrap();
        writeln!(w).unwrap();

        Ok(())
    }
}
