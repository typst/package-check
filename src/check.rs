use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use chrono::{DateTime, Datelike, FixedOffset, Local, Utc};
use codespan_reporting::diagnostic::{Diagnostic, Label};
use comemo::{Prehashed, Track};
use ecow::{eco_format, EcoString};
use fontdb::Database;
use parking_lot::Mutex;
use tracing::debug;
use typst::{
    diag::{FileError, FileResult, Severity, SourceDiagnostic},
    engine::Route,
    eval::Tracer,
    foundations::{Bytes, Datetime},
    model::Document,
    syntax::{
        ast::{self, AstNode, Expr, Ident},
        package::{PackageManifest, PackageSpec},
        FileId, Source, Span, SyntaxNode, VirtualPath,
    },
    text::{Font, FontBook, FontInfo},
    Library, World, WorldExt,
};

#[derive(Default)]
pub struct Diagnostics {
    pub warnings: Vec<Diagnostic<FileId>>,
    pub errors: Vec<Diagnostic<FileId>>,
}

pub fn all_checks(
    packages_root: PathBuf,
    package_spec: &PackageSpec,
) -> (SystemWorld, Diagnostics) {
    let mut diags = Diagnostics::default();
    let (_, _, world) = check_manifest(packages_root, &mut diags, package_spec);
    check_compile(&mut diags, &world);
    check_kebab_case(&mut diags, &world);

    (world, diags)
}

fn check_manifest(
    packages_root: PathBuf,
    diags: &mut Diagnostics,
    package_spec: &PackageSpec,
) -> (PathBuf, PackageManifest, SystemWorld) {
    let package_dir = packages_root
        .join(package_spec.namespace.to_string())
        .join(package_spec.name.to_string())
        .join(package_spec.version.to_string());
    let manifest_path = package_dir.join("typst.toml");
    debug!("Reading manifest at {}", &manifest_path.display());
    let manifest_contents = std::fs::read_to_string(manifest_path).unwrap();
    let manifest: PackageManifest = toml::from_str(&manifest_contents).unwrap();

    let entrypoint = package_dir.join(manifest.package.entrypoint.to_string());
    let world = SystemWorld::new(entrypoint, package_dir.clone())
        .map_err(|err| eco_format!("{err}"))
        .unwrap();

    let manifest_file_id = FileId::new(None, VirtualPath::new("typst.toml"));
    world.file(manifest_file_id).ok(); // TODO: is this really necessary?

    let mut manifest_lines = manifest_contents.lines().scan(0, |start, line| {
        let end = *start + line.len();
        let range = *start..end;
        *start = end + 1;
        Some((line, range))
    });
    let name_line = manifest_lines
        .clone()
        .find(|(l, _)| l.trim().starts_with("name ="));
    let version_line = manifest_lines.find(|(l, _)| l.trim().starts_with("version ="));

    if manifest.package.name != package_spec.name {
        diags.errors.push(
            Diagnostic::error()
                .with_labels(vec![Label::new(
                    codespan_reporting::diagnostic::LabelStyle::Primary,
                    manifest_file_id,
                    name_line.map(|l| l.1).unwrap_or_default(),
                )])
                .with_message(format!(
                    "Unexpected package name. `{name}` was expected. If you want to publish a new package, create a new directory in `packages/{namespace}/`.",
                    name = package_spec.name,
                    namespace = package_spec.namespace,
                )),
        )
    }

    if manifest.package.version != package_spec.version {
        diags.errors.push(
            Diagnostic::error()
                .with_labels(vec![Label::new(
                    codespan_reporting::diagnostic::LabelStyle::Primary,
                    manifest_file_id,
                    version_line.map(|l| l.1).unwrap_or_default(),
                )])
                .with_message(format!(
                    "Unexpected version number. `{version}` was expected. If you want to publish a new version, create a new directory in `packages/{namespace}/{name}`.",
                    version = package_spec.version,
                    name = package_spec.name,
                    namespace = package_spec.namespace,
                )),
        )
    }
    // TODO: other common checks

    (package_dir, manifest, world)
}

fn check_compile(diags: &mut Diagnostics, world: &SystemWorld) -> Option<Document> {
    let mut tracer = Tracer::new();
    let result = typst::compile(world, &mut tracer);
    diags
        .warnings
        .extend(convert_diagnostics(&world, tracer.warnings()));

    match result {
        Ok(doc) => Some(doc),
        Err(errors) => {
            diags.errors.extend(convert_diagnostics(&world, errors));
            None
        }
    }
}

