use std::{ops::Range, path::Path, str::FromStr};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use ignore::overrides::{Override, OverrideBuilder};
use reqwest::StatusCode;
use toml_edit::Item;
use tracing::{debug, warn};
use typst::syntax::{
    package::{PackageSpec, PackageVersion},
    FileId, VirtualPath,
};

use crate::check::path::PackagePath;
use crate::{
    check::{files, Diagnostics, Result, TryExt},
    world::SystemWorld,
};

pub struct Worlds {
    pub package: SystemWorld,
    pub template: Option<SystemWorld>,
}

pub async fn check(
    package_dir: &Path,
    diags: &mut Diagnostics,
    package_spec: Option<&PackageSpec>,
) -> Result<Worlds> {
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

    let entrypoint = package_dir.join(
        manifest
            .get("package")
            .and_then(|package| package.get("entrypoint"))
            .and_then(|entrypoint| entrypoint.as_str())
            .error(
                "manifest/package/entrypoint/missing",
                "Packages must specify an `entrypoint` in their manifest",
            )?,
    );
    let world = SystemWorld::new(entrypoint, package_dir.to_owned()).error(
        "compile/package-world-init",
        "Failed to initialize the Typst compiler",
    )?;

    let manifest_file_id = FileId::new(None, VirtualPath::new("typst.toml"));

    if !manifest.contains_table("package") {
        // TODO: this condition is probably unreachable as the program would
        // have panicked before if the `package` table is missing.
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)])
                .with_message(
                    "All `typst.toml` must contain a [package] section. \
                    See the README.md file of this repository for details \
                    about the manifest format.",
                )
                .with_code("manifest/package/missing"),
        );
        return Ok(Worlds {
            package: world,
            template: None,
        });
    }

    let name = check_name(diags, manifest_file_id, &manifest, package_spec);
    let version = check_version(diags, manifest_file_id, &manifest, package_spec);

    check_compiler_version(diags, manifest_file_id, &manifest);

    let res = check_universe_fields(diags, manifest_file_id, &manifest);
    diags.maybe_emit(res);

    let res = check_file_names(diags, package_dir);
    diags.maybe_emit(res);

    let (exclude, exclude_span) = read_exclude(diags, manifest_file_id, &manifest, package_dir)?;

    let res = dont_over_exclude(diags, manifest_file_id, &exclude, exclude_span.clone());
    diags.maybe_emit(res);

    check_repo(diags, manifest_file_id, &manifest).await;

    let template_world = if let (Some(name), Some(version)) = (name, version) {
        let inferred_package_spec = PackageSpec {
            namespace: "preview".into(),
            name: name.into(),
            version,
        };

        world_for_template(
            &manifest,
            package_dir,
            package_spec.unwrap_or(&inferred_package_spec),
            exclude.clone(),
        )
    } else {
        None
    };

    let template_dir = template_root(package_dir, &manifest);

    if let Some(template_dir) = &template_dir {
        dont_exclude_template_files(diags, package_dir, &exclude, template_dir.as_path());
    }
    let thumbnail_path = check_thumbnail(diags, &manifest, manifest_file_id, package_dir, &exclude);

    files::check_files(diags, package_dir, &exclude, thumbnail_path);

    Ok(Worlds {
        package: world,
        template: template_world,
    })
}

