//! Wrapper around the `git` command line.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Output, Stdio},
};

use eyre::{Context, ContextCompat};
use tokio::process::Command;
use tracing::debug;
use typst::syntax::package::{PackageSpec, PackageVersion};

pub fn repo_dir() -> PathBuf {
    PathBuf::from(
        &std::env::var("PACKAGES_DIR")
            .or_else(|_| std::env::var("GITHUB_WORKSPACE"))
            .unwrap_or("..".to_owned()),
    )
}

pub struct GitRepo<'a> {
    dir: &'a Path,
}

impl<'a> GitRepo<'a> {
    pub fn open(dir: &'a Path) -> Self {
        GitRepo { dir }
    }

    pub async fn pull_main(&self) -> eyre::Result<()> {
        debug!("Pulling main branch");
        traced_git([
            "-C",
            self.dir()?,
            "-c",
            "receive.maxInputSize=134217728", // 128MB
            "pull",
            "origin",
            "main",
            "--ff-only",
        ])
        .await?;
        debug!("Done");
        Ok(())
    }

    pub async fn fetch_commit(&self, sha: impl AsRef<str>) -> eyre::Result<()> {
        debug!("Fetching commit: {}", sha.as_ref());
        traced_git([
            "-C",
            self.dir()?,
            "-c",
            "receive.maxInputSize=134217728", // 128MB
            "fetch",
            "origin",
            sha.as_ref(),
        ])
        .await
        .context("Failed to fetch {} (probably because of some large file).")?;
        debug!("Done");
        Ok(())
    }

    /// Checks out a commit in a new working tree
    pub async fn checkout_commit(
        &self,
        sha: impl AsRef<str>,
        working_tree: impl AsRef<Path>,
    ) -> eyre::Result<()> {
        debug!(
            "Checking out {} in {}",
            sha.as_ref(),
            working_tree.as_ref().display()
        );
        tokio::fs::create_dir_all(&working_tree).await?;
        let working_tree = working_tree.as_ref().canonicalize()?;
        traced_git([
            "-C",
            self.dir
                .to_str()
                .context("Directory name is not valid unicode")?,
            &format!("--work-tree={}", working_tree.display()),
            "checkout",
            sha.as_ref(),
            "--",
            ".",
        ])
        .await?;
        debug!("Done");
        Ok(())
    }

