use std::path::PathBuf;
use std::{ops::Range, path::Path, str::FromStr};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use ignore::overrides::{Override, OverrideBuilder};
use reqwest::StatusCode;
use toml_edit::{Array, Item, Table};
use tracing::{debug, warn};
use typst::syntax::{
    package::{PackageSpec, PackageVersion},
    FileId, VirtualPath,
};

use crate::check::path::{self, PackagePath};
use crate::{
    check::{Diagnostics, Result, TryExt},
    world::SystemWorld,
};

pub struct Worlds {
    pub package: SystemWorld,
    pub template: Option<SystemWorld>,
}

/// A partially parsed manifest with spanned fields.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub package: Spanned<Package>,
    pub template: Option<Spanned<Template>>,
}

impl Manifest {
    pub fn thumbnail(&self) -> Option<Spanned<PackagePath<&Path>>> {
        let thumbnail = self.template.as_ref()?.thumbnail.as_ref()?;
        Some(thumbnail.as_ref().map(PackagePath::as_path))
    }
}

#[derive(Debug, Clone)]
pub struct Package {
    pub entrypoint: Spanned<PackagePath>,
    pub name: Option<Spanned<String>>,
    pub version: Option<Spanned<PackageVersion>>,
    pub exclude: Spanned<Override>,
}

#[derive(Debug, Clone)]
pub struct Template {
    pub path: Option<Spanned<PackagePath>>,
    /// The package directory of this path is still relative to the package, not
    /// the template directory.
    pub entrypoint: Option<Spanned<PackagePath>>,
    pub thumbnail: Option<Spanned<PackagePath>>,
}

pub async fn check(
    package_dir: &Path,
    diags: &mut Diagnostics,
    package_spec: Option<&PackageSpec>,
) -> Result<(Manifest, Worlds)> {
    let manifest_path = package_dir.join("typst.toml");
    debug!("Reading manifest at {}", &manifest_path.display());
    let manifest_contents = std::fs::read_to_string(&manifest_path).map_err(|e| {
        Diagnostic::error()
            .with_code("manifest/io")
            .with_message(format!(
                "Failed to read manifest contents (typst.toml). {e}"
            ))
    })?;
    let manifest = toml_edit::Document::parse(&manifest_contents)
        .error("manifest/toml-syntax", "Failed to parse manifest contents")?;

    let Some(package) = manifest.get_table("package") else {
        return Err(Diagnostic::error()
            .with_label(Label::primary(manifest_id(), 0..0))
            .with_message(
                "All `typst.toml` must contain a [package] section. \
                     See the README.md file of this repository for details \
                     about the manifest format.",
            )
            .with_code("manifest/package/missing"));
    };

    let entrypoint = package.get_str("entrypoint").error(
        "manifest/package/entrypoint/missing",
        "Packages must specify an `entrypoint` in their manifest",
    )?;
    let entrypoint = entrypoint.map(|e| PackagePath::from_relative(package_dir, e));
    let name = check_name(diags, package, package_spec);
    let version = check_version(diags, package, package_spec);
    let exclude = check_exclude(diags, package, package_dir)?;

    check_compiler_version(diags, package);
    check_universe_fields(diags, package);
    check_repo(diags, package).await;

    let res = check_file_names(diags, package_dir);
    diags.maybe_emit(res);

    let res = dont_over_exclude(diags, &exclude);
    diags.maybe_emit(res);

    let package = package.map(|_| Package {
        entrypoint,
        name,
        version,
        exclude,
    });

    let template = check_template(diags, &manifest, package_dir);
    if let Some(template) = &template {
        check_thumbnail(diags, &package.exclude, template);
        dont_exclude_template_files(diags, package_dir, &package.exclude, template);
    }

    let world = SystemWorld::new(package.entrypoint.full().to_owned(), package_dir.to_owned())
        .error(
            "compile/package-world-init",
            "Failed to initialize the Typst compiler",
        )?;

    let template_world = world_for_template(package_dir, package_spec, &package, &template);

    let manifest = Manifest { package, template };
    let worlds = Worlds {
        package: world,
        template: template_world,
    };
    Ok((manifest, worlds))
}

