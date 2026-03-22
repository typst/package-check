use std::collections::HashSet;
use std::path::Path;

use codespan_reporting::diagnostic::{Diagnostic, Label};

use crate::check::manifest::Manifest;
use crate::check::path::PackagePath;
use crate::check::Diagnostics;

pub fn check(diags: &mut Diagnostics, package_dir: &Path, manifest: &Manifest) {
    let exclude = &manifest.package.exclude;
    let thumbnail_path = manifest.thumbnail();

    // Manually keep track of excluded directories, to figure out if nested
    // files are ignored. This is done, so we can generate diagnostics for
    // excluded files.
    let mut excluded_dirs = HashSet::new();

    for ch in ignore::WalkBuilder::new(package_dir).hidden(false).build() {
        let Ok(ch) = ch else { continue };
        let Ok(metadata) = ch.metadata() else {
            continue;
        };

        let file_path = PackagePath::from_full(package_dir, ch.path());

        if metadata.is_dir() {
            // If the parent directory is ignored, all children are ignored too.
            if parent_is_excluded(&excluded_dirs, file_path)
                || exclude.matched(file_path.relative(), true).is_ignore()
            {
                excluded_dirs.insert(ch.into_path());
            }
            continue;
        }

        // The thumbnail is always excluded.
        let is_thumbnail = thumbnail_path.is_some_and(|t| t.val == file_path);
        let excluded = is_thumbnail
            || parent_is_excluded(&excluded_dirs, file_path)
            || exclude.matched(file_path.relative(), false).is_ignore();

        forbid_font_files(diags, file_path);
        exclude_large_files(diags, file_path, excluded, metadata.len());
        exclude_examples_and_tests(diags, file_path, excluded);
    }
}

fn parent_is_excluded(
    excluded_dirs: &HashSet<std::path::PathBuf>,
    file_path: PackagePath<&Path>,
) -> bool {
    file_path
        .full()
        .parent()
        .is_some_and(|parent| excluded_dirs.contains(parent))
}

fn exclude_large_files(
    diags: &mut Diagnostics,
    path: PackagePath<&Path>,
    excluded: bool,
    size: u64,
) {
    /// Size (in bytes) after which a file is considered large.
    const LARGE: u64 = 1024 * 1024; // 1 MB
    const REALLY_LARGE: u64 = 50 * 1024 * 1024; // 50 MB

    if size < LARGE {
        return;
    }

    if path.extension().is_some_and(|ext| ext == "wasm") {
        check_wasm_file_size(diags, path, size);
        // Don't suggest to exclude WASM files, they are generally necessary
        // for the package to work.
        return;
    }

    let (code, message) = if size > REALLY_LARGE {
        (
            "size/extra-large",
            format!(
                "This file is really large ({size}MB). \
                 If possible, do not include it in this repository at all.",
                size = size / 1024 / 1024
            ),
        )
    } else if !excluded {
        (
            "size/large",
            format!(
                "This file is quite large ({size}MB). \
                 If it is not required to use the package \
                 (i.e. it is a documentation file, or part of an example), \
                 it should be added to `exclude` in your `typst.toml`.",
                size = size / 1024 / 1024
            ),
        )
    } else {
        return;
    };

    diags.emit(
        Diagnostic::warning()
            .with_code(code)
            .with_label(Label::primary(path.file_id(), 0..0))
            .with_message(message),
    )
}

fn check_wasm_file_size(diags: &mut Diagnostics, path: PackagePath<&Path>, original_size: u64) {
    let Some(file_name) = path.full().file_name() else {
        return;
    };
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
        .enable_feature(wasm_opt::Feature::Simd)
        .disable_feature(wasm_opt::Feature::RelaxedSimd)
        .disable_feature(wasm_opt::Feature::Gc)
        .disable_feature(wasm_opt::Feature::ExceptionHandling)
        .run(path.full(), &out);

    if wasm_opt_result.is_ok() {
        if let Ok(new_size) = std::fs::metadata(&out).map(|m| m.len()) {
            let diff = (original_size - new_size) / 1024;

            if diff > 20 {
                diags.emit(
                    Diagnostic::warning()
                        .with_label(Label::primary(path.file_id(), 0..0))
                        .with_code("size/wasm")
                        .with_message(format!(
                            "This file could be {diff}kB smaller with `wasm-opt -Os`."
                        )),
                );
            }
        }

        // TODO: ideally this should be async
        std::fs::remove_file(out).ok();
    }
}

fn exclude_examples_and_tests(diags: &mut Diagnostics, path: PackagePath<&Path>, excluded: bool) {
    if excluded {
        return;
    }

    let file_name = path.file_name().to_string_lossy();
    let warning = || Diagnostic::warning().with_label(Label::primary(path.file_id(), 0..0));
    if file_name.contains("example") {
        diags.emit(warning().with_code("exclude/example").with_message(
            "This file seems to be an example, \
             and should probably be added to `exclude` in your `typst.toml`.",
        ));
    } else if file_name.contains("test") {
        diags.emit(warning().with_code("exclude/test").with_message(
            "This file seems to be a test, \
             and should probably be added to `exclude` in your `typst.toml`.",
        ));
    }
}

fn forbid_font_files(diags: &mut Diagnostics, path: PackagePath<&Path>) {
    let Some(ext) = path.extension() else {
        return;
    };
    if !(ext == "otf" || ext == "ttf") {
        return;
    }

    diags.emit(
        Diagnostic::error()
            .with_label(Label::primary(path.file_id(), 0..0))
            .with_code("files/fonts")
            .with_message(
                "Font files are not allowed.\n\n\
                Delete them and instruct your users to install them manually, \
                in your README and/or in a documentation comment.\n\n\
                More details: https://github.com/typst/packages/blob/main/docs/resources.md#fonts-are-not-supported-in-packages",
            ),
    );
}
