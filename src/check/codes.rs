use std::collections::{BTreeMap, HashSet};

use typst::ecow::EcoString;
use typst::syntax::VirtualPath;

use crate::check::diagnostics::Diagnostic;
use crate::check::manifest::{Lint, Selector, manifest_label};
use crate::check::toml::Spanned;
use crate::check::{Checks, Diagnostics};

pub struct Output {
    pub denied: Diagnostics,
    pub allowed: Diagnostics,
    /// There are diagnostics that are overridable, which *weren't* allowed.
    pub has_overridable: bool,
    /// There are staff-overridable diagnostics, which *were* allowed.
    pub has_staff_override: bool,
}

impl Output {
    /// Whether there are no denied no warnings and errors.
    pub fn passed(&self) -> bool {
        self.denied.errors().is_empty() && self.denied.warnings().is_empty()
    }
}

/// Filter out diagnostics that have been allowed in the
/// `tool.package-check.lint` configuration.
pub fn process_diagnostics(checks: &Checks, mut diags: Diagnostics) -> Output {
    let package_check = checks.manifest.package_check.as_ref();

    let mut allowed = Diagnostics::default();
    let mut has_overridable = false;
    let mut has_allowed_staff_overridable = false;
    if let Some(lints) = package_check.and_then(|p| p.lints.as_ref()) {
        // Stores which selectors have been matched.
        let mut matched = HashSet::new();

        allowed = diags.split_off(|diag| {
            let Some(data) = diag.code.data() else {
                return false;
            };

            let mut allowed = None;
            if let Some(lint) = lints.get(&diag.code.ty()) {
                allowed = match_selector(&lint.allowed.val, &data);
            }
            if let Some(idx) = allowed {
                matched.insert((diag.code.ty(), idx));

                if diag.code.ty().overridable().by_staff() {
                    has_allowed_staff_overridable = true;
                }
            } else {
                has_overridable = true;
            }
            allowed.is_some()
        });

        check_unused_lint_selectors(&mut diags, lints, matched);
        diags.sort();
    }

    Output {
        denied: diags,
        allowed,
        has_overridable,
        has_staff_override: has_allowed_staff_overridable,
    }
}

fn match_selector(selector: &Selector, data: &SelectorData) -> Option<usize> {
    match (selector, data) {
        (Selector::Project, SelectorData::Project) => Some(0),
        (Selector::Files(globs), SelectorData::Files(path)) => {
            let path = path.get_without_slash();
            for (i, glob) in globs.iter().enumerate() {
                if glob.matches_relative_file(path) {
                    return Some(i);
                }
            }
            None
        }
        (Selector::Text(regexes), SelectorData::Text(text)) => {
            for (i, regex) in regexes.iter().enumerate() {
                if regex.is_match(text.as_str()) {
                    return Some(i);
                }
            }
            None
        }
        (data, selector) => {
            unreachable!("selector data and type don't match {data:?} {selector:?}");
        }
    }
}

fn check_unused_lint_selectors(
    diags: &mut Diagnostics,
    lints: &BTreeMap<CodeType, Spanned<Lint>>,
    matched: HashSet<(CodeType, usize)>,
) {
    for (code, lint) in lints.iter() {
        match &lint.allowed.val {
            Selector::Project => {
                if !matched.contains(&(*code, 0)) {
                    Diagnostic::warn(Code::UnusedLintOverride)
                        .with_message("This lint does not need to be allowed")
                        .with_label(manifest_label(lint.span()))
                        .emit(diags);
                }
            }
            Selector::Files(globs) => {
                let warnings = (globs.iter().enumerate())
                    .filter(|(i, _)| !matched.contains(&(*code, *i)))
                    .map(|(_, glob)| {
                        Diagnostic::warn(Code::UnusedLintOverrideGlob)
                            .with_message("This file glob doesn't match any diagnostics")
                            .with_label(manifest_label(glob.span()))
                    })
                    .collect::<Vec<_>>();
                if warnings.len() == globs.len() {
                    Diagnostic::warn(Code::UnusedLintOverride)
                        .with_label(manifest_label(lint.allowed.span()))
                        .with_message("This file selector doesn't match any diagnostics")
                        .emit(diags);
                } else {
                    diags.emit_many(warnings);
                }
            }
            Selector::Text(regexes) => {
                let warnings = (regexes.iter().enumerate())
                    .filter(|(i, _)| !matched.contains(&(*code, *i)))
                    .map(|(_, regex)| {
                        Diagnostic::warn(Code::UnusedLintOverrideRegex)
                            .with_message("This regex doesn't match any diagnostics")
                            .with_label(manifest_label(regex.span()))
                    })
                    .collect::<Vec<_>>();
                if warnings.len() == regexes.len() {
                    Diagnostic::warn(Code::UnusedLintOverride)
                        .with_message("This text selector doesn't match any diagnostics")
                        .with_label(manifest_label(lint.allowed.span()))
                        .emit(diags);
                } else {
                    diags.emit_many(warnings);
                }
            }
        }
    }
}

