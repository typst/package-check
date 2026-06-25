use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;

use codespan_reporting::diagnostic::{Diagnostic, Label};
use walkdir::WalkDir;

use crate::check::Diagnostics;
use crate::check::manifest::Manifest;
use crate::check::path::PackagePath;
use crate::check::readme::Readme;

/// Creates a directory iterator with the same settings as the package bundler.
pub fn walk(dir: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(dir)
        .sort_by_file_name(|a, b| a.cmp(b))
        // Disable non-local ignore features
        .parents(false)
        .require_git(false)
        .git_global(false)
        .git_exclude(false)
        // Keep local ignore features for now.
        .git_ignore(true)
        .ignore(true)
        .hidden(true)
        .build()
}

pub fn check(
    diags: &mut Diagnostics,
    package_dir: &Path,
    manifest: &Manifest,
    readme: &Option<Readme>,
) {
    let exclude = &manifest.package.exclude;

    // The bundler enables some local ignore features, which can be confusing.
    // Collect the list of files the bundler would consider bundling, without
    // the exclude globs. Use it to determine if a file is ignored.
    let without_ignored = walk(package_dir)
        .flatten()
        .filter_map(|ch| {
            let metadata = ch.metadata().ok()?;
            metadata.is_file().then_some(ch.into_path())
        })
        .collect::<HashSet<_>>();

    for ch in WalkDir::new(package_dir).into_iter().flatten() {
        let Ok(metadata) = ch.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }

        let file_path = PackagePath::from_full(package_dir, ch.path());
        let excluded = exclude.matches_file(&file_path);
        let ignored = !without_ignored.contains(file_path.full());

        warn_ignored_files(diags, file_path, excluded, ignored);
        forbid_font_files(diags, file_path);
        exclude_large_files(diags, file_path, excluded, metadata.len());
        exclude_examples_and_tests(diags, file_path, excluded);
        link_manuals(diags, readme, file_path, excluded);
    }
}

fn warn_ignored_files(
    diags: &mut Diagnostics,
    file_path: PackagePath<&Path>,
    excluded: bool,
    ignored: bool,
) {
    if excluded || !ignored {
        return;
    }

    // Don't emit noisy warnings for common hidden files that won't be a problem
    // when missing from the bundle.
    const COMMON: [&str; 6] = [
        ".gitattributes",
        ".gitignore",
        ".gitkeep",
        ".ignore",
        ".keep",
        ".typstignore",
    ];
    if COMMON.map(OsStr::new).contains(&file_path.file_name()) {
        return;
    }

    let (reason, hint) = if file_path.file_name().as_encoded_bytes().starts_with(b".") {
        (
            ".\nIt's ignored, because it is hidden: the file name starts with a `.`.",
            "If not, consider removing the file.",
        )
    } else {
        (
            " because of an ignore file, such as `.gitignore` or `.ignore`.",
            "If not, consider removing it or updating the ignore file.",
        )
    };

    diags.emit(
        Diagnostic::warning()
            .with_code("files/ignored")
            .with_label(Label::primary(file_path.file_id(), 0..0))
            .with_message(format_args!(
                "This file won't be present in the bundled package{reason}\n\n\
                 If this is intentional and the file is used for documentation and linked in the readme, \
                 consider explicitly adding it to the `exclude` list.\n\
                 {hint}\n\n\
                 More details: https://github.com/typst/packages/blob/main/docs/tips.md#what-to-commit-what-to-exclude",
            )),
    );
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

fn link_manuals(
    diags: &mut Diagnostics,
    readme: &Option<Readme>,
    path: PackagePath<&Path>,
    excluded: bool,
) {
    let Some(readme) = readme.as_ref() else {
        return;
    };

    let name = path.file_name().to_string_lossy().to_lowercase();

    const MANUAL_FILES: [&str; 4] = ["manual.pdf", "doc.pdf", "docs.pdf", "documentation.pdf"];
    if MANUAL_FILES.contains(&name.as_str())
        && readme.linked_files.iter().all(|l| l.full() != path.full())
    {
        let note = (!excluded)
            .then(|| "It should also be added to `exclude` in your `typst.toml`.".into());

        diags.emit(
            Diagnostic::warning()
                .with_label(Label::primary(path.file_id(), 0..0))
                .with_code("files/manual/unlinked")
                .with_message(
                    "This file seems to be a manual/documentation, but isn't linked in the readme. \
                 It will be inacessible on Typst Universe.",
                )
                .with_notes_iter(note),
        );
    }
}
