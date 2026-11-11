use std::fmt::Write as _;
use std::path::Path;
use std::range::Range;

use typst::WorldExt;
use typst::syntax::{DiagSpan, FileId, RootedPath, VirtualPath, VirtualRoot};

use crate::check::codes::Code;
use crate::check::toml::TomlPath;
use crate::world::SystemWorld;

pub type Result<T> = std::result::Result<T, Diagnostic>;

pub trait TryExt<T> {
    fn error(self, code: Code, message: impl Into<String>) -> Result<T>;
}

impl<T, E> TryExt<T> for std::result::Result<T, E> {
    fn error(self, code: Code, message: impl Into<String>) -> Result<T> {
        match self {
            std::result::Result::Ok(t) => Ok(t),
            std::result::Result::Err(_) => Err(Diagnostic::error(code).with_message(message)),
        }
    }
}

impl<T> TryExt<T> for Option<T> {
    fn error(self, code: Code, message: impl Into<String>) -> Result<T> {
        match self {
            Option::Some(t) => Ok(t),
            Option::None => Err(Diagnostic::error(code).with_message(message)),
        }
    }
}

pub type EmittableResult<'d, T> = std::result::Result<T, EmittableDiagnostic<'d>>;

pub trait MaybeEmit<T> {
    fn maybe_emit(self) -> Option<T>;
}

impl<T> MaybeEmit<T> for EmittableResult<'_, T> {
    fn maybe_emit(self) -> Option<T> {
        match self {
            Ok(val) => Some(val),
            Err(e) => {
                e.diags.emit(e.diag);
                None
            }
        }
    }
}

pub struct EmittableDiagnostic<'d> {
    diags: &'d mut Diagnostics,
    diag: Diagnostic,
}

impl<'d> EmittableDiagnostic<'d> {
    pub fn new(diags: &'d mut Diagnostics, diag: Diagnostic) -> Self {
        Self { diags, diag }
    }
}

impl From<EmittableDiagnostic<'_>> for Diagnostic {
    fn from(value: EmittableDiagnostic<'_>) -> Self {
        value.diag
    }
}

#[derive(Default, Debug)]
pub struct Diagnostics {
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    notes: Vec<Diagnostic>,
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

    pub fn emit(&mut self, d: Diagnostic) {
        tracing::debug!("Emitting: {:?}", &d);
        match d.severity {
            Severity::Error => self.errors.push(d),
            Severity::Warning => self.warnings.push(d),
            Severity::Note => self.notes.push(d),
        }
    }

    pub fn emit_many(&mut self, ds: impl IntoIterator<Item = Diagnostic>) {
        for d in ds {
            self.emit(d)
        }
    }

    pub fn split_off<F>(&mut self, mut f: F) -> Vec<Diagnostic>
    where
        F: FnMut(&Diagnostic) -> bool,
    {
        let mut split_off = Vec::new();

        let mut split = |diags: &mut Vec<Diagnostic>| {
            *diags = std::mem::take(diags)
                .into_iter()
                .filter_map(|diag| match f(&diag) {
                    false => Some(diag),
                    true => {
                        split_off.push(diag);
                        None
                    }
                })
                .collect();
        };

        split(&mut self.errors);
        split(&mut self.warnings);
        split(&mut self.notes);

        split_off
    }

    pub fn extend(&mut self, mut other: Self, dir_prefix: &Path) {
        let fix_labels = |diag: &mut Diagnostic| {
            if let Some(label) = &mut diag.label
                && let Some(path) = dir_prefix
                    .to_str()
                    .and_then(|dir_prefix| VirtualPath::new(dir_prefix).ok())
                    .and_then(|dir| dir.join(label.file.vpath().get_without_slash()).ok())
                && *label.file.root() == VirtualRoot::Project
            {
                label.file = FileId::new(RootedPath::new(VirtualRoot::Project, path))
            }
        };

        other.errors.iter_mut().for_each(fix_labels);
        self.errors.extend(other.errors);

        other.warnings.iter_mut().for_each(fix_labels);
        self.warnings.extend(other.warnings);

        other.notes.iter_mut().for_each(fix_labels);
        self.notes.extend(other.notes);
    }

    /// Sort diagnostics.
    pub fn sort(&mut self) {
        self.errors.sort();
        self.warnings.sort();
        self.notes.sort();
    }

    pub fn errors(&self) -> &[Diagnostic] {
        &self.errors
    }

    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }

    pub fn notes(&self) -> &[Diagnostic] {
        &self.notes
    }

    pub fn all(&self) -> impl DoubleEndedIterator<Item = &Diagnostic> {
        self.errors
            .iter()
            .chain(self.warnings.iter())
            .chain(self.notes.iter())
    }
}