fn check_name(
    diags: &mut Diagnostics,
    package: Spanned<&Table>,
    package_spec: Option<&PackageSpec>,
) -> Option<Spanned<String>> {
    let Some(name) = package.get_spanned("name") else {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), 0..0))
                .with_code("manifest/package/name/missing")
                .with_message(
                    "All `typst.toml` must contain a `name` field. \
                    See the README.md file of this repository for details \
                    about the manifest format.",
                ),
        );
        return None;
    };

    let error = Diagnostic::error().with_label(Label::primary(manifest_id(), name.span()));
    let warning = Diagnostic::warning().with_label(Label::primary(manifest_id(), name.span()));

    let Some(name) = name.try_map(Item::as_str) else {
        diags.emit(
            error
                .with_code("manifest/package/name/type")
                .with_message("`name` must be a string."),
        );
        return None;
    };

    if name.val != casbab::kebab(&name) {
        diags.emit(
            error
                .clone()
                .with_code("manifest/package/name/kebab-case")
                .with_message("Please use kebab-case for package names."),
        );
    }

    if name.contains("typst") {
        diags.emit(
            warning
                .with_code("manifest/package/name/typst")
                .with_message("Package names should generally not include \"typst\"."),
        );
    }

    if let Some(package_spec) = package_spec {
        if name.val != package_spec.name {
            diags.emit(
                error
                    .with_code("manifest/package/name/mismatch")
                    .with_message(format!(
                        "Unexpected package name. `{name}` was expected. \
                        If you want to publish a new package, create a new \
                        directory in `packages/{namespace}/`.",
                        name = package_spec.name,
                        namespace = package_spec.namespace,
                    )),
            )
        }
    }

    Some(name.to_owned())
}

fn check_version(
    diags: &mut Diagnostics,
    package: Spanned<&Table>,
    package_spec: Option<&PackageSpec>,
) -> Option<Spanned<PackageVersion>> {
    let Some(version) = package.get_spanned("version") else {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), 0..0))
                .with_code("manifest/package/version/missing")
                .with_message(
                    "All `typst.toml` must contain a `version` field. \
                    See the README.md file of this repository for details \
                    about the manifest format.",
                ),
        );
        return None;
    };

    let error = Diagnostic::error().with_label(Label::primary(manifest_id(), version.span()));

    let Some(version) = version.try_map(Item::as_str) else {
        diags.emit(
            error
                .with_code("manifest/package/version/type")
                .with_message("`version` must be a string."),
        );
        return None;
    };

    let Some(version) = version.try_map(|v| v.parse::<PackageVersion>().ok()) else {
        diags.emit(
            error
                .with_code("manifest/package/version/invalid")
                .with_message(
                    "`version` must be a valid semantic version \
                (i.e follow the `MAJOR.MINOR.PATCH` format).",
                ),
        );
        return None;
    };

    if let Some(package_spec) = package_spec {
        if version.val != package_spec.version {
            diags.emit(
                error
                    .with_code("manifest/package/version/mismatch")
                    .with_message(format!(
                        "Unexpected version number. `{version}` was expected. \
                        If you want to publish a new version, create a new \
                        directory in `packages/{namespace}/{name}`.",
                        version = package_spec.version,
                        name = package_spec.name,
                        namespace = package_spec.namespace,
                    )),
            )
        }
    }

    Some(version)
}

fn check_compiler_version(diags: &mut Diagnostics, package: Spanned<&Table>) -> Option<()> {
    let compiler = package.get_spanned("compiler")?;

    let Some(compiler) = compiler.try_map(Item::as_str) else {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), compiler.span()))
                .with_code("manifest/package/compiler/type")
                .with_message("Compiler version should be a string"),
        );
        return None;
    };

    if PackageVersion::from_str(&compiler).is_err() {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), compiler.span()))
                .with_code("manifest/package/compiler/invalid")
                .with_message("Compiler version should be a valid semantic version, with three components (for example `0.12.0`)"),
        );
        return None;
    }

    Some(())
}

fn dont_over_exclude(diags: &mut Diagnostics, exclude: &Spanned<Override>) -> Result<()> {
    let warning = Diagnostic::warning().with_label(Label::primary(manifest_id(), exclude.span()));

    if exclude.matched("LICENSE", false).is_ignore() {
        diags.emit(
            warning
                .clone()
                .with_code("exclude/license")
                .with_message("Your LICENSE file should not be excluded."),
        );
    }

    if exclude.matched("README.md", false).is_ignore() {
        diags.emit(
            warning
                .with_code("exclude/readme")
                .with_message("Your README.md file should not be excluded."),
        );
    }

    Ok(())
}

