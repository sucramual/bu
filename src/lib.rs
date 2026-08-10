mod adapters;
mod domain;
mod error;
mod report;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use adapters::{GitAdapter, GitHubAdapter};
pub use domain::Config;
use domain::{BenchConfig, RepositoryConfig};
pub use error::AppError;
pub use report::{RunReport, StatusFormat, format_report, format_status_report};

pub fn load_config(path: Option<PathBuf>) -> Result<Config, AppError> {
    let is_default = path.is_none();
    let path = path.unwrap_or(default_config_path()?);
    if is_default && !path.exists() {
        return create_default_config(&path);
    }

    let source = read_config_source(&path)?;
    let mut config: Config = toml::from_str(&source).map_err(|source| AppError::ConfigParse {
        path: path.clone(),
        source,
    })?;

    if is_default && add_discovered_benches(&mut config)? {
        write_config(&path, &config)?;
    }

    Ok(config)
}

fn create_default_config(path: &Path) -> Result<Config, AppError> {
    let cwd = env::current_dir().map_err(AppError::CurrentDirectory)?;
    let git = GitAdapter::new();
    let worktrees = git
        .worktrees(&cwd)
        .map_err(|error| AppError::ConfigDiscovery {
            path: cwd.clone(),
            message: error.to_string(),
        })?;
    let repository = worktrees
        .iter()
        .find(|worktree| worktree.branch.as_deref() == Some("main"))
        .map(|worktree| worktree.path.clone())
        .ok_or_else(|| AppError::ConfigDiscovery {
            path: cwd,
            message: "the repository has no worktree on the main branch".to_owned(),
        })?;
    let mut config = Config {
        repository: RepositoryConfig {
            path: repository,
            remote: "origin".to_owned(),
            main_branch: "main".to_owned(),
        },
        benches: Vec::new(),
    };
    add_benches_from_worktrees(&mut config, &worktrees);
    write_config(path, &config)?;
    Ok(config)
}

fn read_config_source(path: &Path) -> Result<String, AppError> {
    fs::read_to_string(path).map_err(|source| AppError::ConfigRead {
        path: path.to_path_buf(),
        source,
    })
}

fn add_discovered_benches(config: &mut Config) -> Result<bool, AppError> {
    let git = GitAdapter::new();
    let worktrees =
        git.worktrees(&config.repository.path)
            .map_err(|error| AppError::ConfigDiscovery {
                path: config.repository.path.clone(),
                message: error.to_string(),
            })?;
    Ok(add_benches_from_worktrees(config, &worktrees))
}

fn add_benches_from_worktrees(config: &mut Config, worktrees: &[adapters::GitWorktree]) -> bool {
    let Some(parent) = config.repository.path.parent() else {
        return false;
    };
    let Some(repository_name) = config
        .repository
        .path
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    let prefix = format!("{repository_name}-");
    let mut discovered: Vec<_> = worktrees
        .iter()
        .filter_map(|worktree| {
            if worktree.path.parent() != Some(parent) {
                return None;
            }
            let bench_name = worktree.path.file_name()?.to_str()?;
            let slot = bench_name.strip_prefix(&prefix)?;
            if slot.len() != 2 || !slot.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some(BenchConfig {
                path: worktree.path.clone(),
                standin_branch: format!("{}-{slot}", config.repository.main_branch),
            })
        })
        .collect();
    discovered.sort_by(|left, right| left.path.cmp(&right.path));

    let original_len = config.benches.len();
    for bench in discovered {
        if !config
            .benches
            .iter()
            .any(|configured| configured.path == bench.path)
        {
            config.benches.push(bench);
        }
    }
    config.benches.len() != original_len
}

fn write_config(path: &Path, config: &Config) -> Result<(), AppError> {
    let source = toml::to_string_pretty(config).map_err(|source| AppError::ConfigSerialize {
        path: path.to_path_buf(),
        source,
    })?;
    let Some(parent) = path.parent() else {
        return Err(AppError::ConfigWrite {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "config path has no parent directory",
            ),
        });
    };
    fs::create_dir_all(parent).map_err(|source| AppError::ConfigWrite {
        path: path.to_path_buf(),
        source,
    })?;
    fs::write(path, source).map_err(|source| AppError::ConfigWrite {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
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
