//! Wrapper around the `git` command line.

use std::path::{Path, PathBuf};

use tokio::process::Command;

pub struct GitRepo<'a> {
    dir: &'a Path,
}

impl<'a> GitRepo<'a> {
    pub fn open(dir: &'a Path) -> Self {
        GitRepo { dir }
    }

    pub async fn fetch_commit(&self, sha: impl AsRef<str>) -> Option<()> {
        Command::new("git")
            .args(&["-C", self.dir.to_str()?, "fetch", "origin", sha.as_ref()])
            .spawn()
            .ok()?
            .wait()
            .await
            .ok()?;
        Some(())
    }

    pub async fn checkout_commit(&self, sha: impl AsRef<str>) -> Option<()> {
        Command::new("git")
            .args(&["-C", self.dir.to_str()?, "checkout", sha.as_ref()])
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
                .args(&[
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
}