fn check_name(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::Document<&String>,
    package_spec: Option<&PackageSpec>,
) -> Option<String> {
    let Some(name) = manifest
        .get("package")
        .and_then(|package| package.get("name"))
    else {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)])
                .with_code("manifest/package/name/missing")
                .with_message(
                    "All `typst.toml` must contain a `name` field. \
                    See the README.md file of this repository for details \
                    about the manifest format.",
                ),
        );
        return None;
    };

    let error = Diagnostic::error().with_labels(vec![Label::primary(
        manifest_file_id,
        name.span().unwrap_or_default(),
    )]);
    let warning = Diagnostic::warning().with_labels(vec![Label::primary(
        manifest_file_id,
        name.span().unwrap_or_default(),
    )]);

    let Some(name) = name.as_str() else {
        diags.emit(
            error
                .with_code("manifest/package/name/type")
                .with_message("`name` must be a string."),
        );
        return None;
    };

    if name != casbab::kebab(name) {
        diags.emit(
            error
                .clone()
                .with_code("manifest/package/name/kebab-case")
                .with_message("Please use kebab-case for package names."),
        )
    }

    if name.contains("typst") {
        diags.emit(
            warning
                .with_code("manifest/package/name/typst")
                .with_message("Package names should generally not include \"typst\"."),
        );
    }

    if let Some(package_spec) = package_spec {
        if name != package_spec.name {
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
    manifest_file_id: FileId,
    manifest: &toml_edit::Document<&String>,
    package_spec: Option<&PackageSpec>,
) -> Option<PackageVersion> {
    let Some(version) = manifest
        .get("package")
        .and_then(|package| package.get("version"))
    else {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)])
                .with_code("manifest/package/version/missing")
                .with_message(
                    "All `typst.toml` must contain a `version` field. \
                    See the README.md file of this repository for details \
                    about the manifest format.",
                ),
        );
        return None;
    };

    let error = Diagnostic::error().with_labels(vec![Label::primary(
        manifest_file_id,
        version.span().unwrap_or_default(),
    )]);

    let Some(version) = version.as_str() else {
        diags.emit(
            error
                .with_code("manifest/package/version/type")
                .with_message("`version` must be a string."),
        );
        return None;
    };

    let Ok(version) = version.parse::<PackageVersion>() else {
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
        if version != package_spec.version {
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

fn check_compiler_version(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::Document<&String>,
) -> Option<()> {
    let compiler = manifest.get("package")?.get("compiler")?;
    let Some(compiler_str) = compiler.as_str() else {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, compiler.span()?)])
                .with_code("manifest/package/compiler/type")
                .with_message("Compiler version should be a string"),
        );
        return None;
    };

    if PackageVersion::from_str(compiler_str).is_err() {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, compiler.span()?)])
                .with_code("manifest/package/compiler/invalid")
                .with_message("Compiler version should be a valid semantic version, with three components (for example `0.12.0`)"),
        );
        return None;
    }

    Some(())
}

fn dont_over_exclude(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    exclude: &Override,
    span: std::ops::Range<usize>,
) -> Result<()> {
    let warning = Diagnostic::warning().with_labels(vec![Label::primary(manifest_file_id, span)]);

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
                    .with_labels(vec![Label::primary(file_id, 0..0)])
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
fn check_universe_fields(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::Document<&String>,
) -> Result<()> {
    let pkg = manifest
        .get("package")
        .error("manifest/package/missing", "[package] not found")?
        .as_table()
        .error("manifest/package/invalid-type", "[package] is not a table")?;

    if let Some((license, span)) = pkg
        .get("license")
        .and_then(|l| l.as_str().map(|s| (s, l.span().unwrap_or_default())))
    {
        if let Ok(license) = spdx::Expression::parse(license) {
            for requirement in license.requirements() {
                if let Some(id) = requirement.req.license.id() {
                    if !id.is_osi_approved() {
                        diags.emit(
                            Diagnostic::error()
                                .with_code("manifest/package/license/osi")
                                .with_message("The `license` field should be OSI approved")
                                .with_labels(vec![Label::primary(manifest_file_id, span.clone())]),
                        );
                    }
                } else {
                    diags.emit(
                        Diagnostic::error()
                            .with_code("manifest/package/license/invalid")
                            .with_message("The `license` field should not contain a referencer")
                            .with_labels(vec![Label::primary(manifest_file_id, span.clone())]),
                    );
                }
            }
        } else {
            diags.emit(
                Diagnostic::error()
                    .with_code("manifest/package/license/invalid")
                    .with_message("The `license` field should be a valid SPDX-2 expression")
                    .with_labels(vec![Label::primary(manifest_file_id, span.clone())]),
            );
        }
    } else {
        diags.emit(
            Diagnostic::error()
                .with_code("manifest/package/license/type")
                .with_message("The `license` field should be a string")
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)]),
        );
    }

    if pkg.get("description").map(|d| !d.is_str()).unwrap_or(true) {
        diags.emit(
            Diagnostic::error()
                .with_code("manifest/package/description/type")
                .with_message("The `description` field should be a string")
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)]),
        );
    }

    if pkg
        .get("authors")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().any(|item| !item.is_str()))
        .unwrap_or(true)
    {
        diags.emit(
            Diagnostic::error()
                .with_code("manifest/package/authors/type")
                .with_message("The `authors` field should be an array of strings")
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)]),
        );
        // TODO: check that the format is correct?
    }

    Ok(())
}

async fn check_url(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    field: &Item,
    field_name: &'static str,
) -> Option<()> {
    if let Err(e) = reqwest::get(field.as_str()?)
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
                .with_labels(vec![Label::primary(
                    manifest_file_id,
                    field.span().unwrap_or_default(),
                )])
                .with_code(format!("manifest/package/{}/{}", field_name, kind))
                .with_message(format!(
                    "We could not fetch this URL.\n\nDetails: {:#?}",
                    e.without_url()
                )),
        )
    }

    Some(())
}