#[must_use = "make sure to `emit()` the diagnostic"]
#[derive(Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Code,
    pub label: Option<Label>,
    pub message: String,
    /// The hints and doc links.
    /// Any doc links are stored at the end of the list.
    hints: Vec<String>,
    /// The index of the first hint that is a docs link.
    docs_links_start: usize,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: Code) -> Self {
        Self {
            severity,
            code,
            label: None,
            message: String::new(),
            hints: Vec::new(),
            docs_links_start: 0,
        }
    }

    pub fn error(code: Code) -> Self {
        Self::new(Severity::Error, code)
    }

    pub fn warn(code: Code) -> Self {
        Self::new(Severity::Warning, code)
    }

    pub fn note(code: Code) -> Self {
        Self::new(Severity::Note, code)
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn with_label(mut self, label: impl Into<Option<Label>>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_hints(mut self, hints: impl IntoIterator<Item = String>) -> Self {
        for hint in hints {
            self.hints.insert(self.docs_links_start, hint);
            self.docs_links_start += 1;
        }
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.insert(self.docs_links_start, hint.into());
        self.docs_links_start += 1;
        self
    }

    pub fn with_docs_link(mut self, link: &'static str) -> Self {
        self.hints.push(format!("See: {link}"));
        self
    }

    /// Whether the hints contain any doc links.
    pub fn has_docs_links(&self) -> bool {
        self.docs_links_start < self.hints.len()
    }

    /// Returns an iterator over the hints and any [`Self::docs_links`].
    pub fn hints(&self) -> impl Iterator<Item = &str> {
        self.hints.iter().map(String::as_str)
    }

    pub fn emit(self, diags: &mut Diagnostics) {
        diags.emit(self);
    }

    /// Format this as a fatal error.
    pub fn format_fatal_error(&self) -> String {
        format!(
            "Fatal error [{}]: {}",
            self.code.ty(),
            self.format_message_with_hints()
        )
    }

    /// Combine the message and hints.
    pub fn format_message_with_hints(&self) -> String {
        let mut buf = format!("{}\n", self.message);
        for hint in self.hints() {
            writeln!(buf, "= {hint}").ok();
        }
        buf
    }
}

impl std::cmp::Ord for Diagnostic {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.severity
            .cmp(&other.severity)
            .then_with(|| self.label.cmp(&other.label))
            .then_with(|| self.code.ty().cmp(&other.code.ty()))
            .then_with(|| self.message.cmp(&other.message))
    }
}

impl std::cmp::PartialOrd for Diagnostic {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl From<&Diagnostic> for codespan_reporting::diagnostic::Diagnostic<FileId> {
    fn from(diag: &Diagnostic) -> Self {
        codespan_reporting::diagnostic::Diagnostic {
            severity: diag.severity.into(),
            code: Some(diag.code.ty().to_string()),
            message: diag.message.clone(),
            labels: diag
                .label
                .map(|l| codespan_reporting::diagnostic::Label::primary(l.file, l.range))
                .into_iter()
                .collect(),
            notes: diag.hints().map(String::from).collect(),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        })
    }
}

impl From<typst::diag::Severity> for Severity {
    fn from(value: typst::diag::Severity) -> Self {
        match value {
            typst::diag::Severity::Error => Self::Error,
            typst::diag::Severity::Warning => Self::Warning,
        }
    }
}

impl From<Severity> for codespan_reporting::diagnostic::Severity {
    fn from(value: Severity) -> Self {
        match value {
            Severity::Error => codespan_reporting::diagnostic::Severity::Error,
            Severity::Warning => codespan_reporting::diagnostic::Severity::Warning,
            Severity::Note => codespan_reporting::diagnostic::Severity::Note,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct Label {
    pub file: FileId,
    pub range: Range<usize>,
}

impl Label {
    pub fn new(file: FileId, range: impl Into<Range<usize>>) -> Self {
        Self {
            file,
            range: range.into(),
        }
    }

    /// Create a label for a span.
    pub fn for_span(world: &SystemWorld, span: impl Into<DiagSpan>) -> Option<Self> {
        let span = span.into();
        Some(Label::new(span.id()?, world.range(span)?))
    }
}

impl std::cmp::Ord for Label {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        PrioritizedFile::from(&self.file)
            .cmp(&PrioritizedFile::from(&other.file))
            .then_with(|| self.range.start.cmp(&other.range.start))
    }
}

impl std::cmp::PartialOrd for Label {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Prioritize diagnostics regarding the manifest and readme.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
enum PrioritizedFile<'a> {
    Manifest,
    Readme,
    Other(&'a str),
}

impl<'a> From<&'a FileId> for PrioritizedFile<'a> {
    fn from(id: &'a FileId) -> Self {
        match id.vpath().get_without_slash() {
            "typst.toml" => Self::Manifest,
            "README.md" => Self::Readme,
            path => Self::Other(path),
        }
    }
}

/// Having this as a struct, not a module, is more convenient for auto-imports.
pub struct Docs;

#[rustfmt::skip]
impl Docs {
    pub const MANIFEST: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md";
    pub const PACKAGE: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#package-metadata";
    pub const NAMING_RULES: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#naming-rules";
    pub const DESCRIPTION: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#writing-a-good-description";
    pub const TEMPLATE: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#templates";
    pub const THIRD_PARTY_META:  &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#third-party-metadata";
    pub const WHAT_TO_EXCLUDE: &str = "https://github.com/typst/packages/blob/main/docs/tips.md#what-to-commit-what-to-exclude";
    pub const LICENSE: &str = "https://github.com/typst/packages/blob/main/docs/licensing.md";
    pub const DOCS: &str = "https://github.com/typst/packages/blob/main/docs/documentation.md";
    pub const DOCS_GFM: &str = "https://github.com/typst/packages/blob/main/docs/documentation.md#differences-from-standard-markdown";
    pub const FONTS_NOT_ALLOWED: &str = "https://github.com/typst/packages/blob/main/docs/resources.md#fonts-are-not-supported-in-packages";
    // TODO: Update docs.
    pub const SIZE: &str = "TODO";
}

impl Docs {
    pub fn for_manifest_path(path: &TomlPath) -> &'static str {
        match path.root() {
            "package" => Docs::PACKAGE,
            "template" => Docs::TEMPLATE,
            // TODO: Change to package-check lint docs.
            "tool" => Docs::THIRD_PARTY_META,
            _ => Docs::MANIFEST,
        }
    }
}
