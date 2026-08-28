use crate::strategy::Strategy;
use anyhow::anyhow;
use std::fs;
use std::path::Path;

/// Tries to detect the release strategy for a given directory
/// by looking at the files present.
///
/// # Returns
/// A release strategy if one is detected.
pub fn detect_strategy<P: AsRef<Path>>(dir: &P) -> anyhow::Result<Strategy> {
    fs::read_dir(dir.as_ref())?
        .find_map(|entry| {
            entry.ok().and_then(|e| match e.file_name().to_str()? {
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
