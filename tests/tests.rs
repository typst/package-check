use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use typst::ecow::EcoString;
use typst::syntax::package::{PackageSpec, PackageVersion};

#[derive(Debug)]
struct Snapshot {
    spec: PackageSpec,
    output: String,
}

impl Snapshot {
    fn output_path(&self) -> PathBuf {
        let file_name = spec_to_snapshot_name(&self.spec);
        PathBuf::from_iter(["tests", "out", &file_name])
    }
}

fn main() -> anyhow::Result<()> {
    // Make all paths relative to the workspace. That's nicer for IDEs when
    // clicking on paths printed to the terminal.
    let workspace_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::env::set_current_dir(workspace_dir).unwrap();

    let filters = std::env::args().skip(1).collect::<Vec<_>>();

    let mut snapshots = std::fs::read_dir("tests/ref")?
        .map(|entry| {
            let entry = entry?;
            let path = entry.path();
            let spec = spec_from_snapshot_name(path.file_name().unwrap())?;
            let output = std::fs::read_to_string(&path)?;
            Ok(Snapshot { spec, output })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let all = snapshots.len();
    snapshots.retain(|s| {
        if filters.is_empty() {
            return true;
        }

        let spec = s.spec.to_string();
        filters.iter().any(|f| spec.contains(f))
    });
    let filtered = all - snapshots.len();

    let results = snapshots
        .par_iter()
        .map(check_snapshot)
        .collect::<anyhow::Result<Vec<_>>>()?;

    eprintln!("\n=== Results ===");
    let mut passed = 0;
    for res in results {
        match res {
            TestResult::Passed => passed += 1,
            TestResult::Failed { snapshot, diff } => {
                eprintln!("[ERROR] {}:\n{diff}", snapshot.spec);
            }
        }
    }

    let failed = all - passed - filtered;
    eprintln!("{passed} passed, {failed} failed, {filtered} filtered out");

    Ok(())
}

enum TestResult<'a> {
    Passed,
    Failed {
        snapshot: &'a Snapshot,
        diff: String,
    },
}

fn check_snapshot(snapshot: &Snapshot) -> anyhow::Result<TestResult<'_>> {
    eprintln!("run {}", snapshot.spec,);
    let output = Command::new("typst-package-check")
        .current_dir("tests/packages")
        .stdin(Stdio::null())
        .env("NO_COLOR", "1")
        .env("IGNORE_DOTENV", "1")
        .env("PACKAGES_DIR", ".")
        .arg("check")
        .arg(snapshot.spec.to_string())
        .output()?;

    std::io::stderr().write_all(&output.stderr).unwrap();

    let output = String::from_utf8(output.stdout)?;
    std::fs::write(snapshot.output_path(), &output)?;

    if output != snapshot.output {
        let diff = pretty_assertions::StrComparison::new(&snapshot.output, &output).to_string();
        return Ok(TestResult::Failed { snapshot, diff });
    }

    Ok(TestResult::Passed)
}

fn spec_to_snapshot_name(
    PackageSpec {
        namespace,
        name,
        version,
    }: &PackageSpec,
) -> String {
    format!("{namespace}+{name}+{version}.txt")
}

fn spec_from_snapshot_name(file_name: &OsStr) -> anyhow::Result<PackageSpec> {
    let Some(file_name) = file_name.to_str() else {
        bail!("invalid snapshot file name")
    };
    let Some(stem) = file_name.strip_suffix(".txt") else {
        bail!("snapshot file has no `.txt` extension");
    };

    let mut pieces = stem.split('+');
    let Some(namespace) = pieces.next().map(EcoString::from) else {
        bail!("missing namespace");
    };
    let Some(name) = pieces.next().map(EcoString::from) else {
        bail!("missing package name");
    };
    let Some(version) = pieces.next() else {
        bail!("missing package name");
    };

    if let Some(tail) = pieces.next() {
        bail!("unexpected string at end of snapshot `{tail}`");
    }

    let version = version
        .parse::<PackageVersion>()
        .map_err(|err| anyhow!("invalid package version {err}"))?;

    Ok(PackageSpec {
        namespace,
        name,
        version,
    })
}
