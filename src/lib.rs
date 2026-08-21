mod adapters;
mod domain;
mod error;
mod report;

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use adapters::{GitAdapter, GitHubAdapter};
pub use domain::Config;
use domain::{BenchConfig, RepositoryConfig};
pub use error::AppError;
pub use report::{RunReport, StatusFormat, format_report, format_status_report};

pub fn load_config(path: Option<PathBuf>) -> Result<Config, AppError> {
    let is_default = path.is_none();
    let path = path.unwrap_or(default_config_path()?);
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(source) if is_default && source.kind() == std::io::ErrorKind::NotFound => {
            return create_default_config(&path);
        }
        Err(source) => {
            return Err(AppError::ConfigRead {
                path: path.clone(),
                source,
            });
        }
    };
    let mut config: Config = toml::from_str(&source).map_err(|source| AppError::ConfigParse {
        path: path.clone(),
        source,
    })?;

    if is_default {
        let discovered = add_discovered_benches(&mut config)?;
        if !discovered.is_empty() {
            append_benches(&path, &source, &discovered)?;
        }
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
        .first()
        .map(|worktree| worktree.path.clone())
        .ok_or_else(|| AppError::ConfigDiscovery {
            path: cwd,
            message: "the repository has no primary worktree".to_owned(),
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

fn add_discovered_benches(config: &mut Config) -> Result<Vec<BenchConfig>, AppError> {
    let git = GitAdapter::new();
    let worktrees =
        git.worktrees(&config.repository.path)
            .map_err(|error| AppError::ConfigDiscovery {
                path: config.repository.path.clone(),
                message: error.to_string(),
            })?;
    Ok(add_benches_from_worktrees(config, &worktrees))
}

fn add_benches_from_worktrees(
    config: &mut Config,
    worktrees: &[adapters::GitWorktree],
) -> Vec<BenchConfig> {
    let Some(parent) = config.repository.path.parent() else {
        return Vec::new();
    };
    let Some(repository_name) = config
        .repository
        .path
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Vec::new();
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

    let new_benches: Vec<_> = discovered
        .into_iter()
        .filter(|bench| {
            !config
                .benches
                .iter()
                .any(|configured| configured.path == bench.path)
        })
        .collect();
    config.benches.extend(new_benches.iter().cloned());
    new_benches
}

fn write_config(path: &Path, config: &Config) -> Result<(), AppError> {
    let source = toml::to_string_pretty(config).map_err(|source| AppError::ConfigSerialize {
        path: path.to_path_buf(),
        source,
    })?;
    write_config_source(path, &source)
}

fn append_benches(path: &Path, source: &str, benches: &[BenchConfig]) -> Result<(), AppError> {
    let mut updated = source.to_owned();
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    for bench in benches {
        updated.push('\n');
        updated.push_str("[[benches]]\n");
        updated.push_str(&toml::to_string_pretty(bench).map_err(|source| {
            AppError::ConfigSerialize {
                path: path.to_path_buf(),
                source,
            }
        })?);
    }
    write_config_source(path, &updated)
}

fn write_config_source(path: &Path, source: &str) -> Result<(), AppError> {
    let destination = config_destination(path)?;
    let Some(parent) = destination.parent() else {
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
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| AppError::ConfigWrite {
            path: path.to_path_buf(),
            source,
        })?;
    if let Ok(metadata) = fs::metadata(&destination) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|source| AppError::ConfigWrite {
                path: path.to_path_buf(),
                source,
            })?;
    }
    temporary
        .write_all(source.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| AppError::ConfigWrite {
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .persist(&destination)
        .map_err(|source| AppError::ConfigWrite {
            path: path.to_path_buf(),
            source: source.error,
        })?;

    Ok(())
}

fn config_destination(path: &Path) -> Result<PathBuf, AppError> {
    let mut destination = path.to_path_buf();
    for _ in 0..40 {
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target =
                    fs::read_link(&destination).map_err(|source| AppError::ConfigWrite {
                        path: path.to_path_buf(),
                        source,
                    })?;
                destination = if target.is_absolute() {
                    target
                } else {
                    destination
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(target)
                };
            }
            Ok(_) => return Ok(destination),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(destination),
            Err(source) => {
                return Err(AppError::ConfigWrite {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
    Err(AppError::ConfigWrite {
        path: path.to_path_buf(),
        source: std::io::Error::other("too many config symlinks"),
    })
}

pub fn run_status(config: &Config) -> RunReport {
    let git = GitAdapter::new();
    let github = GitHubAdapter::new();
    report::status(config, &git, &github)
}

pub fn run_recycle(config: &Config, force: bool) -> RunReport {
    let git = GitAdapter::new();
    let github = GitHubAdapter::new();
    report::recycle(config, &git, &github, force)
}

fn default_config_path() -> Result<PathBuf, AppError> {
    let home = env::var_os("HOME").ok_or(AppError::HomeDirectoryUnavailable)?;
    Ok(PathBuf::from(home).join(".config/bu/config.toml"))
}
