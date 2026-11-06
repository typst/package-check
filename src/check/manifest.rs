use std::path::Path;
use std::range::Range;

use ignore::overrides::{Override, OverrideBuilder};
use toml_edit::{Array, Item, Table};
use tracing::debug;
use typst::syntax::package::{PackageSpec, PackageVersion};
use typst::syntax::{FileId, RootedPath, VirtualPath};

use crate::check::diagnostics::{
    Code, Diagnostic, Docs, EmittableDiagnostic, EmittableResult, Label, MaybeEmit,
};
use crate::check::path::PackagePath;
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
}

impl Manifest {
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
            .with_message(format!(
                "Failed to parse manifest as TOML ({})",
                err.message()
            ))
            .with_docs_link(Docs::PACKAGE)
            .with_label(manifest_label(err.span().unwrap_or(0..0)))
    })?;

    // Parse package
    let mut package = require_field(diags, &manifest, "package", Code::ManifestPackageMissing)?
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
    let template = optional_field(diags, &manifest, "template")
        .and_then(|t| t.expected_table(Code::ManifestTemplateType))
        .and_then(|template| check_template(template, package_dir));

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
        package_spec.clone(),
    );

    let template_world =
        world_for_template(workspace, package_dir, package_spec, &package, &template);

    let manifest = Manifest { package, template };
    let worlds = Worlds {
        package: world,
        template: template_world,
    };
    Ok((manifest, worlds))
}

fn check_name(
    package: &mut TableParser,
    package_spec: Option<&PackageSpec>,
) -> Option<Spanned<String>> {
    let name = package
        .required_field("name", Code::ManifestPackageNameMissing)?
        .expected_str(Code::ManifestPackageNameType)?;

    if name.item.val != casbab::kebab(name.item.val) {
        name.error(Code::ManifestPackageNameKebabCase)
            .with_message("Please use kebab-case for package names.")
            .with_docs_link(Docs::NAMING_RULES)
            .emit(name.diags);
    }

    if name.item.val.contains("typst") {
        name.warn(Code::ManifestPackageNameTypst)
            .with_message("Package names should generally not include \"typst\".")
            .with_docs_link(Docs::NAMING_RULES)
            .emit(name.diags);
    }

    if let Some(package_spec) = package_spec
        && name.item.val != package_spec.name
    {
        name.error(Code::ManifestPackageNameMismatch)
            .with_message(format!(
                "Unexpected package name. `{name}` was expected. \
                 If you want to publish a new package, create a new \
                 directory in `packages/{namespace}/`.",
                name = package_spec.name,
                namespace = package_spec.namespace,
            ))
            .emit(name.diags);
    }

    Some(name.item.to_owned())
}

fn check_version(
    package: &mut TableParser,
    package_spec: Option<&PackageSpec>,
) -> Option<Spanned<PackageVersion>> {
    let version = package
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
        version
            .error(Code::ManifestPackageVersionMismatch)
            .with_message(format!(
                "Unexpected version number. `{version}` was expected. \
                 If you want to publish a new version, create a new \
                 directory in `packages/{namespace}/{name}`.",
                version = package_spec.version,
                name = package_spec.name,
                namespace = package_spec.namespace,
            ))
            .emit(version.diags);
    }

    Some(version.item)
}

fn check_compiler_version(package: &mut TableParser) -> Option<Spanned<PackageVersion>> {
    let compiler = package
        .optional_field("compiler")?
        .expected_str(Code::ManifestPackageCompilerType)?
        .parsed(|c| {
            c.parse::<PackageVersion>().error(
                Code::ManifestPackageCompilerInvalid,
                "`compiler` version should be a valid semantic version, \
                 with three components (for example `0.12.0`)",
            )
        })?;
    Some(compiler.item)
}

fn check_license(package: &mut TableParser) -> Option<Spanned<spdx::Expression>> {
    let license = package
        .required_field("license", Code::ManifestPackageLicenseMissing)?
        .expected_str(Code::ManifestPackageLicenseType)?
        .parsed(|l| {
            l.parse::<spdx::Expression>().error(
                Code::ManifestPackageLicenseInvalid,
                "The `license` field should be a valid SPDX-2 expression",
            )
        })?;

    for requirement in license.item.requirements() {
        if let Some(id) = requirement.req.license.id() {
            // TODO: Allow creative commons licenses.
            if !id.is_osi_approved() {
                license
                    .error(Code::ManifestPackageLicenseOsi)
                    .with_message("The `license` field should be OSI approved")
                    .emit(license.diags);
            }
        } else {
            license
                .error(Code::ManifestPackageLicenseInvalid)
                .with_message("The `license` field should not contain a referencer")
                .emit(license.diags);
        }
    }

    Some(license.item)
}

