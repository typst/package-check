use std::{
    ops::Range,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    str::FromStr,
};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use eyre::{Context, ContextCompat};
use ignore::overrides::{Override, OverrideBuilder};
use toml_edit::Item;
use tracing::{debug, warn};
use typst::syntax::{
    package::{PackageSpec, PackageVersion},
    FileId, VirtualPath,
};

use crate::{
    check::{file_size, Diagnostics},
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
) -> eyre::Result<Worlds> {
    let manifest_path = package_dir.join("typst.toml");
    debug!("Reading manifest at {}", &manifest_path.display());
    let manifest_contents =
        std::fs::read_to_string(manifest_path).context("Failed to read manifest contents.")?;
    let manifest = toml_edit::ImDocument::parse(&manifest_contents)
        .context("Failed to parse manifest contents")?;

    let entrypoint = package_dir.join(
        manifest
            .get("package")
            .and_then(|package| package.get("entrypoint"))
            .and_then(|entrypoint| entrypoint.as_str())
            .context("Packages must specify an `entrypoint` in their manifest")?,
    );
    let world = SystemWorld::new(entrypoint, package_dir.to_owned())
        .map_err(|e| eyre::Report::msg(e).wrap_err("Failed to initialize the Typst compiler"))?;

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
                ),
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

    let res = dont_over_exclude(diags, package_dir, manifest_file_id, &manifest);
    diags.maybe_emit(res);

    check_repo(diags, manifest_file_id, &manifest).await;

    let (exclude, _) = read_exclude(package_dir, &manifest)?;

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

    dont_exclude_template_files(diags, &manifest, package_dir, exclude);
    let thumbnail_path = check_thumbnail(diags, &manifest, manifest_file_id, package_dir);

    let res = exclude_large_files(diags, package_dir, &manifest, thumbnail_path);
    diags.maybe_emit(res);

    Ok(Worlds {
        package: world,
        template: template_world,
    })
}

fn check_name(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::ImDocument<&String>,
    package_spec: Option<&PackageSpec>,
) -> Option<String> {
    let Some(name) = manifest
        .get("package")
        .and_then(|package| package.get("name"))
    else {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)])
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
        diags.emit(error.with_message("`name` must be a string."));
        return None;
    };

    if name != casbab::kebab(name) {
        diags.emit(
            error
                .clone()
                .with_message("Please use kebab-case for package names."),
        )
    }

    if name.contains("typst") {
        diags.emit(warning.with_message("Package names should generally not include \"typst\"."));
    }

    if let Some(package_spec) = package_spec {
        if name != package_spec.name {
            diags.emit(error.with_message(format!(
                "Unexpected package name. `{name}` was expected. \
                        If you want to publish a new package, create a new \
                        directory in `packages/{namespace}/`.",
                name = package_spec.name,
                namespace = package_spec.namespace,
            )))
        }
    }

    Some(name.to_owned())
}

fn check_version(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::ImDocument<&String>,
    package_spec: Option<&PackageSpec>,
) -> Option<PackageVersion> {
    let Some(version) = manifest
        .get("package")
        .and_then(|package| package.get("version"))
    else {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)])
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
        diags.emit(error.with_message("`version` must be a string."));
        return None;
    };

    let Ok(version) = version.parse::<PackageVersion>() else {
        diags.emit(error.with_message(
            "`version` must be a valid semantic version \
                (i.e follow the `MAJOR.MINOR.PATCH` format).",
        ));
        return None;
    };

    if let Some(package_spec) = package_spec {
        if version != package_spec.version {
            diags.emit(error.with_message(format!(
                "Unexpected version number. `{version}` was expected. \
                        If you want to publish a new version, create a new \
                        directory in `packages/{namespace}/{name}`.",
                version = package_spec.version,
                name = package_spec.name,
                namespace = package_spec.namespace,
            )))
        }
    }

    Some(version)
}

