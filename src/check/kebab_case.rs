use std::collections::HashSet;

use comemo::Track;
use typst::syntax::{RootedPath, VirtualRoot};
use typst::{
    World,
    engine::{Route, Sink, Traced},
    syntax::{
        FileId, Source, SyntaxNode,
        ast::{self, AstNode},
    },
};

use crate::check::diagnostics::{Code, Diagnostic, Label};
use crate::world::SystemWorld;

use super::Diagnostics;

// Check that all public identifiers are in kebab-case
pub fn check(diags: &mut Diagnostics, world: &SystemWorld) -> Option<()> {
    let main = world.source(world.main()).ok()?;

    let public_names: HashSet<_> = {
        let world = <dyn World>::track(world);

        let mut sink = Sink::new();
        let module = typst_eval::eval(
            world,
            world.library(),
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

    check_ast(world, diags, public_names, src.root(), false);

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
            ast::Expr::Str(s) => {
                if let Ok(path) = src.id().vpath().join(s.get().as_str()) {
                    path
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        let fid = FileId::new(RootedPath::new(VirtualRoot::Project, file_path));
        let Ok(source) = world.source(fid) else {
            continue;
        };

        check_source(source, world, public_names, diags, visited);
    }

    Some(())
}

pub fn check_ast(
    world: &SystemWorld,
    diags: &mut Diagnostics,
    public_names: &HashSet<String>,
    root: &typst::syntax::SyntaxNode,
    // Whether to check function calls (useful when veryfying README examples)
    check_calls: bool,
) {
    // Check all let bindings
    for binding in root.children().filter_map(|c| c.cast::<ast::LetBinding>()) {
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
            Diagnostic::warn(Code::KebabCaseValue)
                .with_message(
                    "This value seems to be public. \
                         It is recommended to use kebab-case names.",
                )
                .with_label(Label::for_span(world, name_ident.span()))
                .emit(diags);
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

                // We recommend kebab-style names but do not warn on
                // all-uppercase names that may represent real-world
                // acronyms.
                if name != casbab::kebab(name) && name != casbab::screaming(name) {
                    Diagnostic::warn(Code::KebabCaseParameter)
                        .with_message(
                            "This argument seems to be part of public function. \
                                 It is recommended to use kebab-case names.",
                        )
                        .with_label(Label::for_span(world, span))
                        .emit(diags);
                }
            }
        }
    }

    if check_calls {
        for call in root.children().filter_map(|c| c.cast::<ast::FuncCall>()) {
            let func_name = match call.callee() {
                ast::Expr::Ident(i) => Some(i),
                ast::Expr::FieldAccess(f) => Some(f.field()),
                _ => None,
            };

            if let Some(func_name) = func_name {
                let func_name = func_name.as_str();
                if func_name != casbab::kebab(func_name) {
                    Diagnostic::warn(Code::KebabCaseValue)
                        .with_message("This function should have a kebab-case name.")
                        .with_label(Label::for_span(world, call.span()))
                        .emit(diags);
                }
            }

            for named_arg in call
                .args()
                .items()
                .filter_map(|f| f.to_untyped().cast::<ast::Named>())
            {
                let arg_name = named_arg.name().as_str();
                if arg_name != casbab::kebab(arg_name) {
                    Diagnostic::warn(Code::KebabCaseParameter)
                        .with_message("This argument should have a kebab-case name.")
                        .with_label(Label::for_span(world, named_arg.name().span()))
                        .emit(diags);
                }
            }
        }
    }
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
