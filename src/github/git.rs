//! Wrapper around the `git` command line.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use tokio::process::Command;
use tracing::debug;

pub struct GitRepo<'a> {
    dir: &'a Path,
}

impl<'a> GitRepo<'a> {
    pub fn open(dir: &'a Path) -> Self {
        GitRepo { dir }
    }

    pub async fn pull_main(&self) -> Option<()> {
        Command::new("git")
            .args([
                "-C",
                self.dir.to_str()?,
                "-c",
                "receive.maxInputSize=134217728", // 128MB
                "pull",
                "origin",
                "main",
                "--ff-only",
            ])
            .spawn()
            .ok()?
            .wait()
            .await
            .ok()?;
        Some(())
    }

    pub async fn fetch_commit(&self, sha: impl AsRef<str>) -> Option<()> {
        Command::new("git")
            .args([
                "-C",
                self.dir.to_str()?,
                "-c",
                "receive.maxInputSize=134217728", // 128MB
                "fetch",
                "origin",
                sha.as_ref(),
            ])
            .spawn()
            .ok()?
            .wait()
            .await
            .ok()?;
        Some(())
    }

    /// Checks out a commit in a new working tree
    pub async fn checkout_commit(
        &self,
        sha: impl AsRef<str>,
        working_tree: impl AsRef<Path>,
    ) -> Option<()> {
        debug!(
            "Checking out {} in {}",
            sha.as_ref(),
            working_tree.as_ref().display()
        );
        tokio::fs::create_dir_all(&working_tree).await.ok()?;
        let working_tree = working_tree.as_ref().canonicalize().unwrap();
        Command::new("git")
            .args([
                "-C",
                self.dir.to_str()?,
                &format!("--work-tree={}", working_tree.display()),
                "checkout",
                sha.as_ref(),
                "--",
                ".",
            ])
            .spawn()
            .ok()?
            .wait()
            .await
            .ok()?;
        Some(())
    }

    pub async fn files_touched_by(&self, sha: impl AsRef<str>) -> Option<Vec<PathBuf>> {
        let command_output = String::from_utf8(
            Command::new("git")
                .args([
                    "-C",
                    self.dir.to_str()?,
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    sha.as_ref(),
                    "main",
                ])
                .output()
                .await
                .ok()?
                .stdout,
        )
        .ok()?;

        Some(
            command_output
                .lines()
                .map(|l| Path::new(l).to_owned())
                .collect(),
        )
    }

    pub fn authors_of(&self, file: &Path) -> Option<HashSet<String>> {
        use std::process::Command;

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
        Some(authors)
    }
}
