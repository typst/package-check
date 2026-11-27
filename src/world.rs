//! Typst World.
//!
//! Most of this module is copied from typst-cli.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use tracing::{Level, debug, span};
use typst::Features;
use typst::diag::{FileError, FileResult, PackageError, PackageResult};
use typst::foundations::Duration;
use typst::foundations::{Bytes, Datetime};
use typst::syntax::package::PackageSpec;
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::files::{FileLoader, FileStore, FsRoot};

use crate::check::Exclude;
use crate::check::files::Ignored;
use crate::workspace::Workspace;

/// A world that provides access to the operating system.
pub struct SystemWorld {
    /// The input path.
    main: FileId,
    /// Typst's standard library.
    library: LazyHash<Library>,
    /// Maps file ids to source files and buffers.
    files: FileStore<CheckedLoader>,
    /// The current datetime if requested. This is stored here to ensure it is
    /// always the same within one compilation.
    /// Reset between compilations if not [`Now::Fixed`].
    now: typst_kit::datetime::Time,
}

impl SystemWorld {
    /// Create a new system world.
    pub fn new(
        input: VirtualPath,
        workspace: Workspace,
        root: WorldRoot,
        features: Features,
        package_spec: Option<PackageSpec>,
        exclude: Exclude,
    ) -> Self {
        let main = FileId::new(RootedPath::new(VirtualRoot::Project, input));
        let library = Library::builder().with_features(features).build();
        let loader = CheckedLoader {
            workspace,
            root,
            package_spec,
            exclude,
            ignored: Ignored::empty(),
            virtual_sources: Mutex::new(HashMap::new()),
        };
        Self {
            main,
            library: LazyHash::new(library),
            files: FileStore::new(loader),
            now: typst_kit::datetime::Time::system(),
        }
    }

    /// Set excluded files.
    pub fn with_exclude(mut self, exclude: Exclude) -> Self {
        // Reset the file store, so the new rules will apply.
        self.files.reset();
        self.files.loader_mut().exclude = exclude;
        self
    }

    /// Set ignored files.
    pub fn with_ignored(mut self, ignored: Ignored) -> Self {
        // Reset the file store, so the new rules will apply.
        self.files.reset();
        self.files.loader_mut().ignored = ignored;
        self
    }

    /// The root relative to which absolute paths are resolved.
    pub fn root(&self) -> &WorldRoot {
        &self.files.loader().root
    }

    /// Lookup a source file by id.
    #[track_caller]
    pub fn lookup(&self, id: FileId) -> FileResult<Source> {
        self.source(id)
    }

    pub fn virtual_source(
        &self,
        id: FileId,
        bytes: Bytes,
        line_shift: usize,
    ) -> FileResult<Source> {
        let virt = VirtualSource { line_shift, bytes };
        self.files.loader().virtual_sources.lock().insert(id, virt);
        self.files.source(id)
    }

    pub fn virtual_line_shift(&self, id: FileId) -> usize {
        let sources = self.files.loader().virtual_sources.lock();
        sources.get(&id).map(|virt| virt.line_shift).unwrap_or(0)
    }

    pub fn package_spec(&self) -> Option<&PackageSpec> {
        self.files.loader().package_spec.as_ref()
    }

    /// Returns all loaded sources are inside [`VirtualRoot::Project`].
    pub fn project_sources(&mut self) -> Vec<Source> {
        // Collect into `Vec` so we don't hold a lock over `slots`.
        let (_, ids) = self.files.dependencies();
        let ids = ids
            .filter(|id| id.root() == &VirtualRoot::Project)
            .collect::<Vec<_>>();

        ids.into_iter()
            .filter_map(|id| self.source(id).ok())
            .collect()
    }
}

impl World for SystemWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.files.loader().workspace.fonts().book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.files.source(id)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files.file(id)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.files.loader().workspace.fonts().font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.now.today(offset)
    }
}

/// A [`FileLoader`] that checks for excluded and ignored files.
struct CheckedLoader {
    /// The workspace in which the package is checked.
    workspace: Workspace,
    /// The root relative to which absolute paths are resolved.
    root: WorldRoot,
    /// The package specification of the currently checked package.
    package_spec: Option<PackageSpec>,
    /// Files that are considered excluded and should not be read from.
    exclude: Exclude,
    /// Files that are ignored, due to bundling rules.
    ignored: Ignored,

    /// Virtual sources.
    virtual_sources: Mutex<HashMap<FileId, VirtualSource>>,
}