macro_rules! code_data {
    (Project) => {
        ()
    };
    (File) => {
        typst::syntax::VirtualPath
    };
    (Text) => {
        typst::ecow::EcoString
    };
}

macro_rules! selector_data {
    (Project($data:expr)) => {
        SelectorData::Project
    };
    (File($data:expr)) => {
        SelectorData::Files($data)
    };
    (Text($data:expr)) => {
        SelectorData::Text($data)
    };
}

macro_rules! codes {
    (
        [Overridable::Never]
        $($never_name:ident = $never_str:literal)+

        $(
            [Overridable::$overridable:ident ($selector_type:ident)]
            $($name:ident = $str:literal)+
        )+
    ) => {
        #[derive(Debug, Clone, Eq, PartialEq)]
        pub enum Code {
            $($never_name,)+
            $($($name(code_data!($selector_type)),)+)+
        }

        impl Code {
            pub fn ty(&self) -> CodeType {
                match self {
                    $(Self::$never_name => CodeType::$never_name,)+
                    $($(Self::$name(..) => CodeType::$name,)+)+
                }
            }

            /// Selector data for overridable lint codes.
            pub fn data(&self) -> Option<SelectorData> {
                match self {
                    $(| Self::$never_name)+ => None,
                    $($(| Self::$name(_data))+ => Some(selector_data!($selector_type(_data.clone()))),)+
                }
            }
        }

        #[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub enum CodeType {
            $($never_name,)+
            $($($name,)+)+
        }

        impl CodeType {
            pub fn variants() -> &'static [CodeType] {
                &[
                    $(Self::$never_name,)+
                    $($(Self::$name,)+)+
                ]
            }

            pub fn overridable(&self) -> Overridable {
                match self {
                    $(| Self::$never_name)+ => Overridable::Never,
                    $($(| Self::$name)+ => Overridable::$overridable (SelectorType::$selector_type),)+
                }
            }


            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$never_name => $never_str,)+
                    $($(Self::$name => $str,)+)+
                }
            }
        }

        impl std::str::FromStr for CodeType {
            type Err = ();

            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                match value {
                    $($never_str => Ok(Self::$never_name),)+
                    $($($str => Ok(Self::$name),)+)+
                    _ => Err(()),
                }
            }
        }
    };
}

