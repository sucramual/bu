mod adapters;
mod domain;
mod error;
mod report;

use std::env;
use std::fs;
use std::path::PathBuf;

use adapters::{GitAdapter, GitHubAdapter};
pub use domain::Config;
pub use error::AppError;
pub use report::{RunReport, format_report};

pub fn load_config(path: Option<PathBuf>) -> Result<Config, AppError> {
    let path = path.unwrap_or(default_config_path()?);
    let source = fs::read_to_string(&path).map_err(|source| AppError::ConfigRead {
        path: path.clone(),
        source,
    })?;

    toml::from_str(&source).map_err(|source| AppError::ConfigParse { path, source })
}

pub fn run_status(config: &Config) -> RunReport {
    let git = GitAdapter::new();
    let github = GitHubAdapter::new();
    report::status(config, &git, &github)
}

pub fn run_recycle(config: &Config) -> RunReport {
    let git = GitAdapter::new();
    let github = GitHubAdapter::new();
    report::recycle(config, &git, &github)
}

fn default_config_path() -> Result<PathBuf, AppError> {
    let home = env::var_os("HOME").ok_or(AppError::HomeDirectoryUnavailable)?;
    Ok(PathBuf::from(home).join(".config/bu/config.toml"))
}
