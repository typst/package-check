//! Typst World.
//!
//! Most of this module is copied from typst-cli.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use chrono::{DateTime, Datelike, FixedOffset, Local, Utc};
use comemo::Prehashed;
use ecow::{eco_format, EcoString};
use fontdb::Database;
use ignore::overrides::Override;
use parking_lot::Mutex;
use reqwest::StatusCode;
use typst::{
    diag::{FileError, FileResult, PackageError, PackageResult},
    foundations::{Bytes, Datetime},
    syntax::{package::PackageSpec, FileId, Source, VirtualPath},
    text::{Font, FontBook, FontInfo},
    Library, World,
};

/// A world that provides access to the operating system.
pub struct SystemWorld {
    /// The working directory.
    workdir: Option<PathBuf>,
    /// The root relative to which absolute paths are resolved.
    root: PathBuf,
    /// The input path.
    main: FileId,
    /// Typst's standard library.
    library: Prehashed<Library>,
    /// Metadata about discovered fonts.
    book: Prehashed<FontBook>,
    /// Locations of and storage for lazily loaded fonts.
    fonts: Vec<FontSlot>,
    /// Maps file ids to source files and buffers.
    slots: Mutex<HashMap<FileId, FileSlot>>,
    /// The current datetime if requested. This is stored here to ensure it is
    /// always the same within one compilation.
    /// Reset between compilations if not [`Now::Fixed`].
    now: OnceLock<DateTime<Utc>>,
    /// Override for package resolution
    package_override: Option<(PackageSpec, PathBuf)>,
    /// Files that are considered excluded and should not be read from.
    excluded: Override,
}

impl SystemWorld {
    /// Create a new system world.
    pub fn new(input: PathBuf, root: PathBuf) -> Result<Self, WorldCreationError> {
        // Resolve the virtual path of the main file within the project root.
        let main_path =
            VirtualPath::within_root(&input, &root).ok_or(WorldCreationError::InputOutsideRoot)?;
        let main = FileId::new(None, main_path);

        let library = Library::default();

        let mut searcher = FontSearcher::new();
        searcher.search(&[]);

        Ok(Self {
            workdir: std::env::current_dir().ok(),
            root,
            main,
            library: Prehashed::new(library),
            book: Prehashed::new(searcher.book),
            fonts: searcher.fonts,
            slots: Mutex::new(HashMap::new()),
            now: OnceLock::new(),
            package_override: None,
            excluded: Override::empty(),
        })
    }

    pub fn with_package_override(mut self, spec: &PackageSpec, dir: &Path) -> Self {
        self.package_override = Some((spec.clone(), dir.to_owned()));
        self
    }

    /// The root relative to which absolute paths are resolved.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The current working directory.
    pub fn workdir(&self) -> &Path {
        self.workdir.as_deref().unwrap_or(Path::new("."))
    }

    /// Lookup a source file by id.
    #[track_caller]
    pub fn lookup(&self, id: FileId) -> FileResult<Source> {
        self.source(id)
    }

    pub fn exclude(&mut self, globs: Override) {
        self.excluded = globs;
    }
}

impl World for SystemWorld {
    fn library(&self) -> &Prehashed<Library> {
        &self.library
    }

    fn book(&self) -> &Prehashed<FontBook> {
        &self.book
    }

    fn main(&self) -> Source {
        self.source(self.main).unwrap()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.slot(id, |slot| {
            slot.source(&self.root, &self.package_override, &self.excluded)
        })
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.slot(id, |slot| {
            slot.file(&self.root, &self.package_override, &self.excluded)
        })
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts[index].get()
    }

    fn today(&self, offset: Option<i64>) -> Option<Datetime> {
        let now = self.now.get_or_init(Utc::now);

        // The time with the specified UTC offset, or within the local time zone.
        let with_offset = match offset {
            None => now.with_timezone(&Local).fixed_offset(),
            Some(hours) => {
                let seconds = i32::try_from(hours).ok()?.checked_mul(3600)?;
                now.with_timezone(&FixedOffset::east_opt(seconds)?)
            }
        };

        Datetime::from_ymd(
            with_offset.year(),
            with_offset.month().try_into().ok()?,
            with_offset.day().try_into().ok()?,
        )
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
}

impl FileSlot {
    /// Create a new file slot.
    fn new(id: FileId) -> Self {
        Self {
            id,
            file: SlotCell::new(),
            source: SlotCell::new(),
        }
    }

