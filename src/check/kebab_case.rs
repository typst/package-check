use std::collections::HashSet;

use codespan_reporting::diagnostic::{Diagnostic, Severity};
use comemo::Track;
use typst::{
    engine::{Route, Sink, Traced},
    syntax::{
        ast::{self, AstNode},
        FileId, Source, SyntaxNode,
    },
    World, ROUTINES,
};

use crate::world::SystemWorld;

use super::{label, Diagnostics};

// Check that all public identifiers are in kebab-case
pub fn check(diags: &mut Diagnostics, world: &SystemWorld) -> Option<()> {
    let main = world.source(world.main()).ok()?;

    let public_names: HashSet<_> = {
        let world = <dyn World>::track(world);

        let mut sink = Sink::new();
        let module = typst_eval::eval(
            &ROUTINES,
            world,
            Traced::default().track(),
            sink.track_mut(),
            Route::default().track(),
            &main,
        )
        .ok()?;
        let scope = module.scope();
        scope.iter().map(|(name, _)| name.to_string()).collect()
    };

    let mut visited = HashSet::new();
    check_source(main, world, &public_names, diags, &mut visited);

    Some(())
}

/// Run the check for a single source file.
fn check_source(
    src: Source,
    world: &SystemWorld,
    public_names: &HashSet<String>,
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
        let Some(name_ident) = find_first::<ast::Ident>(binding.to_untyped()) else {
            continue;
        };

        if !public_names.contains(name_ident.get().as_str()) {
            continue;
        }

        let name = &name_ident.as_str();
        if name.starts_with('_') {
            // This is exported but considered private.
            continue;
        }

        if name == &casbab::screaming_snake(name) || name == &casbab::screaming_kebab(name) {
            // Constants can use SCREAMING_SNAKE_CASE or SCREAMING-KEBAB-CASE
            continue;
        }

        if name != &casbab::kebab(name) {
            diags.emit(Diagnostic {
                severity: codespan_reporting::diagnostic::Severity::Warning,
                message:
                    "This value seems to be public. It is recommended to use kebab-case names."
                        .to_owned(),
                labels: label(world, name_ident.span()).into_iter().collect(),
                notes: Vec::new(),
                code: None,
            })
        }

        if let Some(ast::Expr::Closure(func)) = binding.init() {
            for param in func.params().children() {
                let (name, span) = match param {
                    ast::Param::Named(named) => (named.name().as_str(), named.span()),
                    ast::Param::Pos(ast::Pattern::Normal(ast::Expr::Ident(i))) => {
                        (i.as_str(), i.span())
                    }
                    // Spread params can safely be ignored, their name is only
                    // exposed to the body of the function, not the caller.
                    _ => continue,
                };

                if name != casbab::kebab(name) {
                    diags.emit(Diagnostic {
                        severity: Severity::Warning,
                        message: "This argument seems to be part of public function. \
                            It is recommended to use kebab-case names."
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
    // Because we evaluated the module above, we know that no cyclic import will
    // occur. `visited` still exist because some modules may be imported
    // multiple times.
    //
    // Only imports at the root of the AST will be checked, as this is the most
    // common case anyway.
    for import in src
        .root()
        .children()
        .filter_map(|c| c.cast::<ast::ModuleImport>())
    {
        let file_path = match import.source() {
            ast::Expr::Str(s) => src.id().vpath().join(s.get().as_str()),
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
