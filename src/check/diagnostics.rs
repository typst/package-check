use std::fmt::Write as _;
use std::path::Path;
use std::range::Range;

use typst::WorldExt;
use typst::ecow::EcoString;
use typst::syntax::{DiagSpan, FileId, RootedPath, VirtualPath, VirtualRoot};

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

    pub fn emit_many(&mut self, ds: impl Iterator<Item = Diagnostic>) {
        for d in ds {
            self.emit(d)
        }
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
    }

    /// Sort diagnostics by file path and position.
    pub fn sort(&mut self) {
        let diag_pos = |diag: &Diagnostic| {
            let label = diag.label?;
            let file = PrioritzedFile::from(label.file);
            Some((file, label.range.start))
        };
        self.errors.sort_by_key(diag_pos);
        self.warnings.sort_by_key(diag_pos);
    }

    pub fn errors(&self) -> &[Diagnostic] {
        &self.errors
    }

    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }

    pub fn all(&self) -> impl DoubleEndedIterator<Item = &Diagnostic> {
        self.errors
            .iter()
            .chain(self.warnings.iter())
            .chain(self.notes.iter())
    }
}

/// Prioritize diagnostics regarding the manifest and readme.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
enum PrioritzedFile {
    Manifest,
    Readme,
    Other(EcoString),
}

impl From<FileId> for PrioritzedFile {
    fn from(id: FileId) -> Self {
        let vpath = id.vpath();
        let path = vpath.get_without_slash();
        match path {
            _ if path == "typst.toml" => Self::Manifest,
            _ if path == "README.md" => Self::Readme,
            _ => Self::Other(path.into()),
        }
    }
}

#[must_use = "make sure to `emit()` the diagnostic"]
#[derive(Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Code,
    pub label: Option<Label>,
    pub message: String,
    hints: Vec<String>,
    pub docs_link: Option<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: Code) -> Self {
        Self {
            severity,
            code,
            label: None,
            message: String::new(),
            hints: Vec::new(),
            docs_link: None,
        }
    }

    pub fn error(code: Code) -> Self {
        Self::new(Severity::Error, code)
    }

    pub fn warn(code: Code) -> Self {
        Self::new(Severity::Warning, code)
    }

    pub fn note(code: Code) -> Self {
        Self::new(Severity::Warning, code)
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
        self.hints.extend(hints);
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }

    pub fn with_docs_link(mut self, link: &'static str) -> Self {
        self.docs_link = Some(format!("See: {link}"));
        self
    }

    /// Returns an iterator over the hints and the any [`Self::docs_link`].
    pub fn hints(&self) -> impl Iterator<Item = &str> {
        self.hints
            .iter()
            .map(String::as_str)
            .chain(self.docs_link.as_deref())
    }

    pub fn emit(self, diags: &mut Diagnostics) {
        diags.emit(self);
    }

    /// Format this as a fatal error.
    pub fn format_fatal_error(&self) -> String {
        format!(
            "Fatal error [{}]: {}",
            self.code,
            self.format_message_with_hints()
        )
    }

    /// Format this as a fatal error.
    pub fn format_message_with_hints(&self) -> String {
        let mut buf = format!("{}\n", self.message);
        for hint in self.hints() {
            writeln!(buf, "= {hint}").ok();
        }
        buf
    }
}

