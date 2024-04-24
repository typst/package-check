use std::{collections::HashSet, path::PathBuf};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use comemo::Track;
use ecow::{eco_format, EcoString};
use tracing::debug;
use typst::{
    diag::{Severity, SourceDiagnostic},
    engine::Route,
    eval::Tracer,
    model::Document,
    syntax::{
        ast::{self, AstNode, Expr, Ident},
        package::{PackageManifest, PackageSpec},
        FileId, Source, Span, SyntaxNode, VirtualPath,
    },
    World, WorldExt,
};

use crate::world::SystemWorld;

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
