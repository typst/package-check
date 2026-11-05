use std::path::PathBuf;

use typst::syntax::package::{PackageSpec, PackageVersion};
use typst_kit::packages::FsPackages;

/// Relevant workspace directories.
#[derive(Clone)]
pub struct Workspace {
    /// The path of the `typst/packages` repository, which is used to resolve
    /// package imports, among other things this.
    pub packages_repo_dir: PathBuf,
    /// The path of containing all the packages: `typst/packages/packages`.
    pub packages: FsPackages,
    /// System wide installed packages.
    pub system: SystemPackages,
}

impl Workspace {
    pub fn new(packages_repo_dir: PathBuf, system: SystemPackages) -> Self {
        Self {
            packages: FsPackages::new(packages_repo_dir.join("packages")),
            packages_repo_dir,
            system,
        }
    }

    /// Returns the path of the spec inside [`Self::packages`].
    pub fn path_in_repo(&self, spec: &PackageSpec) -> PathBuf {
        let relative_path = format!("{}/{}/{}", spec.namespace, spec.name, spec.version);
        self.packages.path().join(&relative_path)
    }

    /// Resolves the previous version of the package inside [`Self::packages`].
    pub fn find_previous_version(&self, spec: &PackageSpec) -> Option<PackageSpec> {
        let all_versions_dir = self
            .packages
            .path()
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
