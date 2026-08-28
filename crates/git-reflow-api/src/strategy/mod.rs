use crate::settings::pkg::PackageConfig;
use crate::strategy::basic::BasicStrategy;
use semver::Version;
use serde::de::Error;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, de};
use std::fmt;
use std::fmt::Display;
use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;

pub mod basic;

/// Available release strategies.
///
/// They define how new versions are written to project files, etc.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// The basic release strategy.
    Basic(BasicStrategy),
}

pub trait BaseStrategy {
    /// Writes a new version to the project files.
    ///
    /// # Arguments
    ///
    /// * `new_version` - The new version to apply.
    /// * `config` - The package configuration.
    fn write_version(&self, new_version: &Version, config: &PackageConfig) -> anyhow::Result<()>;

    /// Suggests the package name from the project files.
    ///
    /// # Arguments
    ///
    /// * `dir` - The package directory to get the name for.
    ///
    /// # Returns
    ///
    /// The package name for the given directory.
    fn suggest_name(&self, dir: &Path) -> anyhow::Result<String>;

    /// Suggests nested package configurations (e.g. Cargo workspace) that should be included.
    ///
    /// # Arguments
    ///
    /// * `dir` - The package directory to look for nested packages.
    ///
    /// # Returns
    ///
    /// The package configurations if any.
    fn suggest_packages(&self, dir: &Path) -> anyhow::Result<Vec<PackageConfig>>;
}

impl BaseStrategy for Strategy {
    fn write_version(&self, new_version: &Version, config: &PackageConfig) -> anyhow::Result<()> {
        match self {
            Strategy::Basic(strategy) => strategy.write_version(new_version, config),
        }
    }

    fn suggest_name(&self, dir: &Path) -> anyhow::Result<String> {
        match self {
            Strategy::Basic(strategy) => strategy.suggest_name(dir),
        }
    }

    fn suggest_packages(&self, dir: &Path) -> anyhow::Result<Vec<PackageConfig>> {
        match self {
            Strategy::Basic(strategy) => strategy.suggest_packages(dir),
        }
    }
}

impl Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Strategy::Basic(_) => write!(f, "basic"),
        }
    }
}

impl Default for Strategy {
    fn default() -> Self {
        Strategy::Basic(BasicStrategy::default())
    }
}

impl FromStr for Strategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "basic" => Ok(Strategy::Basic(BasicStrategy::default())),
            &_ => Err(format!("unknown strategy `{s}`").to_string()),
        }
    }
}

/// Deserializes a given string or struct into a result of [`T`].
///
/// # Arguments
///
/// * `deserializer` - The [`Deserializer`] instance.
///
/// # Returns
///
/// A result of [`T`].
///
pub fn string_or_struct<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de> + FromStr<Err = String>,
    D: Deserializer<'de>,
{
    struct StringOrStruct<T>(PhantomData<fn() -> T>);

    impl<'de, T> Visitor<'de> for StringOrStruct<T>
    where
        T: Deserialize<'de> + FromStr<Err = String>,
    {
        type Value = Option<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("string, map, or null")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: Error,
        {
            T::from_str(value)
                .map(Some)
                .map_err(|err| Error::custom(format!("failed to parse string as strategy: {err}")))
        }

        fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            T::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
        }
    }

    deserializer.deserialize_any(StringOrStruct(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::str::FromStr;

    /// Tests that a strategy's name is displayed when formatted.
    #[rstest]
    #[case::basic(Strategy::Basic(BasicStrategy::default()), "basic")]
    fn display_strategy(#[case] strategy: Strategy, #[case] expected: &str) {
        assert_eq!(format!("{strategy}"), expected);
    }

    /// Tests that a default strategy is returned.
    #[rstest]
    fn default_strategy() {
        assert_eq!(
            Strategy::default(),
            Strategy::Basic(BasicStrategy::default())
        );
    }

    /// Tests that the various strategy defaults can be resolved by their name.
    #[rstest]
    #[case::basic("basic", Strategy::Basic(BasicStrategy::default()))]
    fn strategy_from_str(#[case] value: String, #[case] strategy: Strategy) {
        assert_eq!(Strategy::from_str(&value).unwrap(), strategy);
    }

    /// Tests that an unknown name is not resolved to a strategy.
    #[rstest]
    fn strategy_from_str_unknown() {
        assert_eq!(
            Strategy::from_str("invalid").err().unwrap(),
            "unknown strategy `invalid`"
        );
    }

    /// Tests that a valid string can be deserialized to a strategy.
    #[test]
    fn deserialize_strategy_from_str() {
        let json = "\"basic\"";
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result: Result<Option<Strategy>, _> = string_or_struct(deserializer);
        assert_eq!(
            result.unwrap(),
            Some(Strategy::Basic(BasicStrategy::default()))
        );
    }

    /// Tests that an unknown name is not deserialized to a strategy.
    #[test]
    fn deserialize_strategy_from_str_unknown() {
        let json = "\"magic\"";
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result: Result<Option<Strategy>, _> = string_or_struct(deserializer);
        assert_eq!(
            result.unwrap_err().to_string(),
            "failed to parse string as strategy: unknown strategy `magic` at line 1 column 7"
        );
    }

    /// Tests that an empty string is not deserialized to a strategy.
    #[test]
    fn deserialize_strategy_from_str_empty() {
        let json = "\"\"";
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result: Result<Option<Strategy>, _> = string_or_struct(deserializer);
        assert_eq!(
            result.unwrap_err().to_string(),
            "failed to parse string as strategy: unknown strategy `` at line 1 column 2"
        );
    }

    /// Tests that valid struct is deserialized to a strategy.
    #[test]
    fn deserialize_strategy_from_struct() {
        let json = "{ \"basic\": {} }";
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result: Result<Option<Strategy>, _> = string_or_struct(deserializer);
        assert_eq!(
            result.unwrap(),
            Some(Strategy::Basic(BasicStrategy::default()))
        );
    }

    /// Tests that a malformed struct is not deserialized to a strategy.
    #[test]
    fn deserialize_strategy_from_struct_malformed() {
        let json = "{ \"basic\": null }";
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result: Result<Option<Strategy>, _> = string_or_struct(deserializer);
        assert_eq!(
            result.unwrap_err().to_string(),
            "invalid type: null, expected struct BasicStrategy at line 1 column 15"
        );
    }

    /// Tests that `null` is not deserialized to a strategy.
    #[test]
    fn deserialize_strategy_from_null() {
        let json = "null";
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result: Result<Option<Strategy>, _> = string_or_struct(deserializer);
        assert_eq!(
            result.unwrap_err().to_string(),
            "invalid type: null, expected string, map, or null at line 1 column 4"
        );
    }

    /// Tests that an unexpected type is not deserialized to a strategy.
    #[test]
    fn deserialize_strategy_from_invalid_type() {
        let json = "9";
        let deserializer = &mut serde_json::Deserializer::from_str(json);
        let result: Result<Option<Strategy>, _> = string_or_struct(deserializer);
        assert_eq!(
            result.unwrap_err().to_string(),
            "invalid type: integer `9`, expected string, map, or null at line 1 column 1"
        );
    }
}
