use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::range::Range;
use std::sync::LazyLock;

use ignore::overrides::{Override, OverrideBuilder};
use regex_automata::meta::Regex;
use regex_syntax::hir::{Hir, Look};
use spdx::LicenseId;
use toml_edit::{Item, TableLike};
use tracing::debug;
use typst::foundations::Target;
use typst::syntax::package::{PackageSpec, PackageVersion};
use typst::syntax::{FileId, RootedPath, VirtualPath};
use typst::{Feature, Features};

use crate::check::codes::{Code, CodeType, SelectorType};
use crate::check::diagnostics::{Diagnostic, Docs, Label};
use crate::check::kebab_case::is_lower_kebab_case;
use crate::check::path::PackagePath;
use crate::check::toml::{self, Spanned, TableLikeParser, TableParser, ValueParser};
use crate::check::{Diagnostics, Result, TryExt, files};
use crate::workspace::Workspace;
use crate::world::{SystemWorld, WorldRoot};

pub struct Worlds {
    pub package: SystemWorld,
    pub template: Option<SystemWorld>,
}

/// A partially parsed manifest with spanned fields.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub package: Spanned<Package>,
    #[allow(unused)]
    pub template: Option<Spanned<Template>>,
    pub package_check: Option<Spanned<PackageCheck>>,
}

impl Manifest {
    pub fn targets(&self) -> &[Spanned<Target>] {
        self.package_check
            .as_ref()
            .and_then(|p| Some(p.targets.as_ref()?.as_slice()))
            .unwrap_or(&PackageCheck::DEFAULT_TARGETS)
    }

    pub fn template_path(&self) -> Option<&PackagePath> {
        Some(&self.template.as_ref()?.path.as_ref()?.val)
    }
}

#[derive(Debug, Clone)]
pub struct Package {
    pub entrypoint: Spanned<VirtualPath>,
    pub name: Option<Spanned<String>>,
    pub version: Option<Spanned<PackageVersion>>,
    pub exclude: Spanned<Exclude>,
    pub homepage: Option<Spanned<String>>,
    pub repository: Option<Spanned<String>>,
}

#[derive(Debug, Clone)]
pub struct Template {
    pub path: Option<Spanned<PackagePath>>,
    /// The package directory of this path is still relative to the package, not
    /// the template directory.
    pub entrypoint: Option<Spanned<VirtualPath>>,
    pub thumbnail: Option<Spanned<PackagePath>>,
}

#[derive(Debug, Clone)]
pub struct PackageCheck {
    pub targets: Option<Spanned<Vec<Spanned<Target>>>>,
    pub lints: Option<Spanned<BTreeMap<CodeType, Spanned<Lint>>>>,
}

impl PackageCheck {
    const DEFAULT_TARGETS: [Spanned<Target>; 1] = [Spanned::new_const(
        Target::Paged,
        Range { start: 0, end: 0 },
    )];
}

#[derive(Debug, Clone)]
pub struct Lint {
    pub code: Spanned<CodeType>,
    pub allowed: Spanned<Selector>,
}

#[derive(Debug, Clone)]
pub enum Selector {
    Project,
    Files(Vec<Spanned<Glob>>),
    Text(Vec<Spanned<Regex>>),
}

