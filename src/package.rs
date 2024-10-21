use std::path::{Path, PathBuf};

use typst::syntax::package::{PackageSpec, PackageVersion, VersionlessPackageSpec};

/// Return the path of the directory containing all the packages (i.e. `typst/packages/packages`).
pub fn dir() -> PathBuf {
    Path::new(&std::env::var("PACKAGES_DIR").unwrap_or("..".to_owned())).join("packages")
}

pub trait PackageExt: Sized {
    type Versionless;

    fn previous_version(&self) -> Option<Self>;

    fn directory(&self) -> PathBuf;
}

impl PackageExt for PackageSpec {
    type Versionless = VersionlessPackageSpec;

    fn previous_version(&self) -> Option<Self> {
        let all_versions_dir = self.versionless().directory();
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

            if version == self.version {
                continue;
            }

            if last_version.map(|last| last < version).unwrap_or(true) {
                last_version = Some(version);
            }
        }

        last_version.map(|v| PackageSpec {
            version: v,
            name: self.name.clone(),
            namespace: self.namespace.clone(),
        })
    }

    fn directory(&self) -> PathBuf {
        dir()
            .join(self.namespace.as_str())
            .join(self.name.as_str())
            .join(self.version.to_string())
    }
}

pub trait VersionlessPackageExt {
    fn directory(&self) -> PathBuf;
}

impl VersionlessPackageExt for VersionlessPackageSpec {
    fn directory(&self) -> PathBuf {
        dir().join(self.namespace.as_str()).join(self.name.as_str())
    }
}
