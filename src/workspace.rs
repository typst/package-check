use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use indexmap::IndexSet;
use typst::foundations::Smart;
use typst::syntax::package::{PackageSpec, PackageVersion};
use typst_kit::fonts::FontStore;
use typst_kit::packages::FsPackages;

use crate::check::TryExt;
use crate::check::codes::Code;
use crate::check::diagnostics::{Diagnostic, Result};

/// System configuration.
#[derive(Default)]
pub struct System {
    pub package_path: Smart<Option<PathBuf>>,
    pub package_cache_path: Smart<Option<PathBuf>>,
    pub ignore_system_fonts: bool,
}

impl System {
    pub fn sandboxed() -> Self {
        Self {
            package_path: Smart::Custom(None),
            package_cache_path: Smart::Custom(None),
            ignore_system_fonts: true,
        }
    }
}

/// Relevant workspace directories.
#[derive(Clone)]
pub struct Workspace(Arc<WorkspaceInner>);

struct WorkspaceInner {
    repo: Option<PackagesRepo>,
    system: SystemPackages,
    fonts: LazyLock<FontStore, Box<dyn Fn() -> FontStore + Send + Sync>>,
}

impl Workspace {
    pub fn new(repo_dir: Option<PathBuf>, system: System) -> Self {
        Self(Arc::new(WorkspaceInner {
            repo: repo_dir.map(PackagesRepo::new),
            fonts: LazyLock::new(Box::new(move || discover_fonts(system.ignore_system_fonts))),
            system: SystemPackages {
                data: system
                    .package_path
                    .unwrap_or_else(SystemPackages::data_dir)
                    .map(FsPackages::new),
                cache: system
                    .package_cache_path
                    .unwrap_or_else(SystemPackages::cache_dir)
                    .map(FsPackages::new),
            },
        }))
    }

    /// The local `typst/packages` repository.
    pub fn repo(&self) -> Option<&PackagesRepo> {
        self.0.repo.as_ref()
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
    pub fn data_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|path| path.join("typst/packages"))
    }

    pub fn cache_dir() -> Option<PathBuf> {
        dirs::cache_dir().map(|path| path.join("typst/packages"))
    }
}

fn discover_fonts(ignore_system_fonts: bool) -> FontStore {
    let mut fonts = FontStore::new();
    fonts.extend(typst_kit::fonts::embedded());

    if !ignore_system_fonts {
        fonts.extend(typst_kit::fonts::system());
    }

    fonts
}