fn check_file_names(diags: &mut Diagnostics, package_dir: &Path) -> Result<()> {
    for ch in std::fs::read_dir(package_dir).error("io", "Failed to read package directory")? {
        let mut error_for_file = |path, message| {
            let file_id = FileId::new(None, VirtualPath::new(path));
            diags.emit(
                Diagnostic::error()
                    .with_label(Label::primary(file_id, 0..0))
                    .with_message(message),
            )
        };

        let Ok(ch) = ch else {
            continue;
        };
        let Ok(meta) = ch.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }

        let file_name = ch.file_name();
        let file_path = Path::new(&file_name);
        let stem = file_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned());
        let stem_uppercase = stem.as_ref().map(|s| s.to_uppercase());

        if stem_uppercase.as_deref() == Some("LICENCE") {
            error_for_file(file_path, "This file should be named LICENSE.");
        }

        if (stem_uppercase.as_deref() == Some("LICENSE")
            || stem_uppercase.as_deref() == Some("README"))
            && stem_uppercase != stem
        {
            let fixed = if let Some(ext) = file_path.extension() {
                format!(
                    "{}.{}",
                    stem.unwrap_or_default().to_uppercase(),
                    ext.to_string_lossy()
                )
            } else {
                stem.unwrap_or_default().to_uppercase()
            };
            error_for_file(
                file_path,
                &format!(
                    "To keep consistency, please use \
                        ALL CAPS for the name of this file (i.e. {fixed})"
                ),
            )
        }
    }

    Ok(())
}

/// Some fields are optional for the bundler, but required to be published in Typst Universe.
/// Check that they are present.
fn check_universe_fields(diags: &mut Diagnostics, package: Spanned<&Table>) {
    if let Some(license) = package.get_str("license") {
        if let Ok(spdx_license) = spdx::Expression::parse(&license) {
            for requirement in spdx_license.requirements() {
                if let Some(id) = requirement.req.license.id() {
                    if !id.is_osi_approved() {
                        diags.emit(
                            Diagnostic::error()
                                .with_code("manifest/package/license/osi")
                                .with_message("The `license` field should be OSI approved")
                                .with_label(Label::primary(manifest_id(), license.span())),
                        );
                    }
                } else {
                    diags.emit(
                        Diagnostic::error()
                            .with_code("manifest/package/license/invalid")
                            .with_message("The `license` field should not contain a referencer")
                            .with_label(Label::primary(manifest_id(), license.span())),
                    );
                }
            }
        } else {
            diags.emit(
                Diagnostic::error()
                    .with_code("manifest/package/license/invalid")
                    .with_message("The `license` field should be a valid SPDX-2 expression")
                    .with_label(Label::primary(manifest_id(), license.span())),
            );
        }
    } else {
        diags.emit(
            Diagnostic::error()
                .with_code("manifest/package/license/type")
                .with_message("The `license` field should be a string")
                .with_label(Label::primary(manifest_id(), package.span())),
        );
    }

    if package.get_str("description").is_none() {
        diags.emit(
            Diagnostic::error()
                .with_code("manifest/package/description/type")
                .with_message("The `description` field should be a string")
                .with_label(Label::primary(manifest_id(), package.span())),
        );
    }

    if package
        .get_array("authors")
        .is_none_or(|authors| authors.iter().any(|item| !item.is_str()))
    {
        diags.emit(
            Diagnostic::error()
                .with_code("manifest/package/authors/type")
                .with_message("The `authors` field should be an array of strings")
                .with_label(Label::primary(manifest_id(), package.span())),
        );
        // TODO: check that the format is correct?
    }
}

async fn check_url(
    diags: &mut Diagnostics,
    field: Spanned<&str>,
    name: &'static str,
) -> Option<()> {
    if let Err(e) = reqwest::get(field.val)
        .await
        .and_then(|res| res.error_for_status())
    {
        let kind = if matches!(
            e.status(),
            Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        ) {
            "private"
        } else {
            "unreachable"
        };

        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), field.span()))
                .with_code(format!("manifest/package/{}/{}", name, kind))
                .with_message(format!(
                    "We could not fetch this URL.\n\nDetails: {:#?}",
                    e.without_url()
                )),
        )
    }

    Some(())
}

