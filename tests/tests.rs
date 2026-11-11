use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, anyhow, bail};
use clap::Parser;
use codespan_reporting::term;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use typst::ecow::EcoString;
use typst::syntax::package::{PackageSpec, PackageVersion};
use typst_package_check::workspace::{SystemPackages, Workspace};

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

    fn ref_path(&self) -> PathBuf {
        let file_name = spec_to_snapshot_name(&self.spec);
        PathBuf::from_iter(["tests", "ref", &file_name])
    }
}

#[derive(clap::Parser)]
struct Args {
    /// Update snapshots of failed tests.
    ///
    /// Either update `all` or get asked for a `choice` for each failing test.
    #[arg(
        long,
        default_missing_value = "all",
        num_args = 0..=1,
        require_equals = true,
    )]
    update: Option<Update>,
    /// Filter tests by name.
    ///
    /// Any test containing one of the filter strings will be run.
    #[arg()]
    filters: Vec<String>,
}

#[derive(Copy, Clone, clap::ValueEnum)]
enum Update {
    All,
    Choice,
}

fn main() -> anyhow::Result<()> {
    // Make all paths relative to the workspace. That's nicer for IDEs when
    // clicking on paths printed to the terminal.
    let workspace_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::env::set_current_dir(workspace_dir).unwrap();

    prepare()?;

    let args = Args::parse();

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
        if args.filters.is_empty() {
            return true;
        }

        let spec = s.spec.to_string();
        args.filters.iter().any(|f| spec.contains(f))
    });
    let filtered = all - snapshots.len();

    // Ignore system wide installed packages.
    let workspace = Workspace::new("tests/packages".into(), SystemPackages::empty());
    let results = snapshots
        .par_iter()
        .map(|snapshot| {
            let res = check_snapshot(&workspace, snapshot)?;
            Ok((snapshot, res))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    eprintln!("\n=== Results ===");
    let mut passed = 0;
    let mut updated = 0;
    for (snapshot, res) in results {
        match res {
            TestResult::Passed => passed += 1,
            TestResult::Failed { diff, output } => {
                eprintln!(
                    "\x1b[31m[ERROR]\x1b[0m \x1b[33m{}\x1b[0m:\n{diff}",
                    snapshot.spec
                );

                if should_update(args.update)? {
                    std::fs::write(snapshot.ref_path(), &output).context("updating reference")?;
                    eprintln!("updated snapshot");
                    updated += 1;
                }
            }
        }
    }

    let failed = all - passed - filtered - updated;
    eprint!("{passed} passed, {failed} failed");
    if updated > 0 {
        eprint!(", {updated} updated");
    }
    if filtered > 0 {
        eprint!(", {filtered} filtered out");
    }
    eprintln!();

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

fn prepare() -> anyhow::Result<()> {
    // Make sure the `out` directory exists.
    std::fs::create_dir_all("tests/out").context("creating `out` directory")?;

    // Make sure we have the package that should be tested available.
    Command::new("./prepare")
        .current_dir("tests")
        .output()
        .context("preparing packages")?;

    Ok(())
}

enum TestResult {
    Passed,
    Failed { diff: String, output: String },
}

fn check_snapshot(workspace: &Workspace, snapshot: &Snapshot) -> anyhow::Result<TestResult> {
    eprintln!("run {}", snapshot.spec,);
    let res = typst_package_check::check::offline_checks(
        workspace,
        Some(&snapshot.spec),
        workspace.path_in_repo(&snapshot.spec),
        true,
    );

    // Format diagnostics.
    let output = match res {
        Ok((worlds, _, _, diags)) => {
            let config = term::Config {
                tab_width: 2,
                ..Default::default()
            };
            let mut output = String::new();
            for diagnostic in diags.all().rev() {
                term::emit_to_string(&mut output, &config, &worlds.package, diagnostic).unwrap();
            }
            output
        }
        Err(e) => {
            format!("Fatal error: {}\n", e.message)
        }
    };

    // Write to output file.
    std::fs::write(snapshot.output_path(), &output).context("writing output")?;

    // Generate diff.
    if output != snapshot.output {
        let diff = pretty_assertions::StrComparison::new(&snapshot.output, &output).to_string();
        return Ok(TestResult::Failed { diff, output });
    }

    Ok(TestResult::Passed)
}

fn should_update(update: Option<Update>) -> anyhow::Result<bool> {
    let Some(update) = update else {
        return Ok(false);
    };

    let should_update = match update {
        Update::All => true,
        Update::Choice => {
            eprintln!("update snapshot [y/N]?");
            loop {
                let mut buf = String::new();
                std::io::stdin()
                    .read_line(&mut buf)
                    .context("reading user input")?;
                buf.make_ascii_lowercase();
                match buf.trim() {
                    "y" | "yes" => break true,
                    "" | "n" | "no" => break false,
                    _ => (),
                }
            }
        }
    };
    Ok(should_update)
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
