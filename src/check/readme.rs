use std::io::Cursor;
use std::sync::LazyLock;
use std::{collections::HashSet, ops::Range};

use codespan_reporting::diagnostic::{Diagnostic, Label};
use comrak::nodes::{LineColumn, NodeList, NodeValue as MdNode, Sourcepos};
use html5ever::tendril::TendrilSink;
use regex::Regex;
use typst::{
    foundations::Bytes,
    syntax::{FileId, VirtualPath},
    World,
};
use url::Url;

use crate::check::path::PackagePath;
use crate::{
    check::{imports, kebab_case, label, Diagnostics, TryExt},
    world::SystemWorld,
};

#[derive(Default)]
pub struct Readme {
    pub text: String,
    pub linked_files: Vec<PackagePath>,
}

pub async fn check(world: &SystemWorld, diags: &mut Diagnostics) -> crate::check::Result<Readme> {
    // check syntax, versions and kebab-case
    // warn on unsupported gfm features
    let text = tokio::fs::read_to_string(world.root().join("README.md"))
        .await
        .error("io/readme", "Failed to read README.md")?;

    let arena = comrak::Arena::new();
    let md_ast = comrak::parse_document(
        &arena,
        &text,
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

    let mut readme = Readme {
        text,
        linked_files: Vec::new(),
    };

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
                            readme_file_id(),
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
                            readme_file_id(),
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
                check_readme_link_url(world, diags, &mut readme, md_node.sourcepos, &link.url);
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

                check_readme_link_url(world, diags, &mut readme, md_node.sourcepos, &link.url);
            }
            MdNode::HtmlBlock(html) => {
                check_readme_html(world, diags, &mut readme, md_node.sourcepos, &html.literal);
            }
            MdNode::HtmlInline(html) => {
                check_readme_html(world, diags, &mut readme, md_node.sourcepos, html);
            }
            _ => (),
        }
    }

    Ok(readme)
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

    // It is important to create a fake ID for each markdown code block. This
    // allows having multiple sources with the same file path, since the README
    // can contain multiple Typst example blocks.
    let readme_code_block_id = FileId::new_fake(VirtualPath::new("README.md"));
    let source = world
        .virtual_source(
            readme_code_block_id,
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
    readme: &mut Readme,
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
    readme: &mut Readme,
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
    readme: &mut Readme,
    sourcepos: Sourcepos,
    url_text: &str,
) {
    let url_text = url_text.trim();

    // Allow links that are only fragments: `#readme-section`.
    if url_text.starts_with("#") {
        // TODO: Consider validating that the fragment is valid.
        return;
    }

    let url_error = match Url::parse(url_text) {
        Ok(url) => {
            // TODO: Should we fetch the URL here like for the `homepage` and
            // `repository` manifest fields, to check it can be publicly accessed?

            check_repo_file_url(diags, readme, sourcepos, url.as_str());

            return;
        }
        Err(error) => error,
    };

    let invalid_url_error = || {
        Diagnostic::error()
            .with_code("readme/link/invalid-url")
            .with_message(format_args!("Invalid url: `{url_text}`\n{url_error}"))
            .with_labels(vec![Label::primary(
                readme_file_id(),
                sourcepos_to_range(readme, sourcepos),
            )])
    };

    // The link couldn't be parsed as a URL, assume it's a local file.
    let file_url = format!("file:///{url_text}");
    let Ok(url) = Url::parse(&file_url) else {
        diags.emit(invalid_url_error());
        return;
    };

    // Don't allow URL with empty paths. If the path consists only of the root
    // component that we added above, the path was completely empty before.
    let absolute_path = url.path();
    if absolute_path == "/" {
        diags.emit(invalid_url_error());
        return;
    }

    // Check if the local file exists.
    let path = PackagePath::from_relative(world.root(), absolute_path);
    if !path.full().exists() {
        diags.emit(
            Diagnostic::error()
                .with_code("readme/link/file-not-found")
                .with_message(format_args!(
                    "Linked file not found: `{absolute_path}`.\n\n\
                     Make sure to commit all linked files and possibly add them to the `exclude` list.\n\n\
                     More details: https://github.com/typst/packages/blob/main/docs/tips.md#what-to-commit-what-to-exclude",
                ))
                .with_note(format_args!("This link was assumed to be a local file because it's couldn't be parsed as an URL: `{url_text}`\n{url_error}"))
                .with_labels(vec![Label::primary(
                    readme_file_id(),
                    sourcepos_to_range(readme, sourcepos),
                )]),
        );
    }

    readme.linked_files.push(path);
}

const DEFAULT_BRANCHES: [&str; 2] = ["main", "master"];

fn check_repo_file_url(diags: &mut Diagnostics, readme: &Readme, sourcepos: Sourcepos, url: &str) {
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
        return;
    };

    let user = captures.get(1).unwrap().as_str();
    let repo = captures.get(2).unwrap().as_str();
    let branch = captures.get(3).unwrap().as_str();
    let path = captures.get(4).unwrap().as_str();

    if !DEFAULT_BRANCHES.contains(&branch) {
        return;
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
            .with_code("readme/link/repository-url-permalink")
            .with_message(format_args!(
                "{name} URL links to default branch: `{url}`.\n\n\
                 Consider using a link to a specific tag/release or a permalink to a commit instead. \
                 This will ensure that the linked resource always matches this version of the package.\n\n\
                 You can create a permalink here: {non_raw_url}\n\n\
                 Alternatively you can also link to a local file. This is preferred if the linked file \
                 is already present in the submitted package."
            ))
            .with_label(Label::primary(
                readme_file_id(),
                sourcepos_to_range(readme, sourcepos),
            )),
    );
}

fn check_image_alternative_description(
    diags: &mut Diagnostics,
    readme: &Readme,
    sourcepos: Sourcepos,
    alt: &str,
) {
    // Don't use `\w` because that would also include Chinese or similar
    // character categories, which don't use whitespace to separate words.
    static SINGLE_WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{Cased_Letter}\p{Number}\-_]+\w$").unwrap());
    static SINGLE_LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w$").unwrap());

    let alt = alt.trim();
    if alt.is_empty() {
        diags.emit(
            Diagnostic::error()
                .with_code("readme/image/missing-alt")
                .with_message(
                    "Missing alternative description for image. \
                     Please add a short description to make this image more accessible.",
                )
                .with_label(Label::primary(
                    readme_file_id(),
                    sourcepos_to_range(readme, sourcepos),
                )),
        );
    } else if SINGLE_WORD.is_match(alt) || SINGLE_LETTER.is_match(alt) {
        diags.emit(
            Diagnostic::warning()
                .with_code("readme/image/inadequate-alt")
                .with_message(format_args!(
                    "Possibly inadequate alternative description for image: `{alt}`. \
                     Please add a short description to make this image more accessible."
                ))
                .with_label(Label::primary(
                    readme_file_id(),
                    sourcepos_to_range(readme, sourcepos),
                )),
        );
    }
}

fn sourcepos_to_range(readme: &Readme, pos: Sourcepos) -> Range<usize> {
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
    let start = byte_offset(&readme.text, pos.start) - 1;
    // `Sourcepos::end` is end-inclusive (byte-wise), and thus `offset + 1 - 1`.
    let end = byte_offset(&readme.text, pos.end);

    start..end
}

fn readme_file_id() -> FileId {
    FileId::new(None, VirtualPath::new("README.md"))
}
