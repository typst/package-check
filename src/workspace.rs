use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use indexmap::IndexSet;
use typst::syntax::package::{PackageSpec, PackageVersion};
use typst_kit::fonts::FontStore;
use typst_kit::packages::FsPackages;

use crate::check::TryExt;
use crate::check::codes::Code;
use crate::check::diagnostics::{Diagnostic, Result};

/// Relevant workspace directories.
#[derive(Clone)]
pub struct Workspace(Arc<WorkspaceInner>);

struct WorkspaceInner {
    repo: PackagesRepo,
    system: SystemPackages,
    fonts: LazyLock<FontStore, Box<dyn Fn() -> FontStore + Send + Sync>>,
}

impl Workspace {
    pub fn new(repo_dir: PathBuf, system: SystemPackages) -> Self {
        Self(Arc::new(WorkspaceInner {
            repo: PackagesRepo::new(repo_dir),
            system,
            fonts: LazyLock::new(Box::new(discover_fonts)),
        }))
    }

    /// The local `typst/packages` repository.
    pub fn repo(&self) -> &PackagesRepo {
        &self.0.repo
    }

    /// System wide installed packages.
    pub fn system(&self) -> &SystemPackages {
        &self.0.system
    }

    /// Metadata about discovered fonts and lazily loaded fonts.
    pub fn fonts(&self) -> &FontStore {
        &self.0.fonts
    }
}

/// The local `typst/packages` repository.
#[derive(Clone)]
pub struct PackagesRepo {
    repo_dir: PathBuf,
    packages: FsPackages,
}

impl PackagesRepo {
    pub fn new(repo_dir: PathBuf) -> Self {
        Self {
            packages: FsPackages::new(repo_dir.join("packages")),
            repo_dir,
        }
    }

    /// The path of the `typst/packages` repository, which is used to resolve
    /// package imports, among other things this.
    pub fn dir(&self) -> &Path {
        &self.repo_dir
    }

    /// The path of containing all the packages: `typst/packages/packages`.
    pub fn packages(&self) -> &FsPackages {
        &self.packages
    }

    /// Returns the path of the spec inside [`Self::packages`].
    pub fn path_of(&self, spec: &PackageSpec) -> PathBuf {
        let relative_path = format!("{}/{}/{}", spec.namespace, spec.name, spec.version);
        self.packages.path().join(&relative_path)
    }

    /// Returns the `typst.toml` manifest path of the spec inside
    /// [`Self::packages`].
    pub fn manifest_path_of(&self, spec: &PackageSpec) -> PathBuf {
        self.path_of(spec).join("typst.toml")
    }

    /// Resolves the previous version of the package inside [`Self::packages`].
    pub fn find_versions(&self, spec: &PackageSpec) -> Result<IndexSet<PackageVersion>> {
        let all_versions_dir = (self.packages.path())
            .join(spec.namespace.as_str())
            .join(spec.name.as_str());
        let all_versions =
            std::fs::read_dir(all_versions_dir).error(Code::Io, "Failed to list versions")?;

        let mut versions = IndexSet::new();
        for version_dir in all_versions {
            let io_err = |err| {
                Diagnostic::error(Code::Io)
                    .with_message(format!("Failed read version directory ({err})"))
            };

            let version_dir = version_dir.map_err(io_err)?;
            if !version_dir.file_type().map_err(io_err)?.is_dir() {
                continue;
            }

            if let Some(dir) = version_dir.file_name().to_str()
                && let Ok(version) = dir.parse::<PackageVersion>()
            {
                versions.insert(version);
            }
        }

        versions.sort();
        Ok(versions)
    }
}

/// System wide installed package locations that the compiler searches.
#[derive(Clone)]
pub struct SystemPackages {
    /// Manually installed packages: [`FsPackages::system_data()`]
    pub data: Option<FsPackages>,
    /// Cached Universe packages: [`FsPackages::system_cache()`]
    pub cache: Option<FsPackages>,
}

impl SystemPackages {
    pub fn empty() -> Self {
        Self {
            data: None,
            cache: None,
        }
    }
}

impl Default for SystemPackages {
    fn default() -> Self {
        Self {
            data: dirs::data_dir().map(|path| FsPackages::new(path.join("typst/packages"))),
            cache: dirs::cache_dir().map(|path| FsPackages::new(path.join("typst/packages"))),
        }
    }
}

fn discover_fonts() -> FontStore {
    let mut fonts = FontStore::new();
    fonts.extend(typst_kit::fonts::embedded());
    // TODO: Should we not include system fonts here?
    fonts.extend(typst_kit::fonts::system());
    fonts
}
