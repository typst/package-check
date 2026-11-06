use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use typst::syntax::package::{PackageSpec, PackageVersion};
use typst_kit::fonts::FontStore;
use typst_kit::packages::FsPackages;

/// Relevant workspace directories.
#[derive(Clone)]
pub struct Workspace(Arc<WorkspaceInner>);

struct WorkspaceInner {
    repo_dir: PathBuf,
    packages: FsPackages,
    system: SystemPackages,
    fonts: LazyLock<FontStore, Box<dyn Fn() -> FontStore + Send + Sync>>,
}

impl Workspace {
    pub fn new(repo_dir: PathBuf, system: SystemPackages) -> Self {
        Self(Arc::new(WorkspaceInner {
            packages: FsPackages::new(repo_dir.join("packages")),
            repo_dir,
            system,
            fonts: LazyLock::new(Box::new(|| discover_fonts())),
        }))
    }
    /// The path of the `typst/packages` repository, which is used to resolve
    /// package imports, among other things this.
    pub fn repo_dir(&self) -> &Path {
        &self.0.repo_dir
    }

    /// The path of containing all the packages: `typst/packages/packages`.
    pub fn packages(&self) -> &FsPackages {
        &self.0.packages
    }

    /// System wide installed packages.
    pub fn system(&self) -> &SystemPackages {
        &self.0.system
    }

    /// Metadata about discovered fonts and lazily loaded fonts.
    pub fn fonts(&self) -> &FontStore {
        &self.0.fonts
    }

    /// Returns the path of the spec inside [`Self::packages`].
    pub fn path_in_repo(&self, spec: &PackageSpec) -> PathBuf {
        let relative_path = format!("{}/{}/{}", spec.namespace, spec.name, spec.version);
        self.0.packages.path().join(&relative_path)
    }

    /// Resolves the previous version of the package inside [`Self::packages`].
    pub fn find_previous_version(&self, spec: &PackageSpec) -> Option<PackageSpec> {
        let all_versions_dir = (self.0.packages.path())
            .join(spec.namespace.as_str())
            .join(spec.name.as_str());

        let mut last_version = None;
        for version_dir in std::fs::read_dir(&all_versions_dir).ok()? {
            let Ok(version_dir) = version_dir else {
                continue;
            };

            let Some(version) = version_dir
                .file_name()
                .to_str()
                .and_then(|v| v.parse::<PackageVersion>().ok())
            else {
                continue;
            };

            if version == spec.version {
                continue;
            }

            if last_version.map(|last| last < version).unwrap_or(true) {
                last_version = Some(version);
            }
        }

        last_version.map(|v| PackageSpec {
            version: v,
            name: spec.name.clone(),
            namespace: spec.namespace.clone(),
        })
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
            data: dirs::data_dir().map(FsPackages::new),
            cache: dirs::cache_dir().map(FsPackages::new),
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