async fn check_repo(diags: &mut Diagnostics, package: Spanned<&Table>) {
    let repo = package.get_str("repository");
    if let Some(repo) = repo {
        check_url(diags, repo, "repository").await;
    }

    if let Some(homepage) = package.get_str("homepage") {
        check_url(diags, homepage, "homepage").await;

        if repo.is_some_and(|repo| repo.val == homepage.val) {
            diags.emit(
                Diagnostic::error()
                    .with_label(Label::primary(manifest_id(), homepage.span()))
                    .with_code("manifest/package/homepage/redundant")
                    .with_message(
                        "Use the homepage field only if there is a dedicated website. \
                        Otherwise, prefer the `repository` field.",
                    ),
            )
        }
    }
}

/// This function is fallible, because if exclude parsing fails, there might be
/// a lot of false positives in other diagnostics.
fn check_exclude(
    diags: &mut Diagnostics,
    package: Spanned<&Table>,
    package_dir: &Path,
) -> Result<Spanned<Override>> {
    let Some(exclude) = package.get_spanned("exclude") else {
        return Ok(Spanned::new(Override::empty(), package.span()));
    };

    let exclude = exclude.try_map(Item::as_array).error(
        "manifest/package/exclude/type",
        "`exclude` must be an array of strings",
    )?;

    let mut exclude_globs = OverrideBuilder::new(package_dir);
    for exclusion in exclude.iter() {
        let Some(exclusion_str) = exclusion.as_str() else {
            continue;
        };

        if exclusion_str.starts_with('!') {
            warn!("globs with '!' are not supported");
            continue;
        }

        let exclusion_str = exclusion_str
            .strip_prefix("./")
            .inspect(|_| {
                diags.emit(
                    Diagnostic::warning()
                        .with_label(Label::primary(manifest_id(), exclusion.span().unwrap_or_default()))
                        .with_code("manifest/package/exclude/leading-dot")
                        .with_message("Leading `./` of exclusions are trimmed. Use an absolute path starting with `/` to avoid recursive matching."),
                );
            })
            .unwrap_or(exclusion_str);
        exclude_globs.add(&format!("!{exclusion_str}")).ok();
    }

    let exclude_globs = exclude_globs
        .build()
        .error("manifest/package/exclude/invalid", "Invalid exclude globs")?;
    Ok(exclude.map(|_| exclude_globs))
}

fn check_template(
    diags: &mut Diagnostics,
    manifest: &toml_edit::Document<&String>,
    package_dir: &Path,
) -> Option<Spanned<Template>> {
    let template = manifest.get_spanned("template")?;

    let Some(template) = template.try_map(Item::as_table) else {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), template.span()))
                .with_code("manifest/template/type")
                .with_message("`template` must be a table."),
        );
        return None;
    };

    let path = template.get_str("path").map(|path| {
        path.map(PathBuf::from)
            .map(|path| PackagePath::from_relative(package_dir, path))
    });

    let entrypoint = template.get_str("entrypoint").and_then(|entrypoint| {
        let template_dir = path.as_ref()?;
        Some(
            entrypoint
                .map(|entrypoint| path::relative_to(template_dir.full(), entrypoint))
                .map(|path| PackagePath::from_full(package_dir, path)),
        )
    });

    let thumbnail = template.get_str("thumbnail").map(|path| {
        path.map(PathBuf::from)
            .map(|path| PackagePath::from_relative(package_dir, path))
    });

    Some(template.map(|_| Template {
        path,
        entrypoint,
        thumbnail,
    }))
}

fn world_for_template(
    package_dir: &Path,
    package_spec: Option<&PackageSpec>,
    package: &Spanned<Package>,
    template: &Option<Spanned<Template>>,
) -> Option<SystemWorld> {
    let name = package.name.as_ref()?;
    let version = package.version.as_ref()?;
    let inferred_package_spec = PackageSpec {
        namespace: "preview".into(),
        name: name.as_ref().val.into(),
        version: version.val,
    };
    let package_spec = package_spec.unwrap_or(&inferred_package_spec);

    let template = template.as_ref()?;
    let template_path = template.path.as_ref()?;
    let template_main = template.entrypoint.as_ref()?;

    let mut world = SystemWorld::new(
        template_main.full().to_owned(),
        template_path.full().to_owned(),
    )
    .ok()?
    .with_package_override(package_spec, package_dir);
    world.exclude(package.exclude.val.clone());
    Some(world)
}

