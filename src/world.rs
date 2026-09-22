//! Typst World.
//!
//! Most of this module is copied from typst-cli.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use fontdb::Database;
use parking_lot::Mutex;
use tracing::{Level, debug, span};
use typst::foundations::Duration;
use typst::syntax::{RealizeError, RootedPath, VirtualRoot};
use typst::{
    Library, LibraryExt, World,
    diag::{FileError, FileResult, PackageError, PackageResult},
    foundations::{Bytes, Datetime},
    syntax::{FileId, Source, VirtualPath, package::PackageSpec},
    text::{Font, FontBook, FontInfo},
    utils::LazyHash,
};

use crate::check::Exclude;
use crate::package::PackageExt;

/// A world that provides access to the operating system.
pub struct SystemWorld {
    /// The root relative to which absolute paths are resolved.
    root: WorldRoot,
    /// The input path.
    main: FileId,
    /// Typst's standard library.
    library: LazyHash<Library>,
    /// Metadata about discovered fonts.
    book: LazyHash<FontBook>,
    /// Locations of and storage for lazily loaded fonts.
    fonts: Vec<FontSlot>,
    /// Maps file ids to source files and buffers.
    slots: Mutex<HashMap<FileId, FileSlot>>,
    /// The current datetime if requested. This is stored here to ensure it is
    /// always the same within one compilation.
    /// Reset between compilations if not [`Now::Fixed`].
    now: typst_kit::datetime::Time,
    /// The package specification of the currently checked package.
    package_spec: Option<PackageSpec>,
    /// Files that are considered excluded and should not be read from.
    exclude: Exclude,
}

impl SystemWorld {
    /// Create a new system world.
    pub fn new(input: VirtualPath, root: WorldRoot, package_spec: Option<PackageSpec>) -> Self {
        let main = FileId::new(RootedPath::new(VirtualRoot::Project, input));

        let library = Library::default();

        let mut searcher = FontSearcher::new();
        searcher.search(&[]);

        Self {
            root,
            main,
            library: LazyHash::new(library),
            book: LazyHash::new(searcher.book),
            fonts: searcher.fonts,
            slots: Mutex::new(HashMap::new()),
            now: typst_kit::datetime::Time::system(),
            package_spec,
            exclude: Exclude::empty(),
        }
    }

    pub fn exclude(mut self, exclude: Exclude) -> Self {
        self.exclude = exclude;
        self
    }

    /// The root relative to which absolute paths are resolved.
    pub fn root(&self) -> &WorldRoot {
        &self.root
    }

    /// Get the realized entrypoint path.
    pub fn entrypoint(&self) -> PathBuf {
        self.root()
            .realize(self.main().vpath())
            .expect("main file to be inside the world root")
    }

    /// Lookup a source file by id.
    #[track_caller]
    pub fn lookup(&self, id: FileId) -> FileResult<Source> {
        self.source(id)
    }

    pub fn virtual_source(&self, id: FileId, src: Bytes, line_shift: usize) -> FileResult<Source> {
        self.slot(id, |slot| slot.virtual_source(src, line_shift))
    }

    pub fn virtual_line(&self, id: FileId) -> usize {
        self.slot(id, |f| f.line_shift)
    }

    pub fn package_spec(&self) -> Option<&PackageSpec> {
        self.package_spec.as_ref()
    }
}

impl World for SystemWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.slot(id, |slot| {
            slot.source(&self.root, self.package_spec.as_ref(), &self.exclude)
        })
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.slot(id, |slot| {
            slot.file(&self.root, self.package_spec.as_ref(), &self.exclude)
        })
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts[index].get()
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.now.today(offset)
    }
}

impl SystemWorld {
    /// Access the canonical slot for the given file id.
    fn slot<F, T>(&self, id: FileId, f: F) -> T
    where
        F: FnOnce(&mut FileSlot) -> T,
    {
        let mut map = self.slots.lock();
        f(map.entry(id).or_insert_with(|| FileSlot::new(id)))
    }
}

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

    /// Virtualize a path relative to this world root.
    pub fn realize(&self, path: &VirtualPath) -> Result<PathBuf, RealizeError> {
        let root = match self {
            WorldRoot::Package(path) => path,
            WorldRoot::Template { template, .. } => template,
        };
        path.realize(root)
    }
}

/// Holds the processed data for a file ID.
///
/// Both fields can be populated if the file is both imported and read().
struct FileSlot {
    /// The slot's file id.
    id: FileId,
    /// The lazily loaded and incrementally updated source file.
    source: SlotCell<Source>,
    /// The lazily loaded raw byte buffer.
    file: SlotCell<Bytes>,
    line_shift: usize,
}

