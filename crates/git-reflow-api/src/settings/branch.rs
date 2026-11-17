use regex::Regex;

/// The individual branch configuration.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BranchConfig {
    /// The pattern matching the name of a branch.
    #[serde(deserialize_with = "deserialize_name", serialize_with = "serialize_name")]
    pub name: Regex,
    /// The pre-release identifier (e.g. `beta`) if this branch is a pre-release branch.
    ///
    /// **Default:** `None`
    pub pre_release: Option<String>,
}

/// Deserialises the branch name regex from `$.name`.
fn deserialize_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Regex, D::Error> {
    let pattern: String = serde::Deserialize::deserialize(deserializer)?;
    Regex::new(&pattern).map_err(serde::de::Error::custom)
}

/// Serialises the branch name regex in `$.name` as strings.
fn serialize_name<S: serde::Serializer>(pattern: &Regex, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(pattern.as_str())
}
