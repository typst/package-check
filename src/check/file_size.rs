use std::{
    fs,
    path::{Path, PathBuf},
};

/// Size (in bytes) after which a file is considered large.
const SIZE_THRESHOLD: u64 = 1024 * 1024; // 1 MB

pub fn find_large_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut result = Vec::new();
    explore_dir(&mut result, dir);
    for (ref mut item, _) in result.iter_mut() {
        *item = item.strip_prefix(dir).unwrap().to_owned();
    }
    result
}

fn explore_dir(large: &mut Vec<(PathBuf, u64)>, dir: &Path) -> Option<()> {
    for ch in fs::read_dir(dir).ok()? {
        let Ok(ch) = ch else {
            continue;
        };
        let Ok(metadata) = ch.metadata() else {
            continue;
        };
        if metadata.is_file() {
            if metadata.len() > SIZE_THRESHOLD {
                large.push((ch.path(), metadata.len()))
            }
        } else if metadata.is_dir() {
            explore_dir(large, &ch.path());
        }
    }

    Some(())
}
