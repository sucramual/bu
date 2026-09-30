use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("could not determine the home directory for the default config path")]
    HomeDirectoryUnavailable,
    #[error("could not determine the current directory while creating the default config: {0}")]
    CurrentDirectory(#[source] io::Error),
    #[error("could not read configuration at {path}: {source}")]
    ConfigRead { path: PathBuf, source: io::Error },
    #[error("could not parse configuration at {path}: {source}")]
    ConfigParse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("could not discover benches from repository at {path}: {message}")]
    ConfigDiscovery { path: PathBuf, message: String },
    #[error("could not serialize configuration at {path}: {source}")]
    ConfigSerialize {
        path: PathBuf,
        source: toml::ser::Error,
    },
    #[error("could not write configuration at {path}: {source}")]
    ConfigWrite { path: PathBuf, source: io::Error },
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("could not start {program} in {cwd}: {source}")]
    Spawn {
        program: String,
        cwd: PathBuf,
        source: io::Error,
    },
    #[error("could not resolve {path}: {source}")]
    FileSystem { path: PathBuf, source: io::Error },
    #[error("{program} {arguments:?} in {cwd} exited with unexpected status {status}: {stderr}")]
    UnexpectedExit {
        program: String,
        cwd: PathBuf,
        arguments: Vec<String>,
        status: String,
        stderr: String,
    },
    #[error("could not parse {program} output in {cwd}: {source}")]
    InvalidJson {
        program: String,
        cwd: PathBuf,
        source: serde_json::Error,
    },
    #[error("could not parse git status output in {cwd}: {message}")]
    InvalidStatus { cwd: PathBuf, message: String },
    #[error("could not parse git worktree output in {cwd}: {message}")]
    InvalidWorktreeList { cwd: PathBuf, message: String },
    #[error("could not parse git rev-parse output in {cwd}: {message}")]
    InvalidRevParse { cwd: PathBuf, message: String },
    #[error("could not parse lsof output: {message}")]
    InvalidProcessList { message: String },
    #[error(
        "hard reset would delete ignored path {ignored_path} obstructing tracked path {tracked_path} in {cwd}"
    )]
    IgnoredResetObstruction {
        cwd: PathBuf,
        ignored_path: PathBuf,
        tracked_path: PathBuf,
    },
}
