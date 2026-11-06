use std::fmt::{Debug, Display, Formatter};
use std::process::{Command, Output};

#[test]
fn version() {
    exec().arg("version").must_succeed();
}

#[test]
fn typst_version() {
    exec().arg("typst-version").must_succeed();
}

#[test]
fn typst_check_with_path() {
    exec()
        .check()
        .arg("tests/local/second/0.2.1")
        .must_fail()
        .stdout
        .must_contain("warning[exclude/readme]: Your README.md file should not be excluded.");
}

#[test]
fn typst_check_with_spec() {
    // Make sure we have the package that should be tested available.
    Command::new("./prepare")
        .current_dir("tests")
        .output()
        .unwrap();

    exec()
        .current_dir("tests/packages")
        .env("PACKAGES_DIR", ".")
        .check()
        .arg("@local/second:0.2.1")
        .must_fail()
        .stdout
        .must_contain("warning[exclude/readme]: Your README.md file should not be excluded.");
}

/// Executes a command with the Typst CLI.
fn exec() -> Command {
    Command::new(env!("CARGO_BIN_EXE_typst-package-check"))
}

trait CommandExt {
    fn must_succeed(&mut self) -> TestOutput;
    fn must_fail(&mut self) -> TestOutput;
    fn check(&mut self) -> &mut Self;
}

impl CommandExt for Command {
    #[track_caller]
    fn check(&mut self) -> &mut Self {
        self.env("NO_COLOR", "1")
            .env("IGNORE_DOTENV", "1")
            .arg("check")
    }

    #[track_caller]
    fn must_succeed(&mut self) -> TestOutput {
        let output = self.output().unwrap();
        assert!(
            output.status.success(),
            "process failed ({}):\n{}",
            output.status,
            Stream(output.stderr),
        );
        output.into()
    }

    #[track_caller]
    fn must_fail(&mut self) -> TestOutput {
        let output = self.output().unwrap();
        assert!(
            !output.status.success(),
            "process succeeded ({})",
            output.status
        );
        output.into()
    }
}

struct TestOutput {
    stdout: Stream,
}

impl From<Output> for TestOutput {
    fn from(value: Output) -> Self {
        Self {
            stdout: Stream(value.stdout),
        }
    }
}

struct Stream<T = Vec<u8>>(T);

impl<T: AsRef<[u8]>> Stream<T> {
    #[track_caller]
    fn must_contain(&self, data: impl Debug + AsRef<[u8]>) -> &Self {
        assert!(
            self.contains(data.as_ref()),
            "{self:?} did not contain {data:?}"
        );
        self
    }

    fn contains(&self, data: impl AsRef<[u8]>) -> bool {
        memchr::memmem::find(self.0.as_ref(), data.as_ref()).is_some()
    }
}

impl<T: AsRef<[u8]>> Debug for Stream<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&String::from_utf8_lossy(self.0.as_ref()), f)
    }
}

impl<T: AsRef<[u8]>> Display for Stream<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&String::from_utf8_lossy(self.0.as_ref()), f)
    }
}
