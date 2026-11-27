use typst::syntax::ast::{self, AstNode};

use crate::check::Diagnostics;
use crate::check::codes::Code;
use crate::check::diagnostics::{Diagnostic, Docs, Label};
use crate::world::SystemWorld;

/// This should be run after compiling the project, since it checks all loaded
/// file slots of the world.
pub fn check_project(world: &mut SystemWorld, diags: &mut Diagnostics) {
    for src in world.project_sources() {
        check_ast(world, diags, src.root(), false);
    }
}

pub fn check_ast(
    world: &SystemWorld,
    diags: &mut Diagnostics,
    node: &typst::syntax::SyntaxNode,
    // Whether to check function calls (useful when veryfying README examples)
    check_calls: bool,
) {
    for node in node.children() {
        if let Some(binding) = node.cast::<ast::LetBinding>() {
            check_binding(world, diags, binding);
        } else if let Some(closure) = node.cast::<ast::Closure>() {
            check_closure(world, diags, closure);
        } else if let Some(call) = node.cast::<ast::FuncCall>()
            && check_calls
        {
            check_func_call(world, diags, call);
        }

        check_ast(world, diags, node, check_calls);
    }
}

fn check_binding(world: &SystemWorld, diags: &mut Diagnostics, binding: ast::LetBinding) {
    for name in binding.kind().bindings() {
        if !is_recommended_binding_name(name.as_str()) {
            Diagnostic::warn(Code::SrcKebabCase(name.get().clone()))
                .with_message(format!("`{}` is not in kebab-case", name.as_str()))
                .with_label(Label::for_span(world, name.span()))
                .with_docs_link(Docs::CODE_STYLE)
                .emit(diags);
        }
    }
}

fn check_closure(world: &SystemWorld, diags: &mut Diagnostics, closure: ast::Closure) {
    for param in closure.params().children() {
        match param {
            ast::Param::Named(named) => check_params(world, diags, [named.name()]),
            ast::Param::Pos(pattern) => check_params(world, diags, pattern.bindings()),
            ast::Param::Spread(spread) => check_params(world, diags, spread.sink_ident()),
        }
    }
}

fn check_params<'a>(
    world: &SystemWorld,
    diags: &mut Diagnostics,
    names: impl IntoIterator<Item = ast::Ident<'a>>,
) {
    for name in names {
        if !is_recommended_param_name(name.as_str()) {
            Diagnostic::warn(Code::SrcKebabCase(name.get().clone()))
                .with_message(format!("`{}` is not in kebab-case", name.as_str()))
                .with_label(Label::for_span(world, name.span()))
                .with_docs_link(Docs::CODE_STYLE)
                .emit(diags);
        }
    }
}

fn check_func_call(world: &SystemWorld, diags: &mut Diagnostics, call: ast::FuncCall) {
    let func_name = match call.callee() {
        ast::Expr::Ident(i) => Some(i),
        ast::Expr::FieldAccess(f) => Some(f.field()),
        _ => None,
    };

    if let Some(name) = func_name
        && !is_recommended_func_name(name.as_str())
    {
        Diagnostic::warn(Code::SrcKebabCase(name.get().clone()))
            .with_message(format!("`{}` is not in kebab-case", name.as_str()))
            .with_label(Label::for_span(world, name.span()))
            .with_docs_link(Docs::CODE_STYLE)
            .emit(diags);
    }

    for named_arg in call
        .args()
        .items()
        .filter_map(|f| f.to_untyped().cast::<ast::Named>())
    {
        let name = named_arg.name();
        if !is_recommended_param_name(name.as_str()) {
            Diagnostic::warn(Code::SrcKebabCase(name.get().clone()))
                .with_message(format!("`{}` is not in kebab-case", name.as_str()))
                .with_label(Label::for_span(world, name.span()))
                .with_docs_link(Docs::CODE_STYLE)
                .emit(diags);
        }
    }
}

/// Either lower-kebab-case or constants can also use SCREAMING-KEBAB-CASE.
fn is_recommended_binding_name(ident: &str) -> bool {
    is_lower_kebab_case(ident) || is_upper_kebab_case(ident)
}

/// Just lower-kebab-case.
fn is_recommended_func_name(ident: &str) -> bool {
    is_lower_kebab_case(ident)
}

/// Just lower-kebab-case.
fn is_recommended_param_name(ident: &str) -> bool {
    is_lower_kebab_case(ident)
}

pub fn is_lower_kebab_case(ident: &str) -> bool {
    !(ident.starts_with('-') || ident.ends_with('-'))
        && ident
            .chars()
            .all(|c| c != '_' && (c.is_lowercase() || !c.is_uppercase()))
}

pub fn is_upper_kebab_case(ident: &str) -> bool {
    !(ident.starts_with('-') || ident.ends_with('-'))
        && ident
            .chars()
            .all(|c| c != '_' && (!c.is_lowercase() || c.is_uppercase()))
}
