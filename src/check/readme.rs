use std::io::Cursor;
use std::path::Path;
use std::sync::LazyLock;
use std::{collections::HashSet, ops::Range};

use codespan_reporting::diagnostic::{Diagnostic, Label, Severity};
use comrak::nodes::{LineColumn, NodeList, NodeValue as MdNode, Sourcepos};
use html5ever::tendril::TendrilSink;
use regex::Regex;
use typst::{
    foundations::Bytes,
    syntax::{FileId, VirtualPath},
    World,
};

use crate::check::files;
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
            extension: comrak::options::Extension {
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
        let md_node = &*node.data.borrow();
        match &md_node.value {
            // Check that alert blocks are not used
            MdNode::Alert(_) => {
                diags.emit(
                    Diagnostic::warning()
                        .with_code("readme/unsupported-extension/alert")
                        .with_message("GFM alert boxes are not supported on Typst Universe.")
                        .with_labels(vec![Label::primary(
                            readme_fake_file_id(),
                            sourcepos_to_range(&readme, md_node.sourcepos),
                        )]),
                );
            }
            // Also check for checklists
            MdNode::List(NodeList {
                is_task_list: true, ..
            }) => {
                diags.emit(
                    Diagnostic::warning()
                        .with_code("readme/unsupported-extension/tasklist")
                        .with_message("GFM task lists are not supported on Typst Universe.")
                        .with_label(Label::primary(
                            readme_fake_file_id(),
                            sourcepos_to_range(&readme, md_node.sourcepos),
                        )),
                );
            }
            // Basic Typst examples linting
            MdNode::CodeBlock(code_block) => {
                check_readme_code_block(world, diags, code_block, md_node.sourcepos);
            }
            // Check if all links are valid.
            MdNode::Link(link) => {
                check_readme_link_url(world, diags, &readme, md_node.sourcepos, &link.url);
            }
            // Check all image URLs are valid and alt text is specified.
            MdNode::Image(link) => {
                // The alternative description is stored a text child node.
                let mut alt = String::new();
                for child in node.descendants() {
                    if let Some(text) = child.data().value.text() {
                        alt.push_str(text);
                    }
                }
                check_image_alternative_description(diags, &readme, md_node.sourcepos, &alt);

                check_readme_link_url(world, diags, &readme, md_node.sourcepos, &link.url);
            }
            MdNode::HtmlBlock(html) => {
                check_readme_html(world, diags, &readme, md_node.sourcepos, &html.literal);
            }
            MdNode::HtmlInline(html) => {
                check_readme_html(world, diags, &readme, md_node.sourcepos, html);
            }
            _ => (),
        }
    }

    Ok(())
}