codes! {
    [Overridable::Never]
    Io = "io"

    SpecResolve = "spec/resolve"

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
    ManifestPackageLicenseDisallowed = "manifest/package/license/disallowed"
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

    // === Tool ===
    ManifestToolType = "manifest/tool/type"
    ManifestToolPackageCheckType = "manifest/tool/package-check/type"
    ManifestToolPackageCheckTargetsType = "manifest/tool/package-check/targets/type"
    ManifestToolPackageCheckTargetsEmpty = "manifest/tool/package-check/targets/empty"
    ManifestToolPackageCheckTargetsUnknown = "manifest/tool/package-check/targets/unknown"
    ManifestToolPackageCheckTargetsDuplicate = "manifest/tool/package-check/targets/duplicate"
    ManifestToolPackageCheckLintsUnknown = "manifest/tool/package-check/lints/unknown"
    ManifestToolPackageCheckLintsType = "manifest/tool/package-check/lints/type"

    LintOverridable = "lint/overridable"
    LintSelectorType = "lint/selector/type"
    LintSelectorUnexpected = "lint/selector/unexpected"
    LintSelectorProjectType = "lint/selector/project/type"
    LintSelectorProjectInvalid = "lint/selector/project/invalid"
    LintSelectorFileType = "lint/selector/file/type"
    LintSelectorFileInvalid = "lint/selector/file/invalid"
    LintSelectorFileUnexpected = "lint/selector/file/unexpected"
    LintSelectorFileGlob = "lint/selector/file/glob"
    LintSelectorTextType = "lint/selector/text/type"
    LintSelectorTextInvalid = "lint/selector/text/invalid"
    LintSelectorTextUnexpected = "lint/selector/text/unexpected"
    LintSelectorTextRegex = "lint/selector/text/regex"

    UnusedLintOverride = "unused/lint/override"
    UnusedLintOverrideRegex = "unused/lint/override/regex"
    UnusedLintOverrideGlob = "unused/lint/override/glob"

    ExcludeLicense = "exclude/license"
    ExcludeReadme = "exclude/readme"
    ExcludeThumbnail = "exclude/thumbnail"
    ExcludeTemplate = "exclude/template"

    FilesIgnored = "files/ignored"
    FilesLicenseName = "files/license/name"
    FilesReadmeName = "files/readme/name"
    FilesFonts = "files/fonts"

    ReadmeIo = "readme/io"
    // TODO: https://github.com/typst/package-check/issues/56
    ReadmeSyntax = "readme/syntax"
    // TODO: Add support
    ReadmeUnsupportedExtensionAlert = "readme/unsupported-extension/alert"
    ReadmeUnsupportedExtensionTasklist = "readme/unsupported-extension/tasklist"
    ReadmeLinkInvalidUrl = "readme/link/invalid-url"
    ReadmeLinkInvalidPath = "readme/link/invalid-path"
    ReadmeLinkFileNotFound = "readme/link/file-not-found"
    ReadmeImageMissingAlt = "readme/image/missing-alt"

    ImportRelative = "import/relative"
    ImportOutdatedSelf = "import/outdated/self"

    CompileError = "compile/error"

    GitIo = "git/io"
    GitUtf8 = "git/utf-8"

    GithubApiPullUpdate = "github/api/pull/update"
    GithubApiPullCommentsList = "github/api/pull/comment/list"
    GithubApiPullCommentsPost = "github/api/pull/comment/post"
    GithubApiPullCommentsDelete = "github/api/pull/comment/delete"
    GithubApiPullCommentsTag = "github/api/pull/comment/tags"
    GithubApiCommitPulls = "github/api/commit/pulls"
    GithubApiCheckRunCreate = "github/api/check-run/create"
    GithubActionsEventIo = "github/actions/event/io"
    GithubActionsEventInvalid = "github/actions/event/invalid"


    [Overridable::Staff(Project)]
    SizeBundle = "size/bundle"
    SizeArchive = "size/archive"


    [Overridable::User(Project)]
    ManifestPackageNameTypst = "manifest/package/name/typst"
    ManifestPackageDescTypst = "manifest/package/description/typst"
    ManifestPackageCompilerPatch = "manifest/package/compiler/patch"
    ManifestPackageLicenseCcOnly = "manifest/package/license/cc/only"

    [Overridable::User(File)]
    ExcludeExample = "exclude/example"
    ExcludeTest = "exclude/test"
    FilesManualUnlinked =  "files/manual/unlinked"
    FilesCompilationArtifact = "files/compilation-artifact"
    SizeLarge = "size/large"

    [Overridable::User(Text)]
    SrcKebabCase = "src/kebab-case"
    ReadmeLinkRepoDefaultBranch = "readme/link/repo/default-branch"
    ReadmeImageInadequateAlt = "readme/image/inadequate-alt"
    ImportOutdatedDependency = "import/outdated/dependency"
    CompileWarning = "compile/warning"
}

impl std::fmt::Display for CodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Overridable {
    /// Lint cannot be overriden.
    Never,
    /// Lint can be overriden by staff members.
    // TODO: Implement staff overrides in github action.
    Staff(SelectorType),
    /// Lint can be overriden by users.
    User(SelectorType),
}

impl Overridable {
    pub fn selector_type(self) -> Option<SelectorType> {
        match self {
            Overridable::Never => None,
            Overridable::Staff(ty) => Some(ty),
            Overridable::User(ty) => Some(ty),
        }
    }

    pub fn by_staff(self) -> bool {
        matches!(self, Self::Staff(_))
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SelectorType {
    Project,
    File,
    Text,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SelectorData {
    Project,
    /// Contains the relevant file path for this diagnostic.
    Files(VirtualPath),
    /// Contains the relevant text for this diagnostic.
    Text(EcoString),
}
