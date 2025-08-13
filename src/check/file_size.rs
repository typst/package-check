use std::path::{Path, PathBuf};

use ignore::overrides::Override;

use crate::check::{Result, TryExt};

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