fn check_compiler_version(
    diags: &mut Diagnostics,
    manifest_file_id: FileId,
    manifest: &toml_edit::ImDocument<&String>,
) -> Option<()> {
    let compiler = manifest.get("package")?.get("compiler")?;
    let Some(compiler_str) = compiler.as_str() else {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, compiler.span()?)])
                .with_message("Compiler version should be a string"),
        );
        return None;
    };

    if PackageVersion::from_str(compiler_str).is_err() {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, compiler.span()?)])
                .with_message("Compiler version should be a valid semantic version, with three components (for example `0.12.0`)"),
        );
        return None;
    }

    Some(())
}

fn exclude_large_files(
    diags: &mut Diagnostics,
    package_dir: &Path,
    manifest: &toml_edit::ImDocument<&String>,
    thumbnail_path: Option<PathBuf>,
) -> eyre::Result<()> {
    let template_root = template_root(manifest);
    let template_dir = template_root.and_then(|root| package_dir.join(&root).canonicalize().ok());
    let (exclude, _) = read_exclude(package_dir, manifest)?;

    const REALLY_LARGE: u64 = 50 * 1024 * 1024;

    let large_files = file_size::find_large_files(package_dir, exclude.clone());
    for (path, size) in large_files? {
        if Some(path.as_ref())
            == thumbnail_path
                .as_ref()
                .and_then(|t| t.strip_prefix(package_dir).ok())
        {
            // Thumbnail is always excluded
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) == Some("wasm") {
            let path = package_dir.join(&path);
            if let Some(file_name) = path.file_name() {
                let out = std::env::temp_dir().join(file_name);

                let wasm_opt_result = wasm_opt::OptimizationOptions::new_optimize_for_size()
                    // Explicitely enable and disable features to best match what wasmi supports
                    // https://github.com/wasmi-labs/wasmi?tab=readme-ov-file#webassembly-proposals
                    .enable_feature(wasm_opt::Feature::MutableGlobals)
                    .enable_feature(wasm_opt::Feature::TruncSat)
                    .enable_feature(wasm_opt::Feature::SignExt)
                    .enable_feature(wasm_opt::Feature::Multivalue)
                    .enable_feature(wasm_opt::Feature::BulkMemory)
                    .enable_feature(wasm_opt::Feature::ReferenceTypes)
                    .enable_feature(wasm_opt::Feature::TailCall)
                    .enable_feature(wasm_opt::Feature::ExtendedConst)
                    .enable_feature(wasm_opt::Feature::MultiMemory)
                    .disable_feature(wasm_opt::Feature::Simd)
                    .disable_feature(wasm_opt::Feature::RelaxedSimd)
                    .disable_feature(wasm_opt::Feature::Gc)
                    .disable_feature(wasm_opt::Feature::ExceptionHandling)
                    .run(&path, &out);

                if wasm_opt_result.is_ok() {
                    let original_size = std::fs::metadata(&path).map(|m| m.size());
                    let new_size = std::fs::metadata(&out).map(|m| m.size());

                    match (new_size, original_size) {
                        (Ok(new_size), Ok(original_size)) if new_size < original_size => {
                            let diff = (original_size - new_size) / 1024;

                            if diff > 20 {
                                diags.emit(
                                    Diagnostic::warning()
                                        .with_labels(vec![Label::primary(
                                            FileId::new(
                                                None,
                                                VirtualPath::new(path.strip_prefix(package_dir)?),
                                            ),
                                            0..0,
                                        )])
                                        .with_message(format!(
                                        "This file could be {diff}kB smaller with `wasm-opt -Os`."
                                    )),
                                );
                            }
                        }
                        _ => {}
                    }

                    // TODO: ideally this should be async
                    std::fs::remove_file(out).ok();
                }
            }

            // Don't suggest to exclude WASM files, they are generally necessary
            // for the package to work.
            continue;
        }

        let fid = FileId::new(None, VirtualPath::new(&path));

        let message = if size > REALLY_LARGE {
            format!(
                "This file is really large ({size}MB). \
                If possible, do not include it in this repository at all.",
                size = size / 1024 / 1024
            )
        } else if !exclude.matched(path, false).is_ignore() {
            format!(
                "This file is quite large ({size}MB). \
                If it is not required to use the package \
                (i.e. it is a documentation file, or part of an example), \
                it should be added to `exclude` in your `typst.toml`.",
                size = size / 1024 / 1024
            )
        } else {
            continue;
        };

        diags.emit(
            Diagnostic::warning()
                .with_labels(vec![Label::primary(fid, 0..0)])
                .with_message(message),
        )
    }

    // Also exclude examples
    for ch in ignore::WalkBuilder::new(package_dir)
        .overrides(exclude)
        .build()
    {
        let Ok(ch) = ch else {
            continue;
        };

        let Ok(metadata) = ch.metadata() else {
            continue;
        };

        if metadata.is_dir() {
            continue;
        }

        if template_dir
            .as_ref()
            .is_some_and(|template_dir| ch.path().starts_with(template_dir))
        {
            // Don't exclude template files, even if they contain "example" or "test" in their name.
            continue;
        }

        let relative_path = ch
            .path()
            .strip_prefix(package_dir)
            .context("Child path is not part of parent path")?;

        let file_name = ch.file_name();
        let file_name_str = file_name.to_string_lossy();
        let file_id = FileId::new(None, VirtualPath::new(relative_path));
        let warning = Diagnostic::warning().with_labels(vec![Label::primary(file_id, 0..0)]);
        if file_name_str.contains("example") {
            diags.emit(warning.clone().with_message(
                "This file seems to be an example, \
                    and should probably be added to `exclude` in your `typst.toml`.",
            ));
            continue;
        }

        if file_name_str.contains("test") {
            diags.emit(warning.clone().with_message(
                "This file seems to be a test, \
                    and should probably be added to `exclude` in your `typst.toml`.",
            ));
            continue;
        }
    }

    Ok(())
}

