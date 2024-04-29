use std::path::{Path, PathBuf};

/// Size (in bytes) after which a file is considered large.
const SIZE_THRESHOLD: u64 = 1024 * 1024; // 1 MB

pub fn find_large_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut result = Vec::new();
    for ch in walkdir::WalkDir::new(dir) {
        let Ok(ch) = ch else {
            continue;
        };
        let Ok(metadata) = ch.metadata() else {
            continue;
        };
        if metadata.is_file() {
            if metadata.len() > SIZE_THRESHOLD {
                result.push((
                    ch.path().strip_prefix(dir).unwrap().to_owned(),
                    metadata.len(),
                ))
            }
        }
    }
    result
}
