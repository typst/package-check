use std::path::{Path, PathBuf};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use ignore::overrides::Override;
use typst::syntax::{FileId, VirtualPath};

use crate::check::{Diagnostics, Result, TryExt};

/// Size (in bytes) after which a file is considered large.
const SIZE_THRESHOLD: u64 = 1024 * 1024; // 1 MB

pub fn find_large_files(dir: &Path, exclude: Override) -> Result<Vec<(PathBuf, u64)>> {
    let mut result = Vec::new();
    for ch in ignore::WalkBuilder::new(dir).overrides(exclude).build() {
        let Ok(ch) = ch else {
            continue;
        };
        let Ok(metadata) = ch.metadata() else {
            continue;
        };
        if metadata.is_file() && metadata.len() > SIZE_THRESHOLD {
            result.push((
                ch.path()
                    .strip_prefix(dir)
                    .error("internal", "Prefix striping failed even though child path (`ch`) was constructed from parent path (`dir`)")?
                    .to_owned(),
                metadata.len(),
            ))
        }
    }
    Ok(result)
}

pub fn forbid_font_files(
    package_dir: &Path,
    diags: &mut Diagnostics,
) -> std::result::Result<(), Diagnostic<FileId>> {
    for ch in ignore::WalkBuilder::new(package_dir).build() {
        let Ok(ch) = ch else {
            continue;
        };
        let Ok(metadata) = ch.metadata() else {
            continue;
        };

        let ext = ch
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if metadata.is_file() && (&ext == "otf" || &ext == "ttf") {
            let file_id = FileId::new(None, VirtualPath::new(ch.path().strip_prefix(package_dir)
                    .error("internal", "Prefix striping failed even though child path (`ch`) was constructed from parent path (`dir`)")?
        ));
            diags.emit(
                Diagnostic::error()
                    .with_label(Label::primary(file_id, 0..0))
                    .with_code("files/fonts")
                    .with_message(
                        "Font files are not allowed.\n\n\
                        Delete them and instruct your users to install them manually, \
                        in your README and/or in a documentation comment.\n\n\
                        More details: https://github.com/typst/packages/blob/main/docs/resources.md#fonts-are-not-supported-in-packages",
                    ),
            );
        }
    }

    Ok(())
}