// Check that all public identifiers are in kebab-case
// TODO: what about constants? Should MY_VALUE be MY-VALUE?
fn check_kebab_case(diags: &mut Diagnostics, world: &SystemWorld) -> Option<()> {
    let public_names: HashSet<_> = {
        let world = <dyn World>::track(world);
        let mut tracer = Tracer::new();

        let module = typst::eval::eval(
            world,
            Route::default().track(),
            tracer.track_mut(),
            &world.main(),
        )
        .ok()?;
        let scope = module.scope();
        scope.iter().map(|(name, _)| name.clone()).collect()
    };

    fn check_source(
        src: Source,
        world: &SystemWorld,
        public_names: &HashSet<EcoString>,
        diags: &mut Diagnostics,
        visited: &mut HashSet<FileId>,
    ) -> Option<()> {
        if visited.contains(&src.id()) {
            return Some(());
        }
        visited.insert(src.id());

        // Check all let bindings
        for binding in src
            .root()
            .children()
            .filter_map(|c| c.cast::<ast::LetBinding>())
        {
            let Some(name) = find_first::<Ident>(binding.to_untyped()) else {
                continue;
            };

            if !public_names.contains(name.get()) {
                continue;
            }

            if name.as_str() != casbab::kebab(name.as_str()) {
                diags.warnings.push(Diagnostic {
                    severity: codespan_reporting::diagnostic::Severity::Warning,
                    message:
                        "This value seems to be public. It is recommended to use kebab-case names."
                            .to_owned(),
                    labels: label(world, name.span()).into_iter().collect(),
                    notes: Vec::new(),
                    code: None,
                })
            }

            if let Some(Expr::Closure(func)) = binding.init() {
                for param in func.params().children() {
                    let (name, span) = match param {
                        ast::Param::Named(named) => (named.name().as_str(), named.span()),
                        ast::Param::Spread(spread) => {
                            let Some(sink) = spread.sink_ident() else {
                                continue;
                            };
                            (sink.as_str(), sink.span())
                        }
                        ast::Param::Pos(ast::Pattern::Normal(Expr::Ident(i))) => {
                            (i.as_str(), i.span())
                        }
                        _ => continue,
                    };

                    if name != casbab::kebab(name) {
                        diags.warnings.push(Diagnostic {
                            severity: codespan_reporting::diagnostic::Severity::Warning,
                            message:
                                "This argument seems to be part of public function. It is recommended to use kebab-case names."
                                    .to_owned(),
                            labels: label(world, span).into_iter().collect(),
                            notes: Vec::new(),
                            code: None,
                        })
                    }
                }
            }
        }

        // Check imported files recursively.
        //
        // Because we evaluated the module above, we know that no cyclic import
        // will occur. `visited` still exist because some modules may be imported
        // multiple times.
        //
        // Only imports at the root will be checked, as this is the most common
        // case anyway.
        for import in src
            .root()
            .children()
            .filter_map(|c| c.cast::<ast::ModuleImport>())
        {
            let file_path = match import.source() {
                Expr::Str(s) => src.id().vpath().join(s.get().as_str()),
                _ => continue,
            };
            let fid = FileId::new(None, file_path);
            let Ok(source) = world.source(fid) else {
                continue;
            };

            check_source(source, world, public_names, diags, visited);
        }

        Some(())
    }

    let main = world.main();
    let mut visited = HashSet::new();
    check_source(main, world, &public_names, diags, &mut visited);

    Some(())
}

fn convert_diagnostics<'a>(
    world: &'a SystemWorld,
    iter: impl IntoIterator<Item = SourceDiagnostic> + 'a,
) -> impl Iterator<Item = Diagnostic<FileId>> + 'a {
    iter.into_iter().map(|diagnostic| {
        match diagnostic.severity {
            Severity::Error => Diagnostic::error(),
            Severity::Warning => Diagnostic::warning(),
        }
        .with_message(format!(
            "The following error was reported by the Typst compiler: {}",
            diagnostic.message
        ))
        .with_labels(label(world, diagnostic.span).into_iter().collect())
    })
}

/// Create a label for a span.
fn label(world: &SystemWorld, span: Span) -> Option<Label<FileId>> {
    Some(Label::primary(span.id()?, world.range(span)?))
}

/// Find the first child of a given type in a syntax tree
fn find_first<'a, T: AstNode<'a>>(node: &'a SyntaxNode) -> Option<T> {
    for ch in node.children() {
        if let Some(cast) = ch.cast() {
            return Some(cast);
        }

        if let Some(x) = find_first(ch) {
            return Some(x);
        }
    }
    None
}

// Copied from typst-cli, should probably be moved to its own module and
// simplified.

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
        })
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
    pub fn lookup(&self, id: FileId) -> Source {
        self.source(id)
            .expect("file id does not point to any source file")
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
        self.slot(id, |slot| slot.source(&self.root))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.slot(id, |slot| slot.file(&self.root))
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
    fn source(&mut self, project_root: &Path) -> FileResult<Source> {
        self.source.get_or_init(
            || read(self.id, project_root),
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
    fn file(&mut self, project_root: &Path) -> FileResult<Bytes> {
        self.file
            .get_or_init(|| read(self.id, project_root), |data, _| Ok(data.into()))
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
fn system_path(project_root: &Path, id: FileId) -> FileResult<PathBuf> {
    // Determine the root path relative to which the file path
    // will be resolved.
    let root = if let Some(spec) = id.package() {
        project_root // version folder
            .parent() // package folder
            .unwrap()
            .parent() // namespace folder
            .unwrap()
            .parent() // root folder
            .unwrap()
            .join(spec.namespace.as_str())
            .join(spec.name.as_str())
            .join(spec.version.to_string())
    } else {
        project_root.to_owned()
    };
    id.vpath().resolve(&root).ok_or(FileError::AccessDenied)
}

/// Reads a file from a `FileId`.
///
/// If the ID represents stdin it will read from standard input,
/// otherwise it gets the file path of the ID and reads the file from disk.
fn read(id: FileId, project_root: &Path) -> FileResult<Vec<u8>> {
    read_from_disk(&system_path(project_root, id)?)
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
