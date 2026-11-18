use semver::{Prerelease, Version};

/// Applies a pre-release to an already-bumped version, preserving matching pre-releases.
///
/// # Arguments
///
/// * `version` - The version to which the pre-release identifier should be applied.
/// * `pre_release` - An optional pre-release identifier to apply to the version.
///
/// # Returns
///
/// The version with the pre-release identifier applied, if provided.
/// If the version already has a matching pre-release identifier, it is assumed already bumped by `git-cliff`.
/// If no pre-release identifier is provided, the version will be made stable, i.e. without a pre-release.
pub fn apply_pre_release(mut version: Version, pre_release: Option<&str>) -> Version {
    if let Some(identifier) = pre_release {
        if version.pre.as_str() != identifier && !version.pre.as_str().starts_with(&format!("{identifier}.")) {
            version.pre = Prerelease::new(&format!("{identifier}.1")).unwrap();
        }
    } else {
        version.pre = Prerelease::EMPTY;
    }

    version
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Tests that a version has the provided pre-release identifier applied.
    #[rstest]
    #[case::alpha("1.2.3", Some("alpha"), "1.2.3-alpha.1")]
    #[case::bare_identifier("1.2.3-beta", Some("beta"), "1.2.3-beta")]
    #[case::same_channel("1.2.3-rc.1", Some("rc"), "1.2.3-rc.1")]
    #[case::different_channel("1.2.3-rc.2", Some("beta"), "1.2.3-beta.1")]
    #[case::identifier_boundary("1.2.3-betamax.2", Some("beta"), "1.2.3-beta.1")]
    #[case::dotted_identifier("1.2.3-beta.preview.2", Some("beta.preview"), "1.2.3-beta.preview.2")]
    #[case::stable("1.2.3-beta.3", None, "1.2.3")]
    #[case::build("1.2.3-unstable.1+12ab34c", Some("unstable"), "1.2.3-unstable.1+12ab34c")]
    #[case::stable_build("1.2.3-beta.3+12ab34c", None, "1.2.3+12ab34c")]
    #[case::extra_dots("1.2.3-unstable.a", Some("unstable"), "1.2.3-unstable.a")]
    #[case::noop("1.2.3", None, "1.2.3")]
    fn test_apply_pre_release(#[case] old_version: &str, #[case] pre_release: Option<&str>, #[case] expected: &str) {
        let version = Version::parse(old_version).unwrap();
        let new_version = apply_pre_release(version, pre_release);
        assert_eq!(new_version.to_string(), expected);
    }
}