fn check_readme_code_block(
    world: &SystemWorld,
    diags: &mut Diagnostics,
    code_block: &comrak::nodes::NodeCodeBlock,
    sourcepos: Sourcepos,
) {
    let lang = code_block.info.to_lowercase();
    if !lang.is_empty() && lang != "typ" && lang != "typst" {
        return;
    }

    let source = world
        .virtual_source(
            readme_fake_file_id(),
            Bytes::from_string(code_block.literal.clone()),
            sourcepos.start.line,
        )
        .unwrap();
    for err in source.root().errors() {
        diags.emit(
            Diagnostic::warning()
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

fn check_readme_html(
    world: &SystemWorld,
    diags: &mut Diagnostics,
    readme: &str,
    sourcepos: Sourcepos,
    html: &str,
) {
    let parser = html5ever::parse_fragment(
        markup5ever_rcdom::RcDom::default(),
        html5ever::ParseOpts::default(),
        html5ever::QualName::new(
            Some(html5ever::Prefix::from("")),
            html5ever::Namespace::from(""),
            html5ever::LocalName::from(""),
        ),
        Vec::new(),
        false,
    );

    let dom = parser
        .from_utf8()
        .read_from(&mut Cursor::new(html.as_bytes()))
        .expect("read_from only returns IO errors, which shouldn't happen for Cursor");

    check_html_elems(world, diags, readme, sourcepos, &dom.document);
}

fn check_html_elems(
    world: &SystemWorld,
    diags: &mut Diagnostics,
    readme: &str,
    sourcepos: Sourcepos,
    node: &markup5ever_rcdom::Node,
) {
    for child in node.children.borrow().iter() {
        if let markup5ever_rcdom::NodeData::Element { name, attrs, .. } = &child.data {
            // Check images
            if &name.local == "img" {
                let attrs = attrs.borrow();

                let alt = attr_value(&attrs, "alt").unwrap_or("");
                check_image_alternative_description(diags, readme, sourcepos, alt);

                if let Some(src) = attr_value(&attrs, "src") {
                    check_readme_link_url(world, diags, readme, sourcepos, src);
                }
            }

            // Check anchor elements.
            if &name.local == "a" {
                let attrs = attrs.borrow();

                if let Some(href) = attr_value(&attrs, "href") {
                    check_readme_link_url(world, diags, readme, sourcepos, href);
                }
            }
        }

        check_html_elems(world, diags, readme, sourcepos, child);
    }

    fn attr_value<'a>(attrs: &'a [html5ever::Attribute], name: &str) -> Option<&'a str> {
        let attr = attrs.iter().find(|a| &a.name.local == name)?;
        Some(&*attr.value)
    }
}

fn check_readme_link_url(
    world: &SystemWorld,
    diags: &mut Diagnostics,
    readme: &str,
    sourcepos: Sourcepos,
    url: &str,
) {
    let url = url.trim();

    if url.contains("://") {
        // TODO: Should we check the URL here like for the `homepage` and
        // `repository` manifest fields?

        check_repo_file_url(diags, readme, sourcepos, url);
    } else if url.starts_with("#") {
        // TODO: Validate markdown anchor.
    } else {
        // Assume this URL is a path of a local file.
        if !files::path_relative_to(world.root(), Path::new(url)).exists() {
            diags.emit(
                Diagnostic::error()
                    .with_code("readme/link/file-not-found")
                    .with_message(format_args!(
                        "Linked file not found: `{url}`.\n\n\
                         Make sure to commit all linked files and possibly add them to the `exclude` list.\n\n\
                         More details: https://github.com/typst/packages/blob/main/docs/tips.md#what-to-commit-what-to-exclude",
                    ))
                    .with_labels(vec![Label::primary(
                        readme_fake_file_id(),
                        sourcepos_to_range(readme, sourcepos),
                    )]),
            );
        }
    }
}

const DEFAULT_BRANCHES: [&str; 2] = ["main", "master"];

fn check_repo_file_url(
    diags: &mut Diagnostics,
    readme: &str,
    sourcepos: Sourcepos,
    url: &str,
) -> Option<()> {
    static GITHUB_URL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"https://github.com/([^/]+)/([^/]+)/(?:blob|tree)/([^/]+)/(.+)").unwrap()
    });
    static GITHUB_RAW_URL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^https://raw.githubusercontent.com/([^/]+)/([^/]+)/(?:refs/heads/)?([^/]+)/(.+)$",
        )
        .unwrap()
    });
    static GITLAB_URL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"https://gitlab.com/([^/]+)/([^/]+)/-/(?:raw|blob|tree)/([^/]+)/(.+)").unwrap()
    });
    static CODEBERG_URL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"https://codeberg.org/([^/]+)/([^/]+)/(?:raw|src)/branch/([^/]+)/(.+)").unwrap()
    });

    enum Host {
        Github,
        Gitlab,
        Codeberg,
    }

    let (host, captures) = if let Some(captures) =
        (GITHUB_URL.captures(url)).or_else(|| GITHUB_RAW_URL.captures(url))
    {
        (Host::Github, captures)
    } else if let Some(captures) = GITLAB_URL.captures(url) {
        (Host::Gitlab, captures)
    } else if let Some(captures) = CODEBERG_URL.captures(url) {
        (Host::Codeberg, captures)
    } else {
        return None;
    };

    let user = captures.get(1).unwrap().as_str();
    let repo = captures.get(2).unwrap().as_str();
    let branch = captures.get(3).unwrap().as_str();
    let path = captures.get(4).unwrap().as_str();

    if !DEFAULT_BRANCHES.contains(&branch) {
        return None;
    }

    let name = match host {
        Host::Github => "GitHub",
        Host::Gitlab => "Gitlab",
        Host::Codeberg => "Codeberg",
    };

    let non_raw_url = match host {
        Host::Github => format!("https://github.com/{user}/{repo}/blob/{branch}/{path}"),
        Host::Gitlab => format!("https://gitlab.com/{user}/{repo}/-/blob/{branch}/{path}"),
        Host::Codeberg => format!("https://codeberg.org/{user}/{repo}/src/branch/{branch}/{path}"),
    };

    diags.emit(
        Diagnostic::warning()
            .with_code("readme/link/github-url-permalink")
            .with_message(format_args!(
                "{name} URL links to default branch: `{url}`.\n\n\
                 Consider using a link to a specific tag/release or a permalink to a commit instead. \
                 This will ensure that the linked resource always matches this version of the package.\n\n\
                 You can create a permalink here: {non_raw_url}\n\n\
                 Alternatively you can also link to a local file. This is preferred if the linked file \
                 is already present in the submitted package."
            ))
            .with_label(Label::primary(
                readme_fake_file_id(),
                sourcepos_to_range(readme, sourcepos),
            )),
    );

    Some(())
}

