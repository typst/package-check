//! Wrapper around the `git` command line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tracing::debug;

use crate::check::codes::Code;
use crate::check::{Result, TryExt};

pub struct GitRepo {
    dir: PathBuf,
}

impl GitRepo {
    pub fn open(dir: PathBuf) -> Result<Self> {
        let repo = GitRepo { dir };
        traced_git(["config", "--global", "--add", "safe.directory", repo.dir()?])?;
        Ok(repo)
    }

    pub fn files_touched_by(&self, sha: impl AsRef<str>) -> Result<Vec<PathBuf>> {
        debug!("Listing files touched by {}", sha.as_ref());
        let command_output = String::from_utf8(
            traced_git([
                "-C",
                self.dir()?,
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--merge-base",
                std::env::var("GITHUB_BASE_REF")
                    .as_deref()
                    .unwrap_or("main"),
                sha.as_ref(),
                "--",
            ])?
            .stdout,
        )
        .error(Code::GitUtf8, "Invalid UTF-8 output from Git")?;

        debug!("Done");

        Ok(parse_diff_tree_paths(&command_output))
    }

    pub fn commit_for_file(&self, file: &Path) -> Option<String> {
        debug!("Finding the commit that last touched {}", file.display());

        let output = String::from_utf8(
            traced_git([
                "-C",
                self.dir.to_str()?,
                "blame",
                "--porcelain",
                "--",
                Path::new(".").canonicalize().ok()?.join(file).to_str()?,
            ])
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

    pub fn dir(&self) -> Result<&str> {
        self.dir
            .to_str()
            .error(Code::GitUtf8, "Directory name is not valid UTF-8")
    }
}

#[tracing::instrument(name = "git-command")]
fn traced_git<'a>(args: impl IntoIterator<Item = &'a str> + std::fmt::Debug) -> Result<Output> {
    let out = Command::new("git")
        .args(args)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .error(Code::GitIo, "Failed to spawn git subprocess")?
        .wait_with_output()
        .error(Code::GitIo, "Failed to read git output")?;

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
