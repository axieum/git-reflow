use crate::strategy::Strategy;
use crate::strategy::python::PythonStrategy;
use crate::strategy::rust::RustStrategy;
use anyhow::anyhow;
use std::fs;
use std::path::Path;

/// Tries to detect the release strategy for a given directory
/// by looking at the files present.
///
///   * [`PythonStrategy`]
///     * `pyproject.toml`
///     * `setup.py`
///   * [`RustStrategy`]
///     * `Cargo.toml`
///
/// # Returns
/// A release strategy if one is detected.
pub fn detect_strategy<P: AsRef<Path>>(dir: &P) -> anyhow::Result<Strategy> {
    fs::read_dir(dir.as_ref())?
        .find_map(|entry| {
            entry.ok().and_then(|e| match e.file_name().to_str()? {
                "pyproject.toml" | "setup.py" => Some(Strategy::Python(PythonStrategy::default())),
                "Cargo.toml" => Some(Strategy::Rust(RustStrategy::default())),
                _ => None,
            })
        })
        .ok_or_else(|| {
            anyhow!(
                "could not detect release strategy in directory: `{}`",
                dir.as_ref().display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use rstest::rstest;

    /// Tests that a matching manifest file selects its release strategy.
    #[rstest]
    #[case::python_pyproject_toml("pyproject.toml", Strategy::Python(PythonStrategy::default()))]
    #[case::python_setup_py("setup.py", Strategy::Python(PythonStrategy::default()))]
    #[case::rust_cargo_toml("Cargo.toml", Strategy::Rust(RustStrategy::default()))]
    fn detect_strategy_with_file(#[case] filename: &str, #[case] expected: Strategy) {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child(filename).touch().unwrap();

        let strategy = detect_strategy(&temp_dir);
        assert_eq!(
            strategy.unwrap(),
            expected,
            "expected `{}` strategy",
            expected
        );
    }

    /// Tests that an unrecognized manifest file produces a detection error.
    #[test]
    fn detect_strategy_unknown() {
        let temp_dir = assert_fs::TempDir::new().unwrap();
        temp_dir.child("lorem.txt").touch().unwrap();

        let strategy = detect_strategy(&temp_dir);
        assert!(
            strategy
                .unwrap_err()
                .to_string()
                .contains("could not detect release strategy")
        );
    }
}
