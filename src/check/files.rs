use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use typst::syntax::{FileId, RootedPath, VirtualPath};
use walkdir::WalkDir;

use crate::check::Diagnostics;
use crate::check::diagnostics::{Code, Diagnostic, Docs, Label, Result, Severity};
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
    let res = check_special_files(diags, package_dir);
    diags.maybe_emit(res);

    // The bundler enables some local ignore features, which can be confusing.
    // Collect the list of files the bundler would consider bundling, without
    // the exclude globs. Use it to determine if a file is ignored.
    let without_ignored = walk(package_dir)
        .flatten()
        .filter_map(|ch| ch.file_type()?.is_file().then(|| ch.into_path()))
        .collect::<HashSet<_>>();

    for ch in WalkDir::new(package_dir).into_iter().flatten() {
        let Ok(metadata) = ch.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }

        let file_path = PackagePath::from_full(package_dir, ch.path());
        let excluded = manifest.package.exclude.matches_file(&file_path);
        let ignored = !without_ignored.contains(file_path.full());

        warn_ignored_files(diags, file_path, excluded, ignored);
        forbid_font_files(diags, file_path);
        exclude_large_files(diags, file_path, excluded, metadata.len());
        exclude_examples_and_tests(diags, manifest, file_path, excluded);
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

    Diagnostic::warn(Code::FilesIgnored)
        .with_message(format!(
            "This file won't be present in the bundled package{reason}"
        ))
        .with_hint(format!(
            "If this is intentional and the file is used for documentation \
             and linked in the readme, consider explicitly adding it to the \
             `exclude` list.\n {hint}"
        ))
        .with_docs_link(Docs::WHAT_TO_EXCLUDE)
        .with_label(Label::new(file_path.file_id(), 0..0))
        .emit(diags);
}

fn exclude_large_files(
    diags: &mut Diagnostics,
    path: PackagePath<&Path>,
    excluded: bool,
    size: u64,
) {
    // TODO: Adjust limits and add execptions for package exceeding the limit.
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

    let (severity, code, message) = if size > REALLY_LARGE {
        (
            Severity::Error,
            Code::SizeExtraLarge,
            format!(
                "This file is really large ({size}MB). \
                 If possible, do not include it in this repository at all.",
                size = size / 1024 / 1024
            ),
        )
    } else if !excluded {
        (
            Severity::Warning,
            Code::SizeLarge,
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

    Diagnostic::new(severity, code)
        .with_label(Label::new(path.file_id(), 0..0))
        .with_message(message)
        .emit(diags);
}

fn exclude_examples_and_tests(
    diags: &mut Diagnostics,
    manifest: &Manifest,
    path: PackagePath<&Path>,
    excluded: bool,
) {
    // Don't exclude template files, even if they contain "example" or "test" in their name.
    let in_template_dir = || {
        manifest
            .template_path()
            .is_some_and(|dir| path.full().starts_with(dir.full()))
    };
    if excluded || in_template_dir() {
        return;
    }

    let file_name = path.file_name().to_string_lossy();
    let mut file_diag = |diag: Diagnostic| {
        diag.with_docs_link(Docs::WHAT_TO_EXCLUDE)
            .with_label(Label::new(path.file_id(), 0..0))
            .emit(diags)
    };
    if file_name.contains("example") {
        file_diag(Diagnostic::warn(Code::ExcludeExample).with_message(
            "This file seems to be an example, \
             and should probably be added to `exclude` in your `typst.toml`.",
        ));
    } else if file_name.contains("test") {
        file_diag(Diagnostic::warn(Code::ExcludeTest).with_message(
            "This file seems to be a test, \
             and should probably be added to `exclude` in your `typst.toml`.",
        ));
    }
}

fn forbid_font_files(diags: &mut Diagnostics, path: PackagePath<&Path>) {
    let Some(ext) = path.extension().and_then(OsStr::to_str) else {
        return;
    };
    const EXTENSIONS: [&str; 4] = ["otf", "ttf", "woff", "woff2"];
    if !EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
        return;
    }

    Diagnostic::error(Code::FilesFonts)
        .with_message(
            "Font files are not allowed.\n\n\
             Delete them and instruct your users to install them manually, \
             in your README and/or in a documentation comment.",
        )
        .with_docs_link(Docs::FONTS_NOT_ALLOWED)
        .with_label(Label::new(path.file_id(), 0..0))
        .emit(diags);
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
        let hint = (!excluded)
            .then(|| "It should also be added to `exclude` in your `typst.toml`.".into());

        Diagnostic::warn(Code::FilesManualUnlinked)
            .with_message(
                "This file seems to be a manual/documentation, but isn't linked in the readme. \
                 It will be inacessible on Typst Universe.",
            )
            .with_hints(hint)
            .with_docs_link(Docs::WHAT_TO_EXCLUDE)
            .with_label(Label::new(path.file_id(), 0..0))
            .emit(diags);
    }
}

fn check_special_files(diags: &mut Diagnostics, package_dir: &Path) -> Result<()> {
    let entries = std::fs::read_dir(package_dir).map_err(|err| {
        Diagnostic::error(Code::Io)
            .with_message(format!("Failed to read package directory ({err})"))
    })?;
    for ch in entries
        .flatten()
        .filter(|ch| ch.file_type().is_ok_and(|ft| ft.is_file()))
    {
        let file_name = PathBuf::from(ch.file_name());
        let Some(stem) = file_name.file_stem() else {
            continue;
        };
        let stem_uppercase = stem.to_string_lossy().to_ascii_uppercase();

        let label_for_file = || {
            let file_id = FileId::new(RootedPath::new(
                typst::syntax::VirtualRoot::Project,
                VirtualPath::new(file_name.to_str().unwrap()).unwrap(),
            ));
            Label::new(file_id, 0..0)
        };

        if matches!(stem_uppercase.as_str(), "LICENSE" | "LICENCE") && &file_name != "LICENSE" {
            Diagnostic::error(Code::FilesLicenseName)
                .with_message(
                    "This file should be named `LICENSE`, \
                     all uppercase and without a file extension.",
                )
                .with_docs_link(Docs::LICENSE)
                .with_label(label_for_file())
                .emit(diags);
        } else if stem_uppercase == "README" && &file_name != "README.md" {
            let hint = file_name
                .extension()
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("md"))
                .then(|| "READMEs must be formatted in Markdown".to_string());

            Diagnostic::error(Code::FilesReadmeName)
                .with_message(
                    "This file should be named `README.md`, \
                     all uppercase and with the `.md` file extension.",
                )
                .with_hints(hint)
                .with_docs_link(Docs::DOCS)
                .with_label(label_for_file())
                .emit(diags);
        }
    }

    Ok(())
}