    /// Retrieve the source for this file.
    fn source(
        &mut self,
        project_root: &Path,
        package_override: &Option<(PackageSpec, PathBuf)>,
        excluded: &Override,
    ) -> FileResult<Source> {
        self.source.get_or_init(
            || read(self.id, project_root, package_override, excluded),
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
        project_root: &Path,
        package_override: &Option<(PackageSpec, PathBuf)>,
        excluded: &Override,
    ) -> FileResult<Bytes> {
        self.file.get_or_init(
            || read(self.id, project_root, package_override, excluded),
            |data, _| Ok(data.into()),
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
        if std::mem::replace(&mut self.accessed, true) {
            if let Some(data) = &self.data {
                return data.clone();
            }
        }

        // Read and hash the file.
        let result = load();
        let fingerprint = typst::util::hash128(&result);

        // If the file contents didn't change, yield the old processed data.
        if std::mem::replace(&mut self.fingerprint, fingerprint) == fingerprint {
            if let Some(data) = &self.data {
                return data.clone();
            }
        }

        let prev = self.data.take().and_then(Result::ok);
        let value = result.and_then(|data| f(data, prev));
        self.data = Some(value.clone());

        value
    }
}

/// Resolves the path of a file id on the system, downloading a package if
/// necessary.
fn system_path(
    package_override: &Option<(PackageSpec, PathBuf)>,
    project_root: &Path,
    excluded: &Override,
    id: FileId,
) -> FileResult<PathBuf> {
    let exclude = |file: FileResult<PathBuf>| match file {
        Ok(f) => {
            if let Ok(path_in_package) = f.strip_prefix(project_root) {
                if excluded.matched(path_in_package, false).is_ignore() {
                    return Err(FileError::Other(Some(
                        "This file exists but is excluded from your package.".into(),
                    )));
                }
            }

            Ok(f)
        }
        err => err,
    };

    // Determine the root path relative to which the file path
    // will be resolved.
    let root = if let Some(spec) = id.package() {
        if let Some(package_override) = package_override {
            if *spec == package_override.0 {
                return exclude(
                    id.vpath()
                        .resolve(&package_override.1)
                        .ok_or(FileError::AccessDenied),
                );
            }
        }

        expect_parents(
            project_root,
            &[&spec.version.to_string(), &spec.name, &spec.namespace],
        )
        .map(|package_root| {
            Ok(package_root
                .join(spec.namespace.as_str())
                .join(spec.name.as_str())
                .join(spec.version.to_string()))
        })
        .unwrap_or_else(|| prepare_package(spec))
        .unwrap()
    } else {
        project_root.to_owned()
    };
    exclude(id.vpath().resolve(&root).ok_or(FileError::AccessDenied))
}

// Goes up in a file system hierarchy while the parent folder matches the expected name
fn expect_parents<'a>(dir: &'a Path, parents: &'a [&'a str]) -> Option<PathBuf> {
    let dir = dir.canonicalize().ok()?;

    if parents.is_empty() {
        return Some(dir);
    }

    let (expected_parent, rest) = parents.split_first()?;
    let parent = dir.parent()?;
    if parent.file_name().and_then(|n| n.to_str()) != Some(expected_parent) {
        return None;
    }

    expect_parents(parent, rest)
}

/// Reads a file from a `FileId`.
///
/// If the ID represents stdin it will read from standard input,
/// otherwise it gets the file path of the ID and reads the file from disk.
fn read(
    id: FileId,
    project_root: &Path,
    package_override: &Option<(PackageSpec, PathBuf)>,
    excluded: &Override,
) -> FileResult<Vec<u8>> {
    read_from_disk(&system_path(package_override, project_root, excluded, id)?)
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
/// An error that occurs during world construction.
#[derive(Debug)]
pub enum WorldCreationError {
    /// The input file does not appear to exist.
    InputNotFound(PathBuf),
    /// The input file is not contained within the root folder.
    InputOutsideRoot,
    /// The root directory does not appear to exist.
    RootNotFound(PathBuf),
    /// Another type of I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for WorldCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldCreationError::InputNotFound(path) => {
                write!(f, "input file not found (searched at {})", path.display())
            }
            WorldCreationError::InputOutsideRoot => {
                write!(f, "source file must be contained in project root")
            }
            WorldCreationError::RootNotFound(path) => {
                write!(
                    f,
                    "root directory not found (searched at {})",
                    path.display()
                )
            }
            WorldCreationError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl From<WorldCreationError> for EcoString {
    fn from(err: WorldCreationError) -> Self {
        eco_format!("{err}")
    }
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
                let data = std::fs::read(&self.path).ok()?.into();
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
            let buffer = typst::foundations::Bytes::from_static(data);
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

// Package fetching, adapted from typst-cli

const HOST: &str = "https://packages.typst.org";

/// Make a package available in the on-disk cache.
pub fn prepare_package(spec: &PackageSpec) -> PackageResult<PathBuf> {
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

        // Download from network if it doesn't exist yet.
        if spec.namespace == "preview" {
            download_package(spec, &dir)?;
            if dir.exists() {
                return Ok(dir);
            }
        }
    }

    Err(PackageError::NotFound(spec.clone()))
}

/// Download a package over the network.
fn download_package(spec: &PackageSpec, package_dir: &Path) -> PackageResult<()> {
    // The `@preview` namespace is the only namespace that supports on-demand
    // fetching.
    assert_eq!(spec.namespace, "preview");

    let url = format!("{HOST}/preview/{}-{}.tar.gz", spec.name, spec.version);

    let data = match download(&url) {
        Ok(data) => data,
        Err(err) if err.status() == Some(StatusCode::NOT_FOUND) => {
            return Err(PackageError::NotFound(spec.clone()))
        }
        Err(err) => return Err(PackageError::NetworkFailed(Some(eco_format!("{err}")))),
    };

    let body = data.bytes().unwrap();
    let decompressed = flate2::read::GzDecoder::new(&body[..]);
    tar::Archive::new(decompressed)
        .unpack(package_dir)
        .map_err(|err| {
            std::fs::remove_dir_all(package_dir).ok();
            PackageError::MalformedArchive(Some(eco_format!("{err}")))
        })
}

/// Download from a URL.
pub fn download(url: &str) -> reqwest::Result<reqwest::blocking::Response> {
    let client = reqwest::blocking::Client::new();
    let req = client
        .get(url)
        .header("User-Agent", concat!("typst/", env!("CARGO_PKG_VERSION")));
    req.send()
}