async fn check_repo(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::Document<&String>,
) -> Option<()> {
    let repo_field = manifest.get("package")?.get("repository")?;
    check_url(diags, manifest_file_id, repo_field, "repository").await;

    let homepage_field = manifest.get("package")?.get("homepage")?;
    check_url(diags, manifest_file_id, homepage_field, "homepage").await;

    if repo_field.as_str() == homepage_field.as_str() {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(
                    manifest_file_id,
                    homepage_field.span().unwrap_or_default(),
                )])
                .with_code("manifest/package/homepage/redundant")
                .with_message("Use the homepage field only if there is a dedicated website. Otherwise, prefer the `repository` field.".to_owned()),
        )
    }

    Some(())
}

fn read_exclude(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::Document<&String>,
    package_dir: &Path,
) -> Result<(Override, Range<usize>)> {
    let empty_array = toml_edit::Array::new();
    let exclude = manifest
        .get("package")
        .and_then(|package| package.get("exclude"))
        .and_then(|item| item.as_array())
        .unwrap_or(&empty_array);

    let mut exclude_globs = OverrideBuilder::new(package_dir);
    for exclusion in exclude {
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
                        .with_label(Label::primary(manifest_file_id, exclusion.span().unwrap_or_default()))
                        .with_code("manifest/package/exclude/leading-dot")
                        .with_message("Leading `./` of exclusions are trimmed. Use an absolute path starting with `/` to avoid recursive matching."),
                );
            })
            .unwrap_or(exclusion_str);
        exclude_globs.add(&format!("!{exclusion_str}")).ok();
    }
    Ok((
        exclude_globs
            .build()
            .error("manifest/exclude/invalid", "Invalid exclude globs")?,
        exclude.span().unwrap_or(0..0),
    ))
}

fn world_for_template(
    manifest: &toml_edit::Document<&String>,
    package_dir: &Path,
    package_spec: &PackageSpec,
    exclude: Override,
) -> Option<SystemWorld> {
    let template = manifest.get("template")?.as_table()?;
    let template_path = package_dir.join(template.get("path")?.as_str()?);
    let template_main = template_path.join(template.get("entrypoint")?.as_str()?);

    let mut world = SystemWorld::new(template_main, template_path)
        .ok()?
        .with_package_override(package_spec, package_dir);
    world.exclude(exclude);
    Some(world)
}

fn dont_exclude_template_files(
    diags: &mut Diagnostics,
    package_dir: &Path,
    exclude: &Override,
    template_dir: PackagePath<&Path>,
) {
    for entry in ignore::Walk::new(template_dir.full()).flatten() {
        let entry_path = PackagePath::from_full(package_dir, entry.path());

        // For build artifacts, ask the package author to delete them.
        let ext = entry.path().extension().and_then(|e| e.to_str());
        if matches!(ext, Some("pdf" | "png" | "svg")) && entry.path().with_extension("typ").exists()
        {
            diags.emit(
                Diagnostic::error()
                    .with_labels(vec![Label::primary(
                        FileId::new(None, VirtualPath::new(entry_path.relative())),
                        0..0,
                    )])
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
                    .with_labels(vec![Label::primary(
                        FileId::new(None, VirtualPath::new(entry_path.relative())),
                        0..0,
                    )]),
            )
        }
    }
}

fn template_root(
    package_dir: &Path,
    manifest: &toml_edit::Document<&String>,
) -> Option<PackagePath> {
    let root = manifest
        .get("template")
        .and_then(|t| t.get("path"))?
        .as_str()?;
    Some(PackagePath::from_relative(package_dir, root))
}

fn check_thumbnail(
    diags: &mut Diagnostics,
    manifest: &toml_edit::Document<&String>,
    manifest_file_id: FileId,
    package_dir: &Path,
    exclude: &Override,
) -> Option<PackagePath> {
    let thumbnail = manifest.get("template")?.as_table()?.get("thumbnail")?;
    let thumbnail_path = PackagePath::from_relative(package_dir, Path::new(thumbnail.as_str()?));

    if !thumbnail_path.full().exists() {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, thumbnail.span()?)])
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
                .with_labels(vec![Label::primary(manifest_file_id, thumbnail.span()?)])
                .with_code("manifest/template/thumbnail/format")
                .with_message("Thumbnails should be PNG or WebP files."),
        )
    }

    if exclude.matched(thumbnail_path.full(), false).is_ignore() {
        diags.emit(
            Diagnostic::error()
                .with_label(Label::primary(manifest_file_id, thumbnail.span()?))
                .with_code("manifest/template/thumbnail/exclude")
                .with_message("The template thumbnail is automatically excluded"),
        );
    }

    if let Some(root) = template_root(package_dir, manifest) {
        if thumbnail_path.full().starts_with(root.full()) {
            diags.emit(
                Diagnostic::error()
                    .with_label(Label::primary(manifest_file_id, thumbnail.span()?))
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

    Some(thumbnail_path)
}