fn check_image_alternative_description(
    diags: &mut Diagnostics,
    readme: &str,
    sourcepos: Sourcepos,
    alt: &str,
) {
    // Don't use `\w` because that would also include Chinese or similar
    // character categories, which don't use whitespace to separate words.
    static SINGLE_WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{Cased_Letter}\p{Number}\-_]+\w$").unwrap());
    static SINGLE_LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w$").unwrap());

    let alt = alt.trim();

    if alt.is_empty() || SINGLE_WORD.is_match(alt) || SINGLE_LETTER.is_match(alt) {
        let (severity, message) = if alt.is_empty() {
            let message = String::from(
                "Missing alternative description for image. \
                 Please add a short description to make this image more accessible.",
            );
            (Severity::Error, message)
        } else {
            let message = format!(
                "Possibly inadequate alternative description for image: `{alt}`. \
                 Please add a short description to make this image more accessible.",
            );
            (Severity::Warning, message)
        };
        diags.emit(
            Diagnostic::new(severity)
                .with_code("readme/image/missing-alt")
                .with_message(message)
                .with_label(Label::primary(
                    readme_fake_file_id(),
                    sourcepos_to_range(readme, sourcepos),
                )),
        );
    }
}

fn sourcepos_to_range(s: &str, pos: Sourcepos) -> Range<usize> {
    fn byte_offset(s: &str, pos: LineColumn) -> usize {
        // `LineColumn` uses 1-indexed line numbers.
        let line_offset = s
            .split_inclusive('\n')
            .take(pos.line - 1)
            .map(str::len)
            .sum::<usize>();

        line_offset + pos.column
    }

    // `LineColumn::column` is 1-indexed.
    let start = byte_offset(s, pos.start) - 1;
    // `Sourcepos::end` is end-inclusive (byte-wise), and thus `offset + 1 - 1`.
    let end = byte_offset(s, pos.end);

    start..end
}

/// It is important to create a fake ID for each node, this allows to
/// have multiple sources with the same file path, which is useful here
/// since the README can contain multiple Typst example blocks.
fn readme_fake_file_id() -> FileId {
    FileId::new_fake(VirtualPath::new("README.md"))
}
