use std::{fmt::Display, path::Path};

use codespan_reporting::diagnostic::{Diagnostic, Severity};
use typst::syntax::{FileId, VirtualPath};

pub type Result<T> = std::result::Result<T, Diagnostic<FileId>>;

pub trait TryExt<T> {
    fn error(self, code: &'static str, message: impl Display) -> Result<T>;
}

impl<T, E> TryExt<T> for std::result::Result<T, E> {
    fn error(self, code: &'static str, message: impl Display) -> Result<T> {
        match self {
            std::result::Result::Ok(t) => Ok(t),
            std::result::Result::Err(_) => {
                Err(Diagnostic::error().with_message(message).with_code(code))
            }
        }
    }
}

impl<T> TryExt<T> for Option<T> {
    fn error(self, code: &'static str, message: impl Display) -> Result<T> {
        match self {
            Option::Some(t) => Ok(t),
            Option::None => Err(Diagnostic::error().with_message(message).with_code(code)),
        }
    }
}

#[derive(Default, Debug)]
pub struct Diagnostics {
    warnings: Vec<Diagnostic<FileId>>,
    errors: Vec<Diagnostic<FileId>>,
}

impl Diagnostics {
    pub fn maybe_emit<T>(&mut self, maybe_err: Result<T>) -> Option<T> {
        match maybe_err {
            Ok(v) => Some(v),
            Err(e) => {
                self.emit(e);
                None
            }
        }
    }

    pub fn emit(&mut self, d: Diagnostic<FileId>) {
        tracing::debug!("Emitting: {:?}", &d);
        if d.severity == Severity::Warning {
            self.warnings.push(d)
        } else {
            self.errors.push(d)
        }
    }

    pub fn emit_many(&mut self, ds: impl Iterator<Item = Diagnostic<FileId>>) {
        for d in ds {
            self.emit(d)
        }
    }

    pub fn extend(&mut self, mut other: Self, dir_prefix: &Path) {
        let fix_labels = |diag: &mut Diagnostic<FileId>| {
            for label in diag.labels.iter_mut() {
                if label.file_id.package().is_none() {
                    label.file_id = FileId::new(
                        None,
                        VirtualPath::new(dir_prefix.join(label.file_id.vpath().as_rootless_path())),
                    )
                }
            }
        };

        other.errors.iter_mut().for_each(fix_labels);
        self.errors.extend(other.errors);

        other.warnings.iter_mut().for_each(fix_labels);
        self.warnings.extend(other.warnings);
    }

    /// Sort diagnostics by file path and position.
    pub fn sort(&mut self) {
        let diag_pos = |diag: &Diagnostic<FileId>| {
            let label = diag.labels.first()?;
            let file = PrioritzedFile::from(label.file_id);
            Some((file, label.range.start))
        };
        self.errors.sort_by_key(diag_pos);
        self.warnings.sort_by_key(diag_pos);
    }

    pub fn errors(&self) -> &[Diagnostic<FileId>] {
        &self.errors
    }

    pub fn warnings(&self) -> &[Diagnostic<FileId>] {
        &self.warnings
    }
}

/// Prioritize diagnostics regarding the manifest and readme.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
enum PrioritzedFile {
    Manifest,
    Readme,
    Other(&'static VirtualPath),
}

impl From<FileId> for PrioritzedFile {
    fn from(id: FileId) -> Self {
        let vpath = id.vpath();
        let path = vpath.as_rootless_path();
        match path {
            _ if path == "typst.toml" => Self::Manifest,
            _ if path == "README.md" => Self::Readme,
            _ => Self::Other(vpath),
        }
    }
}