pub fn check(
    workspace: &Workspace,
    package_dir: &Path,
    diags: &mut Diagnostics,
    package_spec: Option<&PackageSpec>,
) -> Result<(Manifest, Worlds)> {
    let manifest_path = package_dir.join("typst.toml");
    debug!("Reading manifest at {}", &manifest_path.display());
    let manifest_contents = std::fs::read_to_string(&manifest_path).map_err(|err| {
        Diagnostic::error(Code::ManifestIo)
            .with_message(format!(
                "Failed to read manifest (typst.toml) contents ({err})"
            ))
            .with_docs_link(Docs::PACKAGE)
    })?;
    let manifest = toml_edit::Document::parse(manifest_contents.as_str()).map_err(|err| {
        Diagnostic::error(Code::ManifestTomlSyntax)
            .with_message(format!("Failed to parse manifest as TOML:\n{err}"))
            .with_docs_link(Docs::PACKAGE)
            .with_label(manifest_label(err.span().unwrap_or(0..0)))
    })?;

    let mut manifest = toml::file(diags, manifest_id(), &manifest);

    // Parse package
    let mut package = manifest
        .require_field("package", Code::ManifestPackageMissing)?
        .expect_table(Code::ManifestPackageType)?;
    let entrypoint = package
        .require_field("entrypoint", Code::ManifestPackageEntrypointMissing)?
        .expect_str(Code::ManifestPackageEntrypointType)?
        .parse(|entry| {
            VirtualPath::new(entry).map_err(|err| {
                Diagnostic::error(Code::ManifestPackageEntrypointInvalid)
                    .with_message(format!("Invalid `package.entrypoint` ({err})"))
            })
        })?
        .item;
    let name = check_name(&mut package, package_spec);
    let version = check_version(&mut package, package_spec);
    let exclude = check_exclude(package_dir, &mut package)
        .unwrap_or_else(|| Spanned::new(OverrideBuilder::new(package_dir), package.item.span()));
    check_compiler_version(&mut package);
    check_license(&mut package);
    check_description(&mut package);
    check_authors(&mut package);
    let (homepage, repository) = check_urls(&mut package);
    let package = package.item;

    // Parse template
    let template = check_template(&mut manifest, package_dir);

    // Parse tool.package-check
    let package_check = check_tool(&mut manifest);

    let exclude = build_exclude(diags, exclude, &template);
    dont_over_exclude(diags, &exclude);

    let package = package.map(|_| Package {
        entrypoint,
        name,
        version,
        exclude,
        homepage,
        repository,
    });

    if let Some(template) = &template {
        check_thumbnail(diags, template);
        dont_exclude_template_files(diags, package_dir, &package.exclude, template);
    }

    let package_spec = package_spec.cloned().or_else(|| {
        let name = package.name.as_ref()?;
        let version = package.version.as_ref()?;
        Some(PackageSpec {
            namespace: "preview".into(),
            name: name.as_ref().val.into(),
            version: version.val,
        })
    });

    let world = SystemWorld::new(
        package.entrypoint.val.clone(),
        workspace.clone(),
        WorldRoot::Package(package_dir.to_owned()),
        target_features(package_check.as_deref()),
        package_spec.clone(),
    );

    let template_world = world_for_template(
        workspace,
        package_dir,
        package_spec,
        &package,
        &template,
        &package_check,
    );

    let manifest = Manifest {
        package,
        template,
        package_check,
    };
    let worlds = Worlds {
        package: world,
        template: template_world,
    };
    Ok((manifest, worlds))
}

