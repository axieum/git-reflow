#[cfg(feature = "bitbucket")]
#[path = "providers/bitbucket.rs"]
mod bitbucket;

#[cfg(feature = "github")]
#[path = "providers/github.rs"]
mod github;
