use std::path::Path;

use codespan_reporting::diagnostic::{Diagnostic, Severity};
use typst::syntax::{FileId, VirtualPath};

#[derive(Default, Debug)]
pub struct Diagnostics {
    warnings: Vec<Diagnostic<FileId>>,
    errors: Vec<Diagnostic<FileId>>,
}

impl Diagnostics {
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

    pub fn errors(&self) -> &[Diagnostic<FileId>] {
        &self.errors
    }

    pub fn warnings(&self) -> &[Diagnostic<FileId>] {
        &self.warnings
    }
}