impl FileLoader for CheckedLoader {
    fn load(&self, id: FileId) -> FileResult<Bytes> {
        if let Some(virt) = self.virtual_sources.lock().get(&id) {
            Ok(virt.bytes.clone())
        } else {
            let path = self.resolve_system_path(id)?;
            read_from_disk(&path).map(Bytes::new)
        }
    }
}

impl CheckedLoader {
    fn resolve_system_path(&self, id: FileId) -> FileResult<PathBuf> {
        let _ = span!(Level::DEBUG, "Path resolution").enter();
        debug!("File ID = {:?}", id);

        // Determine the root path relative to which the file path will be resolved.
        let mut resolved = None;
        let root = match id.root() {
            VirtualRoot::Project => self.root.world_dir(),
            VirtualRoot::Package(spec) => {
                // If the current package is imported, return the package dir
                // directly, otherwise try to find the package in the workspace.
                if Some(spec) == self.package_spec.as_ref() {
                    self.root.package_dir()
                } else {
                    let root =
                        find_in_workspace(&self.workspace, spec).map_err(FileError::Package)?;
                    resolved.insert(root).path()
                }
            }
        };

        let realized_path = id.vpath().realize(root).map_err(FileError::from)?;

        // NOTE: To be fully correct, we would need to read the exclude globs of
        // the imported package and filter out files that are imported but
        // excluded. Though these issues will most likely be discovered during
        // package development.
        if let Ok(relative_path) = realized_path.strip_prefix(self.root.package_dir()) {
            if self.exclude.matches_relative_file(relative_path) {
                debug!("This file is excluded");
                return Err(FileError::Other(Some(
                    "This file exists but is excluded from your package.".into(),
                )));
            }

            if self.ignored.matches_relative_file(relative_path) {
                debug!("This file is ignored");
                return Err(FileError::Other(Some(
                    "This file exists but is ignored from your package.".into(),
                )));
            }
        }

        debug!("Resolved to {}", realized_path.display());
        Ok(realized_path)
    }
}

struct VirtualSource {
    line_shift: usize,
    bytes: Bytes,
}

#[derive(Clone)]
pub enum WorldRoot {
    Package(PathBuf),
    Template { package: PathBuf, template: PathBuf },
}

impl WorldRoot {
    pub fn is_package(&self) -> bool {
        matches!(self, Self::Package(..))
    }

    /// Returns the directory of the package this world lives in.
    pub fn package_dir(&self) -> &Path {
        match self {
            WorldRoot::Package(path) => path,
            WorldRoot::Template { package, .. } => package,
        }
    }

    /// Returns the root of the world, either the template directory if this is
    /// a template root, otherwise the package directory.
    pub fn world_dir(&self) -> &Path {
        match self {
            WorldRoot::Package(path) => path,
            WorldRoot::Template { template, .. } => template,
        }
    }

    /// Returns the relative template dir.
    pub fn relative_template_dir(&self) -> Option<&Path> {
        match self {
            WorldRoot::Package(_) => None,
            WorldRoot::Template { package, template } => {
                Some(template.strip_prefix(package).unwrap())
            }
        }
    }

    /// Returns the path in which all pacakges reside.
    pub fn all_packages(&self) -> Option<&Path> {
        // 1. version
        // 2. package name
        // 3. namespace
        self.package_dir().parent()?.parent()?.parent()
    }
}

/// Try to find a pacakge in current git directory or in the on-disk cache.
pub fn find_in_workspace(workspace: &Workspace, spec: &PackageSpec) -> PackageResult<FsRoot> {
    if let Some(repo) = workspace.repo()
        && let Some(root) = repo.packages().obtain(spec)
    {
        return Ok(root);
    }

    if let Some(data) = &workspace.system().data
        && let Some(root) = data.obtain(spec)
    {
        return Ok(root);
    }

    if let Some(cache) = &workspace.system().cache
        && let Some(root) = cache.obtain(spec)
    {
        return Ok(root);
    }

    Err(PackageError::NetworkFailed(Some(
        "All packages are supposed to be present in the \
         `packages` repository, or in the local cache."
            .into(),
    )))
}

/// Read a file from disk.
fn read_from_disk(path: &Path) -> FileResult<Vec<u8>> {
    let f = |e| FileError::from_io(e, path);
    if std::fs::metadata(path).map_err(f)?.is_dir() {
        Err(FileError::IsDirectory)
    } else {
        std::fs::read(path).map_err(f)
    }
}