fn dont_over_exclude(
    diags: &mut Diagnostics,
    package_dir: &Path,
    manifest_file_id: FileId,
    manifest: &toml_edit::ImDocument<&String>,
) -> eyre::Result<()> {
    let (exclude, span) = read_exclude(package_dir, manifest)?;

    let warning = Diagnostic::warning().with_labels(vec![Label::primary(manifest_file_id, span)]);

    if exclude.matched("LICENSE", false).is_ignore() {
        diags.emit(
            warning
                .clone()
                .with_message("Your LICENSE file should not be excluded."),
        );
    }

    if exclude.matched("README.md", false).is_ignore() {
        diags.emit(warning.with_message("Your README.md file should not be excluded."));
    }

    Ok(())
}

fn check_file_names(diags: &mut Diagnostics, package_dir: &Path) -> eyre::Result<()> {
    for ch in std::fs::read_dir(package_dir).context("Failed to read package directory")? {
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
    manifest: &toml_edit::ImDocument<&String>,
) -> eyre::Result<()> {
    let pkg = manifest
        .get("package")
        .context("[package] not found")?
        .as_table()
        .context("[package] is not a table")?;

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
                                .with_message("The `license` field should be OSI approved")
                                .with_labels(vec![Label::primary(manifest_file_id, span.clone())]),
                        );
                    }
                } else {
                    diags.emit(
                        Diagnostic::error()
                            .with_message("The `license` field should not contain a referencer")
                            .with_labels(vec![Label::primary(manifest_file_id, span.clone())]),
                    );
                }
            }
        } else {
            diags.emit(
                Diagnostic::error()
                    .with_message("The `license` field should be a valid SPDX-2 expression")
                    .with_labels(vec![Label::primary(manifest_file_id, span.clone())]),
            );
        }
    } else {
        diags.emit(
            Diagnostic::error()
                .with_message("The `license` field should be a string")
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)]),
        );
    }

    if pkg.get("description").map(|d| !d.is_str()).unwrap_or(true) {
        diags.emit(
            Diagnostic::error()
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
                .with_message("The `authors` field should be an array of strings")
                .with_labels(vec![Label::primary(manifest_file_id, 0..0)]),
        );
        // TODO: check that the format is correct?
    }

    Ok(())
}