impl FileSlot {
    /// Create a new file slot.
    fn new(id: FileId) -> Self {
        Self {
            id,
            file: SlotCell::new(),
            source: SlotCell::new(),
            line_shift: 0,
        }
    }

    /// Retrieve the source for this file.
    fn source(
        &mut self,
        root: &WorldRoot,
        override_spec: Option<&PackageSpec>,
        exclude: &Exclude,
    ) -> FileResult<Source> {
        self.source.get_or_init(
            || read(root, override_spec, exclude, self.id),
            |data, prev| {
                let text = decode_utf8(&data)?;
                if let Some(mut prev) = prev {
                    prev.replace(text);
                    Ok(prev)
                } else {
                    Ok(Source::new(self.id, text.into()))
                }
            },
        )
    }

    fn virtual_source(&mut self, src: Bytes, line_shift: usize) -> FileResult<Source> {
        self.line_shift = line_shift;
        self.source.get_or_init(
            || Ok(src.to_vec()),
            |data, prev| {
                let text = decode_utf8(&data)?;
                if let Some(mut prev) = prev {
                    prev.replace(text);
                    Ok(prev)
                } else {
                    Ok(Source::new(self.id, text.into()))
                }
            },
        )
    }

    /// Retrieve the file's bytes.
    fn file(
        &mut self,
        root: &WorldRoot,
        override_spec: Option<&PackageSpec>,
        exclude: &Exclude,
    ) -> FileResult<Bytes> {
        self.file.get_or_init(
            || read(root, override_spec, exclude, self.id),
            |data, _| Ok(Bytes::new(data)),
        )
    }
}

/// Lazily processes data for a file.
struct SlotCell<T> {
    /// The processed data.
    data: Option<FileResult<T>>,
    /// A hash of the raw file contents / access error.
    fingerprint: u128,
    /// Whether the slot has been accessed in the current compilation.
    accessed: bool,
}

impl<T: Clone> SlotCell<T> {
    /// Creates a new, empty cell.
    fn new() -> Self {
        Self {
            data: None,
            fingerprint: 0,
            accessed: false,
        }
    }

    /// Gets the contents of the cell or initialize them.
    fn get_or_init(
        &mut self,
        load: impl FnOnce() -> FileResult<Vec<u8>>,
        f: impl FnOnce(Vec<u8>, Option<T>) -> FileResult<T>,
    ) -> FileResult<T> {
        // If we accessed the file already in this compilation, retrieve it.
        if std::mem::replace(&mut self.accessed, true)
            && let Some(data) = &self.data
        {
            return data.clone();
        }

        // Read and hash the file.
        let result = load();
        let fingerprint = typst::utils::hash128(&result);

        // If the file contents didn't change, yield the old processed data.
        if std::mem::replace(&mut self.fingerprint, fingerprint) == fingerprint
            && let Some(data) = &self.data
        {
            return data.clone();
        }

        let prev = self.data.take().and_then(Result::ok);
        let value = result.and_then(|data| f(data, prev));
        self.data = Some(value.clone());

        value
    }
}

/// Reads a file from a `FileId`.
fn read(
    root: &WorldRoot,
    override_spec: Option<&PackageSpec>,
    exclude: &Exclude,
    id: FileId,
) -> FileResult<Vec<u8>> {
    let resolved = resolve_system_path(root, override_spec, exclude, id)?;
    read_from_disk(&resolved)
}

/// Resolves the path of a file id on the system.
fn resolve_system_path(
    root: &WorldRoot,
    override_spec: Option<&PackageSpec>,
    exclude: &Exclude,
    id: FileId,
) -> FileResult<PathBuf> {
    let _ = span!(Level::DEBUG, "Path resolution").enter();
    debug!("File ID = {:?}", id);

    // Determine the root path relative to which the file path will be resolved.
    let mut resolved = None;
    let root = match id.root() {
        VirtualRoot::Project => root.world_dir(),
        VirtualRoot::Package(spec) => {
            // If the current package is imported, return the package dir
            // directly, otherwise try to find the package:
            // 1. relative to the current world's package dir.
            // 2. inside the current git directory.
            // 3. in the global package cache.
            if Some(spec) == override_spec {
                root.package_dir()
            } else if let Some(dir) = find_relative_package(root, spec) {
                resolved.insert(dir)
            } else {
                let dir = find_in_git_dir_or_cache(spec).map_err(FileError::Package)?;
                resolved.insert(dir)
            }
        }
    };

    let realized_path = id.vpath().realize(root).or(Err(FileError::AccessDenied))?;

    // FIXME: To be fully correct, we would need to read the exclude globs
    // of the imported package and filter out files that are imported but
    // excluded. Though these issues will most likely be discovered during
    // package development.
    if let Ok(exclude_relative_path) = realized_path.strip_prefix(exclude.root())
        && exclude.matches_relative_file(exclude_relative_path)
    {
        debug!("This file is excluded");
        return Err(FileError::Other(Some(
            "This file exists but is excluded from your package.".into(),
        )));
    }

    debug!("Resolved to {}", realized_path.display());
    Ok(realized_path)
}