fn check_description(package: &mut TableParser) -> Option<Spanned<String>> {
    let desc = package
        .required_field("description", Code::ManifestPackageDescMissing)?
        .expected_str(Code::ManifestPackageDescType)?;

    if desc.item.val.to_ascii_lowercase().contains("typst") {
        desc.warn(Code::ManifestPackageDescTypst)
            .with_message("The `description` field should generally not contain the word `Typst`")
            .with_docs_link(Docs::DESCRIPTION)
            .emit(desc.diags);
    }

    Some(desc.item.to_owned())
}

fn check_authors(package: &mut TableParser) -> Option<Spanned<Vec<String>>> {
    let authors = package
        .required_field("authors", Code::ManifestPackageAuthorsMissing)?
        .expected_array(Code::ManifestPackageAuthorsType)?
        .parsed(|authors| {
            if authors.is_empty() {
                return Err(Diagnostic::error(Code::ManifestPackageAuthorsMissing)
                    .with_message("The `authors` list is empty"));
            }

            authors
                .iter()
                .map(|author| {
                    author.as_str().map(str::to_string).ok_or_else(|| {
                        Diagnostic::error(Code::ManifestPackageAuthorsType)
                            .with_message("The `authors` list should only contain strings")
                            .with_label(manifest_label(author.span().unwrap()))
                    })
                })
                .collect::<Result<Vec<_>>>()
        })?;
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
            .emit(package.diags);
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
        .parsed_with(|parser, exclude| {
            let mut exclude_globs = OverrideBuilder::new(package_dir);
            for exclusion in exclude.iter() {
                let Some(exclusion_str) = exclusion.as_str() else {
                    return Err(parser
                        .error(Code::ManifestPackageExcludeType)
                        .with_message("`exclude` glob must be a string")
                        .with_label(manifest_label(exclusion.span().unwrap())));
                };

                if exclusion_str.starts_with('!') {
                    return Err(parser
                        .error(Code::ManifestPackageExcludeInvalid)
                        .with_message("`exclude` globs starting with '!' are unsupported")
                        .with_label(manifest_label(exclusion.span().unwrap())));
                }

                let exclusion_str = exclusion_str
                    .strip_prefix("./")
                    .inspect(|_| {
                        parser
                            .warn(Code::ManifestPackageExcludeLeadingDot)
                            .with_message("Leading `./` of exclusions are trimmed.")
                            .with_hint(
                                "Use an absolute path starting with `/` to \
                                 avoid recursive matching.",
                            )
                            .with_label(manifest_label(exclusion.span().unwrap()))
                            .emit(parser.diags);
                    })
                    .unwrap_or(exclusion_str);
                exclude_globs.add(&format!("!{exclusion_str}")).ok();
            }

            Ok(exclude_globs)
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
        let relative_path = relative_path.as_ref();
        if self.globs.matched(relative_path, false).is_ignore() {
            return true;
        }

        // Manually check if any of the parent directories is excluded.
        // This has to be done to mimic the behavior of [`ignore::Walk`], which
        // won't enter a directory if it is ignored. [`Override::matched`]
        // function doesn't check if a parent directory is excluded.
        let mut parent = relative_path.parent();
        while let Some(dir) = parent {
            if self.globs.matched(dir, true).is_ignore() {
                return true;
            }
            parent = dir.parent();
        }

        false
    }
}

fn check_template(mut template: TableParser, package_dir: &Path) -> Option<Spanned<Template>> {
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
        let entry_path = PackagePath::from_full(package_dir, entry.path());

        // For build artifacts, ask the package author to delete them.
        let ext = entry.path().extension().and_then(|e| e.to_str());
        if matches!(ext, Some("pdf" | "png" | "svg")) && entry.path().with_extension("typ").exists()
        {
            Diagnostic::error(Code::FilesCompilationArtifact)
                .with_message(
                    "This file is a compiled document and should \
                     not be included in the template. \
                     Please delete it.",
                )
                .with_docs_link(Docs::WHAT_TO_EXCLUDE)
                .with_label(Label::new(entry_path.file_id(), 0..0))
                .emit(diags);
            continue;
        }

        // For other files, check that they are indeed not excluded.
        if entry.metadata().is_ok_and(|m| m.is_file()) && exclude.matches_file(&entry_path) {
            Diagnostic::error(Code::ExcludeTemplate)
                .with_message("This file is part of the template and should not be excluded.")
                .with_docs_link(Docs::WHAT_TO_EXCLUDE)
                .with_label(Label::new(entry_path.file_id(), 0..0))
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

pub fn manifest_label(span: impl Into<Range<usize>>) -> Label {
    Label::new(manifest_id(), span)
}

pub fn manifest_id() -> FileId {
    FileId::new(RootedPath::new(
        typst::syntax::VirtualRoot::Project,
        VirtualPath::new("typst.toml").unwrap(),
    ))
}

fn require_field<'d, 'v>(
    diags: &'d mut Diagnostics,
    manifest: &'v toml_edit::Document<&str>,
    key: &'static str,
    missing: Code,
) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v Item>>> {
    let Some(item) = manifest.get_spanned(key) else {
        let diag = Diagnostic::error(missing)
            .with_message(format!("Missing `[{key}]` table, which is required."))
            .with_label(manifest_label(0..0))
            .with_docs_link(Docs::for_manifest_key(None));
        return Err(EmittableDiagnostic::new(diags, diag));
    };
    Ok(ValueParser {
        diags,
        parent: None,
        key,
        item,
    })
}

fn optional_field<'d, 'v>(
    diags: &'d mut Diagnostics,
    manifest: &'v toml_edit::Document<&str>,
    key: &'static str,
) -> Option<ValueParser<'d, Spanned<&'v Item>>> {
    let item = manifest.get_spanned(key)?;
    Some(ValueParser {
        diags,
        parent: None,
        key,
        item,
    })
}

type TableParser<'d, 'v> = ValueParser<'d, Spanned<&'v Table>>;

struct ValueParser<'d, T> {
    diags: &'d mut Diagnostics,
    parent: Option<&'static str>,
    key: &'static str,
    item: T,
}

impl<'d, T> ValueParser<'d, Spanned<T>> {
    /// Add information related to this field to the diagnostic.
    fn enrich(&self, mut diag: Diagnostic) -> Diagnostic {
        if diag.label.is_none() {
            diag = diag.with_label(manifest_label(self.item.span()));
        }
        if diag.docs_link.is_none() {
            diag = diag.with_docs_link(Docs::for_manifest_key(self.parent))
        }
        diag
    }

    /// Create an [`enrich`](Self::enrich)ed error that.
    fn error(&self, code: Code) -> Diagnostic {
        self.enrich(Diagnostic::error(code))
    }

    /// Create an [`enrich`](Self::enrich)ed warning that.
    fn warn(&self, code: Code) -> Diagnostic {
        self.enrich(Diagnostic::warn(code))
    }

    fn take_value(self) -> (ValueParser<'d, Spanned<()>>, T) {
        let parser = ValueParser {
            diags: self.diags,
            parent: self.parent,
            key: self.key,
            item: Spanned::new((), self.item.span),
        };
        (parser, self.item.val)
    }

    fn with_value<V>(self, val: V) -> ValueParser<'d, Spanned<V>> {
        self.map(|_| val)
    }

    fn map<F, V>(self, map: F) -> ValueParser<'d, Spanned<V>>
    where
        F: FnOnce(T) -> V,
    {
        ValueParser {
            diags: self.diags,
            parent: self.parent,
            key: self.key,
            item: self.item.map(map),
        }
    }
}

impl<'d, 'v> TableParser<'d, 'v> {
    fn require_field<'slf>(
        &'slf mut self,
        key: &'static str,
        missing: Code,
    ) -> EmittableResult<'slf, ValueParser<'slf, Spanned<&'v Item>>> {
        let table_key = self.key;
        let Some(item) = self.item.get_spanned(key) else {
            let diag = self
                .error(missing)
                .with_message(format!("Missing `{table_key}.{key}`, which is required."));
            return Err(EmittableDiagnostic::new(self.diags, diag));
        };
        Ok(ValueParser {
            diags: self.diags,
            parent: Some(table_key),
            key,
            item,
        })
    }

    fn required_field<'slf>(
        &'slf mut self,
        key: &'static str,
        missing: Code,
    ) -> Option<ValueParser<'slf, Spanned<&'v Item>>> {
        self.require_field(key, missing).maybe_emit()
    }

    fn optional_field<'slf>(
        &'slf mut self,
        key: &'static str,
    ) -> Option<ValueParser<'slf, Spanned<&'v Item>>> {
        let item = self.item.get_spanned(key);
        Some(ValueParser {
            diags: self.diags,
            parent: Some(self.key),
            key,
            item: item?,
        })
    }
}