impl From<&Diagnostic> for codespan_reporting::diagnostic::Diagnostic<FileId> {
    fn from(diag: &Diagnostic) -> Self {
        codespan_reporting::diagnostic::Diagnostic {
            severity: diag.severity.into(),
            code: Some(diag.code.to_string()),
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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
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

macro_rules! codes {
    (
        $(
            [Overridable::$overridable:ident]
            $($name:ident = $str:literal)+
        )+
    ) => {
        #[derive(Debug, Copy, Clone, Eq, PartialEq)]
        pub enum Code {
            $($($name,)+)+
        }

        impl Code {
            pub fn as_str(self) -> &'static str {
                match self {
                    $($(Self::$name => $str,)+)+
                }
            }

            pub fn overridable(self) -> Overridable {
                match self {
                    $($(| Self::$name)+ => Overridable::$overridable,)+
                }
            }

            pub fn variants() -> &'static [Code] {
                &[
                    $($(Self::$name,)+)+
                ]
            }
        }
    };
}

codes! {
    [Overridable::Never]
    Io = "io"

    ManifestIo = "manifest/io"
    ManifestTomlSyntax = "manifest/toml-syntax"

    // === Package ===
    ManifestPackageMissing = "manifest/package/missing"
    ManifestPackageType = "manifest/package/type"
    // Entrypoint
    ManifestPackageEntrypointMissing = "manifest/package/entrypoint/missing"
    ManifestPackageEntrypointType = "manifest/package/entrypoint/type"
    ManifestPackageEntrypointInvalid = "manifest/package/entrypoint/invalid"
    // Name
    ManifestPackageNameMissing = "manifest/package/name/missing"
    ManifestPackageNameType = "manifest/package/name/type"
    ManifestPackageNameKebabCase = "manifest/package/name/kebab-case"
    ManifestPackageNameMismatch = "manifest/package/name/mismatch"
    // Version
    ManifestPackageVersionMissing = "manifest/package/version/missing"
    ManifestPackageVersionType = "manifest/package/version/type"
    ManifestPackageVersionInvalid = "manifest/package/version/invalid"
    ManifestPackageVersionMismatch = "manifest/package/version/mismatch"
    // Compiler
    ManifestPackageCompilerType = "manifest/package/compiler/type"
    ManifestPackageCompilerInvalid = "manifest/package/compiler/invalid"
    // License
    ManifestPackageLicenseMissing = "manifest/package/license/missing"
    ManifestPackageLicenseType = "manifest/package/license/type"
    ManifestPackageLicenseInvalid = "manifest/package/license/invalid"
    ManifestPackageLicenseOsi = "manifest/package/license/osi"
    // Description
    ManifestPackageDescMissing = "manifest/package/description/missing"
    ManifestPackageDescType = "manifest/package/description/type"
    // Authors
    ManifestPackageAuthorsMissing = "manifest/package/authors/missing"
    ManifestPackageAuthorsType = "manifest/package/authors/type"
    // Homepage
    ManifestPackageHomepageType = "manifest/package/homepage/type"
    ManifestPackageHomepageRedundant = "manifest/package/homepage/redundant"
    ManifestPackageHomepageUnreachable = "manifest/package/homepage/unreachable"
    // Repository
    ManifestPackageRepoType = "manifest/package/repository/type"
    ManifestPackageRepoUnreachable = "manifest/package/repository/unreachable"
    // Exclude
    ManifestPackageExcludeType = "manifest/package/exclude/type"
    ManifestPackageExcludeInvalid = "manifest/package/exclude/invalid"
    ManifestPackageExcludeLeadingDot = "manifest/package/exclude/leading-dot"

    // === Template ===
    ManifestTemplateType = "manifest/template/type"
    // Path
    ManifestTemplatePathMissing = "manifest/template/path/missing"
    ManifestTemplatePathType = "manifest/template/path/type"
    ManifestTemplatePathInvalid = "manifest/template/path/invalid"
    // Entrypoint
    ManifestTemplateEntrypointMissing = "manifest/template/entrypoint/missing"
    ManifestTemplateEntrypointType = "manifest/template/entrypoint/type"
    ManifestTemplateEntrypointInvalid = "manifest/template/entrypoint/invalid"
    // Thumbnail
    ManifestTemplateThumbnailMissing = "manifest/template/thumbnail/missing"
    ManifestTemplateThumbnailType = "manifest/template/thumbnail/type"
    ManifestTemplateThumbnailInvalid = "manifest/template/thumbnail/invalid"
    ManifestTemplateThumbnailNotFound = "manifest/template/thumbnail/not-found"
    ManifestTemplateThumbnailFormat = "manifest/template/thumbnail/format"
    ManifestTemplateThumbnailLocation = "manifest/template/thumbnail/location"

    ExcludeLicense = "exclude/license"
    ExcludeReadme = "exclude/readme"
    ExcludeThumbnail = "exclude/thumbnail"
    ExcludeTemplate = "exclude/template"

    FilesIgnored = "files/ignored"
    FilesLicenseName = "files/license/name"
    FilesReadmeName = "files/readme/name"
    FilesFonts = "files/fonts"
    FilesManualUnlinked =  "files/manual/unlinked"
    FilesCompilationArtifact = "files/compilation-artifact"

    ReadmeIo = "readme/io"
    ReadmeSyntax = "readme/syntax"
    ReadmeUnsupportedExtensionAlert = "readme/unsupported-extension/alert"
    ReadmeUnsupportedExtensionTasklist = "readme/unsupported-extension/tasklist"
    ReadmeLinkInvalidUrl = "readme/link/invalid-url"
    ReadmeLinkInvalidPath = "readme/link/invalid-path"
    ReadmeLinkFileNotFound = "readme/link/file-not-found"
    ReadmeImageMissingAlt = "readme/image/missing-alt"

    ImportRelative = "import/relative"
    ImportOutdatedSelf = "import/outdated/self"

    CompileError = "compile/error"
    CompileWarning = "compile/warning"

    AuthorsChanged = "author/changed"

    ListVersionsIo = "list-versions/io"
    ListVersionsUtf8 = "list-versions/utf-8"
    ListVersionsInvalid = "list-versions/invalid"

    GitIo = "git/io"
    GitUtf8 = "git/utf-8"

    GithubApiPullUpdate = "github/api/pull/update"
    GithubApiCheckRunCreate = "github/api/check-run/create"
    GithubApiCheckRunCancel = "github/api/check-run/cancel"
    GithubApiCheckRunFatal = "github/api/check-run/fatal"
    GithubApiCheckRunFinalize = "github/api/check-run/finalize"
    GithubActionsEventIo = "github/actions/event/io"
    GithubActionsEventInvalid = "github/actions/event/invalid"

    [Overridable::Staff]
    SizeExtraLarge = "size/extra-large"


    [Overridable::User]
    KebabCaseValue = "kebab-case/value"
    KebabCaseParameter = "kebab-case/parameter"
    // TODO: Hints shouldn't need be overriden.
    ImportOutdatedDependency = "import/outdated/dependency"

    ManifestPackageNameTypst = "manifest/package/name/typst"
    ManifestPackageDescTypst = "manifest/package/description/typst"

    ReadmeLinkRepoDefaultBranch = "readme/link/repo/default-branch"
    ReadmeImageInadequateAlt = "readme/image/inadequate-alt"

    ExcludeExample = "exclude/example"
    ExcludeTest = "exclude/test"

    SizeLarge = "size/large"
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Overridable {
    /// Lint cannot be overriden.
    Never,
    /// Lint can be overriden by staff members.
    Staff,
    /// Lint can be overriden by users.
    User,
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

/// Having this as a struct, not a module, is more convenient for auto-imports.
pub struct Docs;

#[rustfmt::skip]
impl Docs {
    pub const MANIFEST: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md";
    pub const PACKAGE: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#package-metadata";
    pub const NAMING_RULES: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#naming-rules";
    pub const DESCRIPTION: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#writing-a-good-description";
    pub const TEMPLATE: &str = "https://github.com/typst/packages/blob/main/docs/manifest.md#templates";
    pub const WHAT_TO_EXCLUDE: &str = "https://github.com/typst/packages/blob/main/docs/tips.md#what-to-commit-what-to-exclude";
    pub const LICENSE: &str = "https://github.com/typst/packages/blob/main/docs/licensing.md";
    pub const DOCS: &str = "https://github.com/typst/packages/blob/main/docs/documentation.md";
    pub const DOCS_GFM: &str = "https://github.com/typst/packages/blob/main/docs/documentation.md#differences-from-standard-markdown";
    pub const FONTS_NOT_ALLOWED: &str = "https://github.com/typst/packages/blob/main/docs/resources.md#fonts-are-not-supported-in-packages";
}

impl Docs {
    pub fn for_manifest_key(parent: Option<&str>) -> &'static str {
        match parent {
            Some("package") => Docs::PACKAGE,
            Some(_) => Docs::TEMPLATE,
            None => Docs::MANIFEST,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::check::diagnostics::Code;

    #[test]
    fn unique_code_string() {
        let mut set = HashSet::new();
        for code in Code::variants() {
            assert!(set.insert(code.as_str()), "{code}");
        }
    }
}