fn target_features(package_check: Option<&PackageCheck>) -> Features {
    package_check
        .and_then(|p| p.targets.as_ref())
        .map(|targets| {
            targets
                .iter()
                .filter_map(|t| match t.val {
                    Target::Paged => None,
                    Target::Html => Some(Feature::Html),
                    Target::Bundle => Some(Feature::Bundle),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn check_name(
    package: &mut TableParser,
    package_spec: Option<&PackageSpec>,
) -> Option<Spanned<String>> {
    let mut name = package
        .required_field("name", Code::ManifestPackageNameMissing)?
        .expected_str(Code::ManifestPackageNameType)?;

    if !is_lower_kebab_case(name.item.val) {
        Diagnostic::error(Code::ManifestPackageNameKebabCase)
            .with_message("Please use kebab-case for package names.")
            .with_docs_link(Docs::NAMING_RULES)
            .emit_at(&mut name);
    }

    if name.item.val.contains("typst") {
        Diagnostic::warn(Code::ManifestPackageNameTypst(()))
            .with_message("Package names should generally not include \"typst\".")
            .with_docs_link(Docs::NAMING_RULES)
            .emit_at(&mut name);
    }

    if let Some(package_spec) = package_spec
        && name.item.val != package_spec.name
    {
        Diagnostic::error(Code::ManifestPackageNameMismatch)
            .with_message(format!(
                "Unexpected package name. `{name}` was expected. \
                 If you want to publish a new package, create a new \
                 directory in `packages/{namespace}/`.",
                name = package_spec.name,
                namespace = package_spec.namespace,
            ))
            .emit_at(&mut name);
    }

    Some(name.item.to_owned())
}

fn check_version(
    package: &mut TableParser,
    package_spec: Option<&PackageSpec>,
) -> Option<Spanned<PackageVersion>> {
    let mut version = package
        .required_field("version", Code::ManifestPackageVersionMissing)?
        .expected_str(Code::ManifestPackageVersionType)?
        .parsed(|v| {
            v.parse::<PackageVersion>().error(
                Code::ManifestPackageVersionInvalid,
                "`version` must be a valid semantic version \
                 (i.e follow the `MAJOR.MINOR.PATCH` format).",
            )
        })?;

    if let Some(package_spec) = package_spec
        && version.item.val != package_spec.version
    {
        Diagnostic::error(Code::ManifestPackageVersionMismatch)
            .with_message(format!(
                "Unexpected version number. `{version}` was expected. \
                 If you want to publish a new version, create a new \
                 directory in `packages/{namespace}/{name}`.",
                version = package_spec.version,
                name = package_spec.name,
                namespace = package_spec.namespace,
            ))
            .emit_at(&mut version);
    }

    Some(version.item)
}

fn check_compiler_version(package: &mut TableParser) -> Option<Spanned<PackageVersion>> {
    let mut compiler = package
        .optional_field("compiler")?
        .expected_str(Code::ManifestPackageCompilerType)?
        .parsed(|c| {
            c.parse::<PackageVersion>().error(
                Code::ManifestPackageCompilerInvalid,
                "`compiler` version should be a valid semantic version, \
                 with three components (for example `0.12.0`)",
            )
        })?;

    if compiler.item.val.patch > 0 {
        let zeroed = PackageVersion {
            patch: 0,
            ..compiler.item.val
        };
        Diagnostic::warn(Code::ManifestPackageCompilerPatch(()))
            .with_message(format!(
                "The `compiler` field is a minimum version. \
                 Unless you explicitly require the latest patch release, \
                 changing it to `{zeroed}` will allow using this package with \
                 a slightly outdated compiler.",
            ))
            .emit_at(&mut compiler);
    }

    Some(compiler.item)
}

fn check_license(package: &mut TableParser) -> Option<Spanned<spdx::Expression>> {
    let license = package
        .required_field("license", Code::ManifestPackageLicenseMissing)?
        .expected_str(Code::ManifestPackageLicenseType)?
        .parsed_with(|parser, license| {
            let license = license.parse::<spdx::Expression>().map_err(|err| {
                Diagnostic::error(Code::ManifestPackageLicenseInvalid).with_message(format!(
                    "The `license` field should be a valid SPDX-2 expression:\n{err}"
                ))
            })?;

            let mut has_osi_approved = false;
            let mut cc_license = None;
            for requirement in license.requirements() {
                if let Some(id) = requirement.req.license.id() {
                    if id.is_osi_approved() {
                        has_osi_approved = true;
                    } else if is_allowed_cc(id) {
                        cc_license = Some(id);
                    } else {
                        Diagnostic::error(Code::ManifestPackageLicenseDisallowed)
                            .with_message(format!(
                                "The `license` field should be OSI approved \
                                 or an allowed Creative Commons (CC) license {}",
                                id.name
                            ))
                            .emit_at(parser);
                    }
                } else {
                    Diagnostic::error(Code::ManifestPackageLicenseInvalid)
                        .with_message("The `license` field should not contain a referencer")
                        .emit_at(parser);
                }
            }

            if let Some(cc) = cc_license
                && !has_osi_approved
            {
                Diagnostic::warn(Code::ManifestPackageLicenseCcOnly(()))
                    .with_message("This package is only licensed under a Creative Commons license.")
                    .with_hint(format!(
                        "Usually a Creative Commons license should not be used for \
                         source code. It's recommended to dual-license the package \
                         under an OSI approved license such as `MIT AND {}`",
                        cc.name
                    ))
                    .with_hint(
                        "The complete package can use a Creative Commons license, \
                         if it has been derived from work that is licensed under \
                         a Creative Commons license.\n\
                         In that case you should suppress this diagnostic.",
                    )
                    .with_docs_link(Docs::LICENSE)
                    .emit_at(parser);
            }

            Ok(license)
        })?;

    Some(license.item)
}

// Check that a license is any version of CC-BY, CC-BY-SA, or CC0.
fn is_allowed_cc(license: LicenseId) -> bool {
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^CC(-BY|-BY-SA|0)-[0-9]\.[0-9](-[A-Z]+)?$").unwrap());

    RE.is_match(license.name)
}

fn check_description(package: &mut TableParser) -> Option<Spanned<String>> {
    let desc = package
        .required_field("description", Code::ManifestPackageDescMissing)?
        .expected_str(Code::ManifestPackageDescType)?
        .parsed(|desc| {
            if desc.to_ascii_lowercase().contains("typst") {
                return Err(Diagnostic::warn(Code::ManifestPackageDescTypst(()))
                    .with_message(
                        "The `description` field should generally not contain the word `Typst`, \
                         unless this package actually has to do with Typst itself or its ecosystem",
                    )
                    .with_docs_link(Docs::DESCRIPTION));
            }
            Ok(desc)
        })?;

    Some(desc.item.to_owned())
}

fn check_authors(package: &mut TableParser) -> Option<Spanned<Vec<Spanned<String>>>> {
    let authors = package
        .required_field("authors", Code::ManifestPackageAuthorsMissing)?
        .expected_array(Code::ManifestPackageAuthorsType)?
        .validate(|parser, authors| {
            if authors.is_empty() {
                Diagnostic::error(Code::ManifestPackageAuthorsMissing)
                    .with_message("The `authors` list is empty")
                    .emit_at(parser);
            }
        })
        .parse_array(|author| {
            let author = author.expect_str(Code::ManifestPackageAuthorsType)?;
            Ok(author.item.to_owned())
        });

    Some(authors.item)
}

fn check_urls(package: &mut TableParser) -> (Option<Spanned<String>>, Option<Spanned<String>>) {
    let homepage = package
        .optional_field("homepage")
        .and_then(|h| h.expected_str(Code::ManifestPackageHomepageType))
        .map(|h| h.item.to_owned());
    let repo = package
        .optional_field("repository")
        .and_then(|r| r.expected_str(Code::ManifestPackageRepoType))
        .map(|r| r.item.to_owned());

    if let Some(repo) = &repo
        && let Some(homepage) = &homepage
        && repo.val == homepage.val
    {
        Diagnostic::error(Code::ManifestPackageHomepageRedundant)
            .with_message(
                "Use the homepage field only if there is a dedicated website. \
                 Otherwise, prefer the `repository` field.",
            )
            .with_label(manifest_label(homepage.span()))
            .emit_at(package);
    }

    (homepage, repo)
}

fn check_exclude(
    package_dir: &Path,
    package: &mut TableParser,
) -> Option<Spanned<OverrideBuilder>> {
    let exclude = package
        .optional_field("exclude")?
        .expected_array(Code::ManifestPackageExcludeType)?
        .parse_array(|exclusion| {
            let exclusion = exclusion
                .expect_str(Code::ManifestPackageExcludeType)?
                .parse_with(|parser, str| {
                    if str.starts_with('!') {
                        return Err(Diagnostic::error(Code::ManifestPackageExcludeInvalid)
                            .with_message("`exclude` globs starting with '!' are unsupported"));
                    }

                    let exclusion_str = str
                        .strip_prefix("./")
                        .inspect(|_| {
                            Diagnostic::warn(Code::ManifestPackageExcludeLeadingDot)
                                .with_message("Leading `./` of exclusions are trimmed.")
                                .with_hint(
                                    "Use an absolute path starting with `/` to \
                                     avoid recursive matching.",
                                )
                                .emit_at(parser);
                        })
                        .unwrap_or(str);

                    Ok(format!("!{exclusion_str}"))
                })?;

            Ok(exclusion.item)
        })
        .parsed(|exclude| {
            let mut globs = OverrideBuilder::new(package_dir);
            for glob in exclude {
                globs.add(&glob.val).map_err(|err| {
                    Diagnostic::error(Code::ManifestPackageExcludeInvalid)
                        .with_message(format!("Invalid exclude `glob` ({err})"))
                        .with_label(manifest_label(glob.span()))
                })?;
            }
            Ok(globs)
        })?;
    Some(exclude.item)
}

fn build_exclude(
    diags: &mut Diagnostics,
    mut exclude_globs: Spanned<OverrideBuilder>,
    template: &Option<Spanned<Template>>,
) -> Spanned<Exclude> {
    let thumbnail_path = template.as_ref().and_then(|t| t.thumbnail.as_ref());
    if let Some(thumbnail_path) = thumbnail_path {
        // Check if the thumbnail is directly excluded, it's fine if a parent
        // directory is excluded.
        if let Ok(exclude) = exclude_globs.build()
            && exclude.matched(thumbnail_path.full(), false).is_ignore()
        {
            Diagnostic::warn(Code::ExcludeThumbnail)
                .with_message("The template thumbnail is automatically excluded")
                .with_label(manifest_label(thumbnail_path.span()))
                .with_docs_link(Docs::WHAT_TO_EXCLUDE)
                .emit(diags);
        }

        exclude_globs
            .add(&format!("!{}", thumbnail_path.relative().display()))
            .ok();
    }

    let exclude = match exclude_globs.build() {
        Ok(globs) => Exclude::new(globs),
        Err(err) => {
            Diagnostic::error(Code::ManifestPackageExcludeInvalid)
                .with_message(format!("Invalid exclude globs ({err})"))
                .with_label(manifest_label(exclude_globs.span()))
                .with_docs_link(Docs::PACKAGE)
                .emit(diags);
            Exclude::empty()
        }
    };

    exclude_globs.map(|_| exclude)
}

fn dont_over_exclude(diags: &mut Diagnostics, exclude: &Spanned<Exclude>) {
    if exclude.matches_relative_file("LICENSE") {
        Diagnostic::warn(Code::ExcludeLicense)
            .with_message("Your LICENSE file should not be excluded.")
            .with_label(manifest_label(exclude.span()))
            .emit(diags);
    }

    if exclude.matches_relative_file("README.md") {
        Diagnostic::warn(Code::ExcludeReadme)
            .with_message("Your README.md file should not be excluded.")
            .with_label(manifest_label(exclude.span()))
            .emit(diags);
    }
}

#[derive(Debug, Clone)]
pub struct Exclude {
    globs: Override,
}

impl Exclude {
    pub fn empty() -> Self {
        Self {
            globs: Override::empty(),
        }
    }

    pub fn new(globs: Override) -> Self {
        Self { globs }
    }

    pub fn root(&self) -> &Path {
        self.globs.path()
    }

    /// Whether the package file path is excluded.
    pub fn matches_file<T: AsRef<Path>>(&self, path: &PackagePath<T>) -> bool {
        self.matches_relative_file(path.relative())
    }

    /// Whether the package relative file path is excluded.
    pub fn matches_relative_file(&self, relative_path: impl AsRef<Path>) -> bool {
        match_relative_file(relative_path.as_ref(), |path, is_dir| {
            self.globs.matched(path, is_dir).is_ignore()
        })
    }
}

#[derive(Debug, Clone)]
pub struct Glob(globset::GlobMatcher);

impl Glob {
    pub fn new(glob: &str) -> std::result::Result<Self, globset::Error> {
        let glob = globset::Glob::new(glob)?;
        Ok(Self(glob.compile_matcher()))
    }

    /// Whether the package file path matches.
    pub fn matches_file<T: AsRef<Path>>(&self, path: &PackagePath<T>) -> bool {
        self.matches_relative_file(path.relative())
    }

    /// Whether the package relative file path matches.
    pub fn matches_relative_file(&self, relative_path: impl AsRef<Path>) -> bool {
        let only_dir = self.0.glob().glob().ends_with('/');
        match_relative_file(relative_path.as_ref(), |path, is_dir| {
            if only_dir && !is_dir {
                return false;
            }
            self.0.is_match(path)
        })
    }
}

/// Returns true, if the file itself or any of its parent directories match.
fn match_relative_file<F>(relative_path: &Path, is_match: F) -> bool
where
    F: Fn(&Path, bool) -> bool,
{
    if is_match(relative_path, false) {
        return true;
    }

    // Manually check if any of the parent directories is excluded.
    // This has to be done to mimic the behavior of [`ignore::Walk`], which
    // won't enter a directory if it is ignored. [`Override::matched`]
    // function doesn't check if a parent directory is excluded.
    let mut parent = relative_path.parent();
    while let Some(dir) = parent {
        if is_match(dir, true) {
            return true;
        }
        parent = dir.parent();
    }

    false
}

fn check_template(manifest: &mut toml::File, package_dir: &Path) -> Option<Spanned<Template>> {
    let mut template = manifest
        .optional_field("template")?
        .expected_table(Code::ManifestTemplateType)?;

    let path = template
        .required_field("path", Code::ManifestTemplatePathMissing)
        .and_then(|p| p.expected_str(Code::ManifestTemplatePathType))
        .and_then(|p| {
            p.parsed(|path| {
                // The template path was previously naively joined in `typst-cli` and
                // thus shouldn't be absolute, otherwise it could escape the package
                // directory, when initializing a template.
                if path.trim().starts_with("/") {
                    return Err(Diagnostic::error(Code::ManifestTemplatePathInvalid)
                        .with_message(format!("`template.path` is absolute `{path}`"))
                        .with_hint("Use a relative path instead."));
                }

                PackagePath::from_relative(package_dir, path).map_err(|err| {
                    Diagnostic::error(Code::ManifestTemplatePathInvalid)
                        .with_message(format!("`template.path` is invalid `{path}` ({err})"))
                })
            })
        })
        .map(|path| path.item);

    let entrypoint = template
        .required_field("entrypoint", Code::ManifestTemplateEntrypointMissing)
        .and_then(|p| p.expected_str(Code::ManifestTemplateEntrypointType))
        .and_then(|entry| {
            entry.parsed(|entry| {
                VirtualPath::new(entry).map_err(|err| {
                    Diagnostic::error(Code::ManifestTemplateEntrypointInvalid).with_message(
                        format!("`template.entrypoint` is invalid `{entry}` ({err})"),
                    )
                })
            })
        })
        .map(|path| path.item);

    let thumbnail = template
        .required_field("thumbnail", Code::ManifestTemplateThumbnailMissing)
        .and_then(|thumb| thumb.expected_str(Code::ManifestTemplateThumbnailType))
        .and_then(|thumb| {
            thumb.parsed(|thumb| {
                PackagePath::from_relative(package_dir, thumb).map_err(|err| {
                    Diagnostic::error(Code::ManifestTemplateThumbnailInvalid)
                        .with_message(format!("`template.thumbnail` is invalid `{thumb}` ({err})"))
                })
            })
        })
        .map(|path| path.item);

    Some(template.item.map(|_| Template {
        path,
        entrypoint,
        thumbnail,
    }))
}

fn world_for_template(
    workspace: &Workspace,
    package_dir: &Path,
    package_spec: Option<PackageSpec>,
    package: &Spanned<Package>,
    template: &Option<Spanned<Template>>,
    package_check: &Option<Spanned<PackageCheck>>,
) -> Option<SystemWorld> {
    let package_spec = package_spec?;
    let template = template.as_ref()?;
    let template_dir = template.path.as_ref()?;
    let main = template.entrypoint.as_ref()?;

    let root = WorldRoot::Template {
        package: package_dir.to_owned(),
        template: template_dir.full().to_owned(),
    };

    let world = SystemWorld::new(
        main.val.clone(),
        workspace.clone(),
        root,
        target_features(package_check.as_deref()),
        Some(package_spec),
    );

    Some(world.exclude(package.exclude.val.clone()))
}

fn dont_exclude_template_files(
    diags: &mut Diagnostics,
    package_dir: &Path,
    exclude: &Exclude,
    template: &Spanned<Template>,
) {
    let Some(template_path) = &template.path else {
        return;
    };

    for entry in files::walk(template_path.full()).flatten() {
        let path = PackagePath::from_full(package_dir, entry.path());

        // For build artifacts, ask the package author to delete them.
        let ext = path.extension().and_then(|e| e.to_str());
        if matches!(ext, Some("pdf" | "png" | "svg")) && entry.path().with_extension("typ").exists()
        {
            Diagnostic::error(Code::FilesCompilationArtifact(path.virtual_path()))
                .with_message(
                    "This file is a compiled document and should \
                     not be included in the template. \
                     Please delete it.",
                )
                .with_docs_link(Docs::WHAT_TO_EXCLUDE)
                .with_label(Label::new(path.file_id(), 0..0))
                .emit(diags);
            continue;
        }

        // For other files, check that they are indeed not excluded.
        if entry.metadata().is_ok_and(|m| m.is_file()) && exclude.matches_file(&path) {
            Diagnostic::error(Code::ExcludeTemplate)
                .with_message("This file is part of the template and should not be excluded.")
                .with_docs_link(Docs::WHAT_TO_EXCLUDE)
                .with_label(Label::new(path.file_id(), 0..0))
                .emit(diags);
        }
    }
}

fn check_thumbnail(diags: &mut Diagnostics, template: &Spanned<Template>) {
    let Some(thumbnail_path) = &template.thumbnail else {
        return;
    };

    if !thumbnail_path.full().exists() {
        Diagnostic::error(Code::ManifestTemplateThumbnailNotFound)
            .with_message("This file does not exist.")
            .with_docs_link(Docs::TEMPLATE)
            .with_label(manifest_label(thumbnail_path.span()))
            .emit(diags);
    }

    if !matches!(
        thumbnail_path.extension().and_then(|e| e.to_str()),
        Some("png" | "webp")
    ) {
        Diagnostic::error(Code::ManifestTemplateThumbnailFormat)
            .with_message("Thumbnails should be PNG or WebP files.")
            .with_docs_link(Docs::TEMPLATE)
            .with_label(manifest_label(thumbnail_path.span()))
            .emit(diags);
    }

    if let Some(template_path) = &template.path
        && thumbnail_path.full().starts_with(template_path.full())
    {
        Diagnostic::error(Code::ManifestTemplateThumbnailLocation)
            .with_message(
                "The thumbnail file should be outside of the template directory.\n\n\
                 When your template will be used as a base for users's projects, \
                 the template directory will be copied as is, and the thumbnail file \
                 is generally not displayed in documents based on your template.",
            )
            .with_docs_link(Docs::TEMPLATE)
            .with_label(manifest_label(thumbnail_path.span()))
            .emit(diags);
    }
}

fn check_tool(manifest: &mut toml::File) -> Option<Spanned<PackageCheck>> {
    let mut tools = manifest
        .optional_field("tool")?
        .expected_table(Code::ManifestToolType)?;
    let mut package_check = tools
        .optional_field("package-check")?
        .expected_table(Code::ManifestToolPackageCheckType)?;

    let targets = package_check
        .optional_field("targets")
        .and_then(|t| t.expected_array(Code::ManifestToolPackageCheckTargetsType))
        .map(|t| {
            let mut seen = HashSet::new();
            t.validate(|parser, targets| {
                if targets.is_empty() {
                    Diagnostic::error(Code::ManifestToolPackageCheckTargetsEmpty)
                        .with_message("The `targets` list must be non-empty")
                        .emit_at(parser);
                }
            })
            .parse_array(|t| {
                let target = t
                    .expect_str(Code::ManifestToolPackageCheckTargetsType)?
                    .parse(|t| {
                        let target = match t {
                            "paged" => Target::Paged,
                            "html" => Target::Html,
                            "bundle" => Target::Bundle,
                            _ => {
                                return Err(Diagnostic::error(
                                    Code::ManifestToolPackageCheckTargetsUnknown,
                                )
                                .with_message(format!("unknown compiler target `{t:?}`")));
                            }
                        };

                        if !seen.insert(target) {
                            return Err(Diagnostic::error(
                                Code::ManifestToolPackageCheckTargetsDuplicate,
                            )
                            .with_message(format!("duplicate compiler target `{t:?}`")));
                        }

                        Ok(target)
                    })?;
                Ok(target.item)
            })
            .item
        })
        // Filter out empty arrays, otherwise no compile checks will be run.
        // Do this after parsing array elements, since `parse_array` does
        // `filter_map` internally.
        .filter(|t| !t.is_empty());

    let lints = package_check
        .optional_field("lints")
        .and_then(|l| l.expected_table(Code::ManifestToolPackageCheckLintsType))
        .map(|l| {
            l.parse_map(
                |lint| {
                    lint.parse::<CodeType>().map_err(|_| {
                        Diagnostic::warn(Code::ManifestToolPackageCheckLintsUnknown)
                            .with_message(format!("Unknown lint code `{lint}`"))
                    })
                },
                |code, selector| {
                    let span = span_across(code.span(), selector.item.span());
                    let Some(selector_type) = code.overridable().selector_type() else {
                        return Err(Diagnostic::error(Code::LintOverridable)
                            .with_message(format!("`{}` cannot be overriden", code.val))
                            .with_label(manifest_label(span)));
                    };
                    let allowed = parse_selector_type(selector, selector_type)?;
                    Ok(Spanned::new(
                        Lint {
                            code: code.to_owned(),
                            allowed,
                        },
                        span,
                    ))
                },
            )
        })
        .map(|lints| lints.item);

    Some(package_check.item.map(|_| PackageCheck { targets, lints }))
}

fn span_across(a: Range<usize>, b: Range<usize>) -> Range<usize> {
    Range::from(a.start.min(b.start)..a.end.max(b.end))
}

fn parse_selector_type(
    selector: ValueParser<Spanned<&Item>>,
    selector_type: SelectorType,
) -> Result<Spanned<Selector>> {
    let mut selector = selector.expect_inline_table(Code::LintSelectorType)?;

    const ALLOW: &str = "allow";
    selector.unexpected(Code::LintSelectorUnexpected, [ALLOW]);

    match selector_type {
        SelectorType::Project => {
            let allow = selector
                .require_field(ALLOW, Code::LintSelectorProjectInvalid)?
                .expect_bool(Code::LintSelectorProjectType)?
                .parse(|val| {
                    if !val {
                        return Err(Diagnostic::error(Code::LintSelectorProjectInvalid)
                            .with_message(r#"a project level selector must be `"allow"`"#));
                    }
                    Ok(Selector::Project)
                })?;
            Ok(allow.item)
        }
        SelectorType::File => {
            let allow = selector
                .require_field(ALLOW, Code::LintSelectorFileType)?
                .expect_inline_table(Code::LintSelectorFileType)?;
            parse_files_selector(allow)
        }
        SelectorType::Text => {
            let allow = selector
                .require_field(ALLOW, Code::LintSelectorTextType)?
                .expect_inline_table(Code::LintSelectorTextType)?;
            parse_text_selector(allow)
        }
    }
}

fn parse_files_selector<T: TableLike>(
    mut selector: TableLikeParser<T>,
) -> Result<Spanned<Selector>> {
    const FIELD: &str = "files";
    selector.unexpected(Code::LintSelectorFileUnexpected, [FIELD]);
    let globs = selector
        .require_field(FIELD, Code::LintSelectorFileInvalid)?
        .expect_array(Code::LintSelectorFileType)?
        .parse_array(|item| {
            let glob = item.expect_str(Code::LintSelectorFileType)?.parse(|str| {
                Glob::new(str).map_err(|err| {
                    Diagnostic::error(Code::LintSelectorFileGlob)
                        .with_message(format!("invalid file glob ({err})"))
                })
            })?;
            Ok(glob.item)
        });
    Ok(globs.item.map(Selector::Files))
}

fn parse_text_selector<T: TableLike>(
    mut selector: TableLikeParser<T>,
) -> Result<Spanned<Selector>> {
    const FIELD: &str = "matches";
    selector.unexpected(Code::LintSelectorTextUnexpected, [FIELD]);
    let regexes = selector
        .require_field(FIELD, Code::LintSelectorTextInvalid)?
        .expect_array(Code::LintSelectorTextType)?
        .parse_array(|item| {
            let regex = item.expect_str(Code::LintSelectorTextType)?.parse(|str| {
                let hir = regex_syntax::parse(str).map_err(|err| {
                    Diagnostic::error(Code::LintSelectorTextRegex).with_message(format!("{err}"))
                })?;
                // Build an anchored regex that has to match the *full* input.
                let hir = Hir::concat(vec![Hir::look(Look::Start), hir, Hir::look(Look::End)]);
                regex_automata::meta::Builder::new()
                    .build_from_hir(&hir)
                    .map_err(|err| {
                        Diagnostic::error(Code::LintSelectorTextRegex)
                            .with_message(format!("{err}"))
                    })
            })?;
            Ok(regex.item)
        });
    Ok(regexes.item.map(Selector::Text))
}

pub fn manifest_label(span: impl Into<Range<usize>>) -> Label {
    Label::new(manifest_id(), span)
}

pub fn manifest_id() -> FileId {
    FileId::new(RootedPath::new(
        typst::syntax::VirtualRoot::Project,
        VirtualPath::new("typst.toml").unwrap(),
    ))
}