    pub async fn files_touched_by(&self, sha: impl AsRef<str>) -> eyre::Result<Vec<PathBuf>> {
        debug!("Listing files touched by {}", sha.as_ref());
        let command_output = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    self.dir()?,
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "--merge-base",
                    "main",
                    sha.as_ref(),
                ])
                .output()
                .await?
                .stdout,
        )?;

        debug!("Done");

        Ok(parse_diff_tree_paths(&command_output))
    }

    pub fn authors_of(&self, file: &Path) -> Option<HashSet<String>> {
        use std::process::Command;

        debug!("Lisiting authors of {}", file.display());

        let output = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    self.dir.to_str()?,
                    "blame",
                    "--porcelain",
                    "--",
                    Path::new(".").canonicalize().ok()?.join(file).to_str()?,
                ])
                .output()
                .ok()?
                .stdout,
        )
        .ok()?;

        let authors: HashSet<_> = output
            .lines()
            .filter(|l| l.starts_with("author "))
            .map(|l| {
                let prefix_len = "author ".len();
                l[prefix_len..].to_owned()
            })
            .collect();

        debug!("Done");
        Some(authors)
    }

    pub fn commit_for_file(&self, file: &Path) -> Option<String> {
        use std::process::Command;

        debug!("Finding the commit that last touched {}", file.display());

        let output = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    self.dir.to_str()?,
                    "blame",
                    "--porcelain",
                    "--",
                    Path::new(".").canonicalize().ok()?.join(file).to_str()?,
                ])
                .output()
                .ok()?
                .stdout,
        )
        .ok()?;

        output
            .lines()
            .next()?
            .split(' ')
            .next()
            .map(|commit| commit.to_owned())
    }

    pub fn dir(&self) -> eyre::Result<&str> {
        self.dir
            .to_str()
            .context("Directory name is not valid unicode")
    }

    pub async fn has_previous_version(&self, package: &PackageSpec) -> eyre::Result<bool> {
        let package_dir = PathBuf::from(self.dir()?)
            .join("packages")
            .join(package.namespace.as_str())
            .join(package.name.as_str());
        let mut all_versions = tokio::fs::read_dir(package_dir).await?;
        while let Ok(Some(version)) = all_versions.next_entry().await {
            if version
                .file_type()
                .await
                .map(|t| t.is_dir())
                .unwrap_or(false)
            {
                let version: PackageVersion = version
                    .file_name()
                    .to_str()
                    .context("Can't convert directory name to string")?
                    .parse()
                    .ok()
                    .context("Invalid version number")?;

                if version < package.version {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }
}

#[tracing::instrument(name = "git-command")]
async fn traced_git(
    args: impl IntoIterator<Item = &str> + std::fmt::Debug,
) -> eyre::Result<Output> {
    let out = Command::new("git")
        .args(args)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?
        .wait_with_output()
        .await?;

    if let Ok(stderr) = std::str::from_utf8(&out.stderr) {
        debug!(stderr = stderr)
    }
    if let Ok(stdout) = std::str::from_utf8(&out.stdout) {
        debug!(stdout = stdout)
    }

    Ok(out)
}

fn parse_diff_tree_paths(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .map(|l| {
            if l.starts_with('"') && l.ends_with('"') {
                let mut path = Vec::new();
                let mut escape_seq = 0;
                let mut escaped = String::new();
                for c in l[1..l.len() - 2].chars() {
                    if c == '\\' {
                        escape_seq = 3;
                        continue;
                    }

                    if escape_seq == 0 {
                        let start = path.len();
                        path.resize(start + c.len_utf8(), 0);
                        c.encode_utf8(&mut path[start..]);
                    } else {
                        escaped.push(c);
                        escape_seq -= 1;

                        if escape_seq == 0 {
                            path.push(
                                u8::from_str_radix(&escaped, 8)
                                    .expect("Invalid escape sequence from Git"),
                            );
                            escaped = String::new()
                        }
                    }
                }

                PathBuf::from(String::from_utf8(path).expect("Invalid UTF8 in path")).to_owned()
            } else {
                Path::new(l).to_owned()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[test]
    fn touched_filed() {
        let output = r#"packages/preview/scholarly-tauthesis/0.8.0/.gitattributes
packages/preview/scholarly-tauthesis/0.8.0/.gitignore
packages/preview/scholarly-tauthesis/0.8.0/LICENSE
packages/preview/scholarly-tauthesis/0.8.0/README.md
packages/preview/scholarly-tauthesis/0.8.0/tauthesis.typ
packages/preview/scholarly-tauthesis/0.8.0/template/bibliography.bib
packages/preview/scholarly-tauthesis/0.8.0/template/code/README.md
packages/preview/scholarly-tauthesis/0.8.0/template/code/square.jl
packages/preview/scholarly-tauthesis/0.8.0/template/content/01.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/02.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/03.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/04.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/A.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/README.md
packages/preview/scholarly-tauthesis/0.8.0/template/content/abstract.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/glossary.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/preface.typ
packages/preview/scholarly-tauthesis/0.8.0/template/content/tekoalyn-kaytto.typ
"packages/preview/scholarly-tauthesis/0.8.0/template/content/tiivistelm\303\244.typ"
packages/preview/scholarly-tauthesis/0.8.0/template/content/use-of-ai.typ
packages/preview/scholarly-tauthesis/0.8.0/template/images/README.md
packages/preview/scholarly-tauthesis/0.8.0/template/images/tau-logo-fin-eng.svg
packages/preview/scholarly-tauthesis/0.8.0/template/main.typ
packages/preview/scholarly-tauthesis/0.8.0/template/meta.typ
packages/preview/scholarly-tauthesis/0.8.0/template/preamble.typ
packages/preview/scholarly-tauthesis/0.8.0/thumbnail.png
packages/preview/scholarly-tauthesis/0.8.0/typst.toml"#;
        assert_eq!(
            super::parse_diff_tree_paths(output),
            [
                "packages/preview/scholarly-tauthesis/0.8.0/.gitattributes",
                "packages/preview/scholarly-tauthesis/0.8.0/.gitignore",
                "packages/preview/scholarly-tauthesis/0.8.0/LICENSE",
                "packages/preview/scholarly-tauthesis/0.8.0/README.md",
                "packages/preview/scholarly-tauthesis/0.8.0/tauthesis.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/bibliography.bib",
                "packages/preview/scholarly-tauthesis/0.8.0/template/code/README.md",
                "packages/preview/scholarly-tauthesis/0.8.0/template/code/square.jl",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/01.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/02.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/03.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/04.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/A.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/README.md",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/abstract.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/glossary.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/preface.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/tekoalyn-kaytto.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/tiivistelmä.ty",
                "packages/preview/scholarly-tauthesis/0.8.0/template/content/use-of-ai.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/images/README.md",
                "packages/preview/scholarly-tauthesis/0.8.0/template/images/tau-logo-fin-eng.svg",
                "packages/preview/scholarly-tauthesis/0.8.0/template/main.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/meta.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/template/preamble.typ",
                "packages/preview/scholarly-tauthesis/0.8.0/thumbnail.png",
                "packages/preview/scholarly-tauthesis/0.8.0/typst.toml"
            ]
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
        )
    }
}