// Goes up in a file system hierarchy while the parent folder matches the expected name
fn find_relative_package(root: &WorldRoot, spec: &PackageSpec) -> Option<PathBuf> {
    let dir = root.all_packages()?;
    let mut buf = dir.to_path_buf();

    buf.push(spec.namespace.as_str());
    buf.push(spec.name.as_str());
    buf.push(spec.version.to_string());

    if !buf.exists() {
        debug!(
            "Expected package `{spec}` to be present in `{}`",
            dir.display()
        );
        return None;
    }

    Some(buf)
}

/// Try to find a pacakge in current git directory or in the on-disk cache.
fn find_in_git_dir_or_cache(spec: &PackageSpec) -> PackageResult<PathBuf> {
    let git_package_dir = spec.git_dir();
    if git_package_dir.exists() {
        return Ok(git_package_dir);
    }

    let subdir = format!(
        "typst/packages/{}/{}/{}",
        spec.namespace, spec.name, spec.version
    );

    if let Some(data_dir) = dirs::data_dir() {
        let dir = data_dir.join(&subdir);
        if dir.exists() {
            return Ok(dir);
        }
    }

    if let Some(cache_dir) = dirs::cache_dir() {
        let dir = cache_dir.join(&subdir);
        if dir.exists() {
            return Ok(dir);
        }

        return Err(PackageError::NetworkFailed(Some(
            "All packages are supposed to be present in the `packages` repository, or in the local cache.".into(),
        )));
    }

    Err(PackageError::NotFound(spec.clone()))
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

/// Decode UTF-8 with an optional BOM.
fn decode_utf8(buf: &[u8]) -> FileResult<&str> {
    // Remove UTF-8 BOM.
    Ok(std::str::from_utf8(
        buf.strip_prefix(b"\xef\xbb\xbf").unwrap_or(buf),
    )?)
}

/// Searches for fonts.
pub struct FontSearcher {
    /// Metadata about all discovered fonts.
    pub book: FontBook,
    /// Slots that the fonts are loaded into.
    pub fonts: Vec<FontSlot>,
}

/// Holds details about the location of a font and lazily the font itself.
pub struct FontSlot {
    /// The path at which the font can be found on the system.
    path: PathBuf,
    /// The index of the font in its collection. Zero if the path does not point
    /// to a collection.
    index: u32,
    /// The lazily loaded font.
    font: OnceLock<Option<Font>>,
}

impl FontSlot {
    /// Get the font for this slot.
    pub fn get(&self) -> Option<Font> {
        self.font
            .get_or_init(|| {
                let data = Bytes::new(std::fs::read(&self.path).ok()?);
                Font::new(data, self.index)
            })
            .clone()
    }
}

impl FontSearcher {
    /// Create a new, empty system searcher.
    pub fn new() -> Self {
        Self {
            book: FontBook::new(),
            fonts: vec![],
        }
    }

    /// Search everything that is available.
    pub fn search(&mut self, font_paths: &[PathBuf]) {
        let mut db = Database::new();

        // Font paths have highest priority.
        for path in font_paths {
            db.load_fonts_dir(path);
        }

        // System fonts have second priority.
        db.load_system_fonts();

        for face in db.faces() {
            let path = match &face.source {
                fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => path,
                // We never add binary sources to the database, so there
                // shouln't be any.
                fontdb::Source::Binary(_) => continue,
            };

            let info = db
                .with_face_data(face.id, FontInfo::new)
                .expect("database must contain this font");

            if let Some(info) = info {
                self.book.push(info);
                self.fonts.push(FontSlot {
                    path: path.clone(),
                    index: face.index,
                    font: OnceLock::new(),
                });
            }
        }

        // Embedded fonts have lowest priority.
        self.add_embedded();
    }

    /// Add fonts that are embedded in the binary.
    fn add_embedded(&mut self) {
        for data in typst_assets::fonts() {
            let buffer = typst::foundations::Bytes::new(data);
            for (i, font) in Font::iter(buffer).enumerate() {
                self.book.push(font.info().clone());
                self.fonts.push(FontSlot {
                    path: PathBuf::new(),
                    index: i as u32,
                    font: OnceLock::from(Some(font)),
                });
            }
        }
    }
}
