use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use typst::syntax::{FileId, VirtualPath};

/// A path inside a package that allows either retrieving the full or the
/// package-relative path.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct PackagePath<T = PathBuf> {
    offset: usize,
    full_path: T,
}

impl PackagePath<PathBuf> {
    /// Create a new package path from the package directory and a relative path
    /// within the directory.
    pub fn from_relative<T: AsRef<Path>>(package_dir: &Path, relative_path: T) -> Self {
        let full_path = path_relative_to(package_dir, relative_path.as_ref());
        let offset = package_dir.components().count();
        Self { offset, full_path }
    }

    pub fn as_path(&self) -> PackagePath<&Path> {
        PackagePath {
            offset: self.offset,
            full_path: self.full_path.as_path(),
        }
    }
}

impl<T: AsRef<Path>> PackagePath<T> {
    /// Create a new package path from the package dir and a full path that has
    /// to start with the package directory.
    pub fn from_full(package_dir: &Path, full_path: T) -> Self {
        let path = full_path.as_ref();

        path.strip_prefix(package_dir)
            .expect("full path must start with the pacakge directory");

        let offset = package_dir.components().count();
        Self { offset, full_path }
    }

    /// The full path, not necessarily absolute, but it will resolve the file
    /// from the current directory.
    pub fn full(&self) -> &Path {
        self.full_path.as_ref()
    }

    /// The path relative to the package directory.
    pub fn relative(&self) -> &Path {
        let mut components = self.full_path.as_ref().components();
        for _ in 0..self.offset {
            components.next();
        }
        components.as_path()
    }

    pub fn file_name(&self) -> &OsStr {
        self.full()
            .file_name()
            .expect("package path must be file or directory within a package")
    }

    pub fn extension(&self) -> Option<&OsStr> {
        self.full().extension()
    }

    /// The [`FileId`] of the package relative path.
    pub fn file_id(&self) -> FileId {
        FileId::new(None, VirtualPath::new(self.relative()))
    }
}

/// Strips any any leading root components (`/` or `\`) of the `path` before
/// joining it to the `root` path. Absolute paths would otherwise replace the
/// complete path when `join`ed with a parent path.
pub fn path_relative_to(root: &Path, path: &Path) -> PathBuf {
    let components = path
        .components()
        .skip_while(|c| matches!(c, std::path::Component::RootDir))
        .map(|c| Path::new(c.as_os_str()));

    PathBuf::from_iter(std::iter::once(root).chain(components))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_relative() {
        #[track_caller]
        fn test_relative(root: &str, relative: &str) {
            let path = PackagePath::from_relative(Path::new(root), relative);
            assert_eq!(path.full(), "/root/path/to/package/src/thing.typ");
            assert_eq!(path.relative(), "src/thing.typ");
        }

        test_relative("/root/path/to/package/", "/src/thing.typ");
        test_relative("/root/path/to/package/", "src/thing.typ");
        test_relative("/root/path/to/package", "/src/thing.typ");
        test_relative("/root/path/to/package", "src/thing.typ");
    }

    #[test]
    fn from_full() {
        #[track_caller]
        fn test_full(root: &str, relative: &str) {
            let path = PackagePath::from_full(Path::new(root), relative);
            assert_eq!(path.full(), "/root/path/to/package/src/thing.typ");
            assert_eq!(path.relative(), "src/thing.typ");
        }

        test_full(
            "/root/path/to/package/",
            "/root/path/to/package/src/thing.typ",
        );
        test_full(
            "/root/path/to/package",
            "/root/path/to/package/src/thing.typ",
        );
    }
}