fn dont_exclude_template_files(
    diags: &mut Diagnostics,
    package_dir: &Path,
    exclude: &Override,
    template: &Spanned<Template>,
) {
    let Some(template_path) = &template.path else {
        return;
    };

    for entry in ignore::Walk::new(template_path.full()).flatten() {
        let entry_path = PackagePath::from_full(package_dir, entry.path());

        // For build artifacts, ask the package author to delete them.
        let ext = entry.path().extension().and_then(|e| e.to_str());
        if matches!(ext, Some("pdf" | "png" | "svg")) && entry.path().with_extension("typ").exists()
        {
            diags.emit(
                Diagnostic::error()
                    .with_label(Label::primary(entry_path.file_id(), 0..0))
                    .with_code("files/compilation-artifact")
                    .with_message(
                        "This file is a compiled document and should \
                        not be included in the template. \
                        Please delete it.",
                    ),
            );
            continue;
        }

        // For other files, check that they are indeed not excluded.
        if exclude
            .matched(entry.path(), entry.metadata().is_ok_and(|m| m.is_dir()))
            .is_ignore()
        {
            diags.emit(
                Diagnostic::error()
                    .with_code("exclude/template")
                    .with_message("This file is part of the template and should not be excluded.")
                    .with_label(Label::primary(entry_path.file_id(), 0..0)),
            )
        }
    }
}

fn check_thumbnail(diags: &mut Diagnostics, exclude: &Override, template: &Spanned<Template>) {
    let Some(thumbnail_path) = &template.thumbnail else {
        return;
    };

    if !thumbnail_path.full().exists() {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), thumbnail_path.span()))
                .with_code("manifest/template/thumbnail/not-found")
                .with_message("This file does not exist."),
        )
    }

    if !matches!(
        thumbnail_path.extension().and_then(|e| e.to_str()),
        Some("png" | "webp")
    ) {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), thumbnail_path.span()))
                .with_code("manifest/template/thumbnail/format")
                .with_message("Thumbnails should be PNG or WebP files."),
        )
    }

    if exclude.matched(thumbnail_path.full(), false).is_ignore() {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_id(), thumbnail_path.span()))
                .with_code("manifest/template/thumbnail/exclude")
                .with_message("The template thumbnail is automatically excluded"),
        );
    }

    if let Some(template_path) = &template.path {
        if thumbnail_path.full().starts_with(template_path.full()) {
            diags.emit(
                Diagnostic::error()
                    .with_label(Label::primary(manifest_id(), thumbnail_path.span()))
                    .with_code("manifest/template/thumbnail/location")
                    .with_message(
                        "The thumbnail file should be outside of the template directory.\n\n\
                        When your template will be used as a base for users's projects, \
                        the template directory will be copied as is, and the thumbnail file
                        is generally not displayed in documents based on your template.",
                    ),
            );
        }
    }
}

fn manifest_id() -> FileId {
    FileId::new(None, VirtualPath::new("typst.toml"))
}

#[derive(Debug, Copy, Clone)]
pub struct Spanned<T> {
    pub val: T,
    /// Don't use [`std::ops::Range`] directly, so we can derive Copy.
    span: (usize, usize),
}

impl<T> Spanned<T> {
    fn new(val: T, span: Range<usize>) -> Self {
        Self {
            val,
            span: (span.start, span.end),
        }
    }

    pub fn span(&self) -> Range<usize> {
        self.span.0..self.span.1
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

    pub fn try_map<F, V>(self, f: F) -> Option<Spanned<V>>
    where
        F: FnOnce(T) -> Option<V>,
    {
        Some(Spanned {
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

    fn get_table(&self, name: &'static str) -> Option<Spanned<&Table>>;

    fn get_array(&self, name: &'static str) -> Option<Spanned<&Array>>;

    fn get_str(&self, name: &'static str) -> Option<Spanned<&str>>;
}

impl TomlExt for Table {
    fn get_spanned(&self, name: &'static str) -> Option<Spanned<&Item>> {
        let item = self.get(name)?;
        // The span should only ever be none for user-created tables, not
        // deserialized ones.
        let span = item.span()?;
        Some(Spanned::new(item, span))
    }

    fn get_table(&self, name: &'static str) -> Option<Spanned<&Table>> {
        self.get_spanned(name)?.try_map(Item::as_table)
    }

    fn get_array(&self, name: &'static str) -> Option<Spanned<&Array>> {
        self.get_spanned(name)?.try_map(Item::as_array)
    }

    fn get_str(&self, name: &'static str) -> Option<Spanned<&str>> {
        self.get_spanned(name)?.try_map(Item::as_str)
    }
}