impl<'d, T> ValueParser<'d, Spanned<T>> {
    fn parse_with<F, V>(self, parse: F) -> EmittableResult<'d, ValueParser<'d, Spanned<V>>>
    where
        F: for<'t> FnOnce(&mut ValueParser<'t, Spanned<()>>, T) -> Result<V>,
    {
        let (mut field, val) = self.take_value();
        match parse(&mut field, val) {
            Ok(val) => Ok(field.with_value(val)),
            Err(diag) => {
                let diag = field.enrich(diag);
                Err(EmittableDiagnostic::new(field.diags, diag))
            }
        }
    }

    fn parse<F, V>(self, parse: F) -> EmittableResult<'d, ValueParser<'d, Spanned<V>>>
    where
        F: FnOnce(T) -> Result<V>,
    {
        self.parse_with(|_, val| parse(val))
    }

    fn parsed_with<F, V>(self, parse: F) -> Option<ValueParser<'d, Spanned<V>>>
    where
        F: for<'t> FnOnce(&mut ValueParser<'t, Spanned<()>>, T) -> Result<V>,
    {
        self.parse_with(parse).maybe_emit()
    }

    fn parsed<F, V>(self, parse: F) -> Option<ValueParser<'d, Spanned<V>>>
    where
        F: FnOnce(T) -> Result<V>,
    {
        self.parse(parse).maybe_emit()
    }
}

