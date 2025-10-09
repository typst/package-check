use std::{collections::HashSet, ops::Range};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use comrak::nodes::{Ast as MdAst, NodeList, NodeValue as MdNode, Sourcepos};
use typst::{
    foundations::Bytes,
    syntax::{FileId, VirtualPath},
    World,
};

use crate::{
    check::{imports, kebab_case, label, Diagnostics, TryExt},
    world::SystemWorld,
};

pub async fn check_readme(
    world: &SystemWorld,
    diags: &mut Diagnostics,
) -> crate::check::Result<()> {
    // check syntax, versions and kebab-case
    // warn on unsupported gfm features
    let readme = tokio::fs::read_to_string(world.root().join("README.md"))
        .await
        .error("io/readme", "Failed to read README.md")?;

    let arena = comrak::Arena::new();
    let md_ast = comrak::parse_document(
        &arena,
        &readme,
        &comrak::Options {
            // Try to be faithful to the Universe parser
            extension: comrak::ExtensionOptions {
                strikethrough: true,
                tagfilter: true,
                table: true,
                header_ids: Some(String::new()),
                footnotes: true,
                // The following extensions are not enabled on Universe, but
                // their usage is common in READMEs and we want to warn about
                // their usage.
                tasklist: true,
                alerts: true,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    for node in md_ast.descendants() {
        // It is important to create a fake ID for each node, this allows to
        // have multiple sources with the same file path, which is usefull here
        // since the README can contain multiple Typst example blocks.
        let readme_id = FileId::new_fake(VirtualPath::new("README.md"));

        // Check that alert blocks are not used
        if let MdAst {
            sourcepos,
            value: MdNode::Alert(_),
            ..
        } = *node.data.borrow()
        {
            diags.emit(
                Diagnostic::warning()
                    .with_code("readme/unsupported-extension/alert")
                    .with_message("GFM alert boxes are not supported on Typst Universe.")
                    .with_labels(vec![Label::primary(
                        readme_id,
                        sourcepos_to_range(&readme, &sourcepos),
                    )]),
            );
            continue;
        }

        // Also check for checklists
        if let MdAst {
            sourcepos,
            value: MdNode::List(NodeList {
                is_task_list: true, ..
            }),
            ..
        } = *node.data.borrow()
        {
            diags.emit(
                Diagnostic::warning()
                    .with_code("readme/unsupported-extension/tasklist")
                    .with_message("GFM task lists are not supported on Typst Universe.")
                    .with_labels(vec![Label::primary(
                        readme_id,
                        sourcepos_to_range(&readme, &sourcepos),
                    )]),
            );
            continue;
        }

        // Basic Typst examples linting
        let comrak::nodes::NodeValue::CodeBlock(ref code_block) = node.data.borrow().value else {
            continue;
        };

        let lang = code_block.info.to_lowercase();
        if !lang.is_empty() && lang != "typ" && lang != "typst" {
            continue;
        }

        let source = world
            .virtual_source(
                readme_id,
                Bytes::from_string(code_block.literal.clone()),
                node.data.borrow().sourcepos.start.line,
            )
            .unwrap();
        for err in source.root().errors() {
            diags.emit(
                Diagnostic::error()
                    .with_code("readme/syntax")
                    .with_message(format!(
                        "Syntax error in README.\n\n  {}\n\n\
                        If this code block is not supposed to be parsed as a Typst source, \
                        please explicitely specify another language.",
                        err.message
                    ))
                    .with_labels(label(world, err.span).into_iter().collect()),
            );
        }

        kebab_case::check_ast(world, diags, &HashSet::new(), source.root(), true);

        let main_path = world
            .root()
            .join(world.main().vpath().as_rootless_path())
            .canonicalize()
            .ok();
        let all_packages = world
            .root()
            .parent()
            .and_then(|package_dir| package_dir.parent())
            .and_then(|namespace_dir| namespace_dir.parent());
        imports::check_ast(
            diags,
            world,
            source.root(),
            &world.root().join("README.md"),
            main_path.as_deref(),
            all_packages,
        );
    }

    Ok(())
}

fn sourcepos_to_range(s: &str, pos: &Sourcepos) -> Range<usize> {
    fn line_start(s: &str, line: usize) -> usize {
        let mut offset = 0;
        // Sourcepos::line is one-indexed, enumerate is zero-indexed
        let line = line - 1;

        for (i, line_len) in s.lines().map(str::len).enumerate() {
            if i == line {
                return offset;
            }

            // Extra character for the \n
            offset += line_len + 1;
        }

        offset
    }

    let start = line_start(s, pos.start.line) + pos.start.column - 1;
    let end = line_start(s, pos.end.line) + pos.end.column - 1;
    start..end
}