async fn check_url(diags: &mut Diagnostics, manifest_file_id: FileId, field: &Item) -> Option<()> {
    if let Err(e) = reqwest::get(field.as_str()?)
        .await
        .and_then(|res| res.error_for_status())
    {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(
                    manifest_file_id,
                    field.span().unwrap_or_default(),
                )])
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
    manifest: &toml_edit::ImDocument<&String>,
) -> Option<()> {
    let repo_field = manifest.get("package")?.get("repository")?;
    check_url(diags, manifest_file_id, repo_field).await;

    let homepage_field = manifest.get("package")?.get("homepage")?;
    check_url(diags, manifest_file_id, homepage_field).await;

    if repo_field.as_str() == homepage_field.as_str() {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(
                    manifest_file_id,
                    homepage_field.span().unwrap_or_default(),
                )])
                .with_message("Use the homepage field only if there is a dedicated website. Otherwise, prefer the `repository` field.".to_owned()),
        )
    }

    Some(())
}

fn read_exclude(
    package_dir: &Path,
    manifest: &toml_edit::ImDocument<&String>,
) -> eyre::Result<(Override, Range<usize>)> {
    let empty_array = toml_edit::Array::new();
    let exclude = manifest
        .get("package")
        .and_then(|package| package.get("exclude"))
        .and_then(|item| item.as_array())
        .unwrap_or(&empty_array);

    let mut exclude_globs = OverrideBuilder::new(
        package_dir
            .canonicalize()
            .context("Failed to canonicalize package directory")?,
    );
    for exclusion in exclude {
        let Some(exclusion) = exclusion.as_str() else {
            continue;
        };

        if exclusion.starts_with('!') {
            warn!("globs with '!' are not supported");
            continue;
        }

        let exclusion = exclusion.trim_start_matches("./");
        exclude_globs.add(&format!("!{exclusion}")).ok();
    }
    Ok((
        exclude_globs.build().context("Invalid exclude globs")?,
        exclude.span().unwrap_or(0..0),
    ))
}

fn world_for_template(
    manifest: &toml_edit::ImDocument<&String>,
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
    manifest: &toml_edit::ImDocument<&String>,
    package_dir: &Path,
    exclude: Override,
) -> Option<()> {
    let template_root = template_root(manifest)?;
    for entry in ignore::Walk::new(package_dir.join(template_root)).flatten() {
        // For build artifacts, ask the package author to delete them.
        let ext = entry.path().extension().and_then(|e| e.to_str());
        if matches!(ext, Some("pdf" | "png" | "svg")) && entry.path().with_extension("typ").exists()
        {
            diags.emit(
                Diagnostic::error()
                    .with_labels(vec![Label::primary(
                        FileId::new(
                            None,
                            VirtualPath::new(entry.path().strip_prefix(package_dir).ok()?),
                        ),
                        0..0,
                    )])
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
            .matched(
                entry.path().canonicalize().ok()?,
                entry.metadata().ok()?.is_dir(),
            )
            .is_ignore()
        {
            diags.emit(
                Diagnostic::error()
                    .with_message("This file is part of the template and should not be excluded.")
                    .with_labels(vec![Label::primary(
                        FileId::new(
                            None,
                            VirtualPath::new(entry.path().strip_prefix(package_dir).ok()?),
                        ),
                        0..0,
                    )]),
            )
        }
    }

    Some(())
}

fn template_root(manifest: &toml_edit::ImDocument<&String>) -> Option<PathBuf> {
    Some(PathBuf::from(
        manifest
            .get("template")
            .and_then(|t| t.get("path"))?
            .as_str()?,
    ))
}

fn check_thumbnail(
    diags: &mut Diagnostics,
    manifest: &toml_edit::ImDocument<&String>,
    manifest_file_id: FileId,
    package_dir: &Path,
) -> Option<PathBuf> {
    let thumbnail = manifest.get("template")?.as_table()?.get("thumbnail")?;
    let thumbnail_path = package_dir.join(thumbnail.as_str()?);

    if !thumbnail_path.exists() {
        diags.emit(
            Diagnostic::error()
                .with_labels(vec![Label::primary(manifest_file_id, thumbnail.span()?)])
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
                .with_message("Thumbnails should be PNG or WebP files."),
        )
    }

    Some(thumbnail_path)
}
