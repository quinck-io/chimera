pub mod auth;
pub mod broker;
pub mod registration;

pub const RUNNER_VERSION: &str = "2.329.0";
// TODO: make this dynamic, e.g. by reading from a file or an environment variable OR getting from the GitHub API. This is the version of the GitHub Actions runner that Chimera will use to run jobs. We need to specify this version because the GitHub Actions runner is not backward compatible, and we want to ensure that Chimera uses a compatible version.
