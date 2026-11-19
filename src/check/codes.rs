use std::collections::{BTreeMap, HashSet};

use typst::ecow::EcoString;
use typst::syntax::VirtualPath;

use crate::check::diagnostics::Diagnostic;
use crate::check::manifest::{Lint, Selector, manifest_label};
use crate::check::toml::Spanned;
use crate::check::{Checks, Diagnostics};

/// Filter out diagnostics that have been allowed in the
/// `tool.package-check.lint` configuration.
pub fn filter_diagnostics(checks: &Checks, diags: &mut Diagnostics) -> Vec<Diagnostic> {
    let package_check = checks.manifest.package_check.as_ref();
    let Some(lints) = package_check.and_then(|p| p.lints.as_ref()) else {
        return Vec::new();
    };

    // Stores which selectors have been matched.
    let mut matched = HashSet::new();

    let allowed = diags.split_off(|diag| {
        let Some(lint) = lints.get(&diag.code.ty()) else {
            return false;
        };
        let Some(data) = diag.code.data() else {
            return false;
        };

        match (data, &lint.allowed.val) {
            (SelectorData::Project, Selector::Project) => {
                matched.insert((diag.code.ty(), 0));
                true
            }
            (SelectorData::Files(path), Selector::Files(globs)) => {
                let path = path.get_without_slash();
                for (i, glob) in globs.iter().enumerate() {
                    if glob.matches_relative_file(path) {
                        matched.insert((diag.code.ty(), i));
                        return true;
                    }
                }
                false
            }
            (SelectorData::Text(text), Selector::Text(regexes)) => {
                for (i, regex) in regexes.iter().enumerate() {
                    if regex.is_match(text.as_str()) {
                        matched.insert((diag.code.ty(), i));
                        return true;
                    }
                }
                false
            }
            (data, selector) => {
                unreachable!("selector data and type don't match {data:?} {selector:?}");
            }
        }
    });

    check_unused_lint_overrides(diags, lints, matched);
    diags.sort();

    allowed
}

fn check_unused_lint_overrides(
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
    (Files) => {
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
    (Files($data:expr)) => {
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
    ManifestToolPackageCheckLintsUnknown = "manifest/tool/package-check/lints/unknown"
    ManifestToolPackageCheckLintsType = "manifest/tool/package-check/lints/type"

    LintOverridable = "lint/overridable"
    LintSelectorProjectType = "lint/selector/project/type"
    LintSelectorProjectInvalid = "lint/selector/project/invalid"
    LintSelectorFilesType = "lint/selector/files/type"
    LintSelectorFilesInvalid = "lint/selector/files/invalid"
    LintSelectorFilesUnexpected = "lint/selector/files/unexpected"
    LintSelectorFilesGlob = "lint/selector/files/glob"
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

    // TODO: Packages targeting html/bundle export.
    CompileError = "compile/error"
    CompileWarning = "compile/warning"

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


    [Overridable::Staff(Project)]
    SizeBundle = "size/bundle"
    SizeArchive = "size/archive"


    [Overridable::User(Project)]
    ManifestPackageNameTypst = "manifest/package/name/typst"
    ManifestPackageDescTypst = "manifest/package/description/typst"
    ManifestPackageCompilerPatch = "manifest/package/compiler/patch"
    ManifestPackageLicenseCcOnly = "manifest/package/license/cc/only"

    [Overridable::User(Files)]
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
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SelectorType {
    Project,
    Files,
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
