use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("could not determine the home directory for the default config path")]
    HomeDirectoryUnavailable,
    #[error("could not read configuration at {path}: {source}")]
    ConfigRead { path: PathBuf, source: io::Error },
    #[error("could not parse configuration at {path}: {source}")]
    ConfigParse {
        path: PathBuf,
        source: toml::de::Error,
    },
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
}