impl<'d, 'v> ValueParser<'d, Spanned<&'v Item>> {
    fn expect_type<V: ?Sized, F>(
        self,
        ty: Code,
        what: &'static str,
        expect: F,
    ) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v V>>>
    where
        F: for<'a> FnOnce(&'a Item) -> Option<&'a V>,
    {
        self.parse_with(|field, item| {
            expect(item).ok_or_else(|| {
                let key = field.key;
                Diagnostic::error(ty).with_message(match field.parent {
                    Some(parent) => format!("The `{parent}.{key}` field should be {what}"),
                    None => format!("The `{key}` field should be {what}"),
                })
            })
        })
    }

    fn expect_table(self, ty: Code) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v Table>>> {
        self.expect_type(ty, "a table", Item::as_table)
    }

    fn expected_table(self, ty: Code) -> Option<ValueParser<'d, Spanned<&'v Table>>> {
        self.expect_table(ty).maybe_emit()
    }

    fn expect_str(self, ty: Code) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v str>>> {
        self.expect_type(ty, "a string", Item::as_str)
    }

    fn expected_str(self, ty: Code) -> Option<ValueParser<'d, Spanned<&'v str>>> {
        self.expect_str(ty).maybe_emit()
    }

    fn expect_array(self, ty: Code) -> EmittableResult<'d, ValueParser<'d, Spanned<&'v Array>>> {
        self.expect_type(ty, "an array", Item::as_array)
    }

    fn expected_array(self, ty: Code) -> Option<ValueParser<'d, Spanned<&'v Array>>> {
        self.expect_array(ty).maybe_emit()
    }
}

#[derive(Debug, Copy, Clone)]
pub struct Spanned<T> {
    pub val: T,
    span: Range<usize>,
}

impl<T> Spanned<T> {
    fn new(val: T, span: impl Into<Range<usize>>) -> Self {
        Self {
            val,
            span: span.into(),
        }
    }

    pub fn span(&self) -> Range<usize> {
        self.span
    }

    pub fn as_ref(&self) -> Spanned<&T> {
        Spanned {
            val: &self.val,
            span: self.span,
        }
    }

    pub fn map<F, V>(self, f: F) -> Spanned<V>
    where
        F: FnOnce(T) -> V,
    {
        Spanned {
            val: f(self.val),
            span: self.span,
        }
    }

    pub fn filter_map<F, V>(self, f: F) -> Option<Spanned<V>>
    where
        F: FnOnce(T) -> Option<V>,
    {
        Some(Spanned {
            val: f(self.val)?,
            span: self.span,
        })
    }

    pub fn try_map<F, V, E>(self, f: F) -> std::result::Result<Spanned<V>, E>
    where
        F: FnOnce(T) -> std::result::Result<V, E>,
    {
        Ok(Spanned {
            val: f(self.val)?,
            span: self.span,
        })
    }
}

impl<T: ?Sized + ToOwned> Spanned<&T> {
    pub fn to_owned(self) -> Spanned<T::Owned>
    where
        T: ToOwned,
    {
        Spanned {
            val: self.val.to_owned(),
            span: self.span,
        }
    }
}

impl<T> std::ops::Deref for Spanned<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.val
    }
}

impl<T> std::ops::DerefMut for Spanned<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.val
    }
}

trait TomlExt {
    fn get_spanned(&self, name: &'static str) -> Option<Spanned<&Item>>;
}

impl TomlExt for Table {
    fn get_spanned(&self, name: &'static str) -> Option<Spanned<&Item>> {
        let item = self.get(name)?;
        // The span should only ever be none for user-created tables, not
        // deserialized ones.
        let span = item.span()?;
        Some(Spanned::new(item, span))
    }
}
