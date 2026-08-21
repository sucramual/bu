use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::domain::{
    CurrentBranch, DirtyFile, GitOperation, OperationState, PullRequest, PullRequestState,
    StandinState, WorktreeState,
};
use crate::error::AdapterError;

#[derive(Debug)]
struct CommandOutput {
    arguments: Vec<String>,
    success: bool,
    status: String,
    stdout: String,
    raw_stdout: Vec<u8>,
    stderr: String,
}

fn run_command(
    cwd: &Path,
    program: &str,
    arguments: &[String],
) -> Result<CommandOutput, AdapterError> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .output()
        .map_err(|source| AdapterError::Spawn {
            program: program.to_owned(),
            cwd: cwd.to_path_buf(),
            source,
        })?;

    Ok(CommandOutput {
        arguments: arguments.to_vec(),
        success: output.status.success(),
        status: output.status.code().map_or_else(
            || "terminated by signal".to_owned(),
            |code| code.to_string(),
        ),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        raw_stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn unexpected_exit(program: &str, cwd: &Path, output: CommandOutput) -> AdapterError {
    AdapterError::UnexpectedExit {
        program: program.to_owned(),
        cwd: cwd.to_path_buf(),
        arguments: output.arguments,
        status: output.status,
        stderr: output.stderr,
    }
}

pub struct GitAdapter;

#[derive(Debug)]
pub struct GitWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
}

impl GitAdapter {
    pub fn new() -> Self {
        Self
    }

    pub fn worktree_state(&self, bench: &Path) -> Result<WorktreeState, AdapterError> {
        let output = run_command(
            bench,
            "git",
            &arguments(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
        )?;
        if !output.success {
            return Err(unexpected_exit("git status", bench, output));
        }

        parse_worktree_state(&output.raw_stdout, bench)
    }

    pub fn same_repository(&self, repository: &Path, bench: &Path) -> Result<bool, AdapterError> {
        Ok(self.git_common_dir(repository)? == self.git_common_dir(bench)?)
    }

    pub fn worktrees(&self, repository: &Path) -> Result<Vec<GitWorktree>, AdapterError> {
        let output = run_command(
            repository,
            "git",
            &arguments(&["worktree", "list", "--porcelain", "-z"]),
        )?;
        if !output.success {
            return Err(unexpected_exit("git worktree list", repository, output));
        }

        parse_worktrees(&output.raw_stdout, repository)
    }

    pub fn current_branch(&self, bench: &Path) -> Result<CurrentBranch, AdapterError> {
        let output = run_command(
            bench,
            "git",
            &arguments(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
        )?;
        if output.success {
            let name = output.stdout.trim().to_owned();
            let commit = self.ref_commit(bench, "HEAD")?;
            return Ok(CurrentBranch::Attached { name, commit });
        }
        if output.status == "1" {
            return Ok(CurrentBranch::Detached);
        }

        Err(unexpected_exit("git symbolic-ref", bench, output))
    }

    pub fn operation_state(&self, bench: &Path) -> Result<OperationState, AdapterError> {
        let mut operations = Vec::new();
        for (operation, paths) in [
            (GitOperation::Merge, ["MERGE_HEAD"].as_slice()),
            (
                GitOperation::Rebase,
                ["REBASE_HEAD", "rebase-merge", "rebase-apply"].as_slice(),
            ),
            (GitOperation::CherryPick, ["CHERRY_PICK_HEAD"].as_slice()),
        ] {
            let exists = paths.iter().try_fold(false, |found, path| {
                if found {
                    Ok(true)
                } else {
                    self.git_path_exists(bench, path)
                }
            })?;
            if exists {
                operations.push(operation);
            }
        }

        Ok(if operations.is_empty() {
            OperationState::Normal
        } else {
            OperationState::InProgress(operations)
        })
    }

    fn git_path_exists(&self, bench: &Path, git_path: &str) -> Result<bool, AdapterError> {
        let output = run_command(
            bench,
            "git",
            &arguments(&["rev-parse", "--git-path", git_path]),
        )?;
        if !output.success {
            return Err(unexpected_exit("git rev-parse", bench, output));
        }
        let path = PathBuf::from(output.stdout.trim());
        Ok(if path.is_absolute() {
            path.exists()
        } else {
            bench.join(path).exists()
        })
    }

    fn git_common_dir(&self, path: &Path) -> Result<PathBuf, AdapterError> {
        let output = run_command(path, "git", &arguments(&["rev-parse", "--git-common-dir"]))?;
        if !output.success {
            return Err(unexpected_exit("git rev-parse", path, output));
        }
        let common_dir = PathBuf::from(output.stdout.trim());
        let common_dir = if common_dir.is_absolute() {
            common_dir
        } else {
            path.join(common_dir)
        };
        fs::canonicalize(&common_dir).map_err(|source| AdapterError::FileSystem {
            path: common_dir,
            source,
        })
    }

    pub fn standin_state(
        &self,
        repository: &Path,
        bench: &Path,
        remote: &str,
        main_branch: &str,
        standin_branch: &str,
    ) -> Result<StandinState, AdapterError> {
        let standin_ref = format!("refs/heads/{standin_branch}");
        if !self.ref_exists(repository, &standin_ref)? {
            return Ok(StandinState::Missing);
        }
        if let Some(worktree) = self.standin_worktree(repository, bench, &standin_ref)? {
            return Ok(StandinState::CheckedOutElsewhere(worktree));
        }

        let upstream_ref = format!("refs/remotes/{remote}/{main_branch}");
        if !self.ref_exists(repository, &upstream_ref)? {
            return Ok(StandinState::UpstreamNotFetched);
        }

        let output = run_command(
            repository,
            "git",
            &arguments(&["merge-base", "--is-ancestor", &standin_ref, &upstream_ref]),
        )?;
        if output.success {
            let upstream = run_command(
                repository,
                "git",
                &arguments(&["rev-parse", "--verify", &upstream_ref]),
            )?;
            if !upstream.success {
                return Err(unexpected_exit("git rev-parse", repository, upstream));
            }
            let standin = self.ref_commit(repository, &standin_ref)?;
            return Ok(StandinState::Ready {
                standin_commit: standin,
                upstream_commit: upstream.stdout.trim().to_owned(),
            });
        }
        if output.status == "1" {
            return Ok(StandinState::NotFastForwardable);
        }

        Err(unexpected_exit("git merge-base", repository, output))
    }

    fn ref_exists(&self, repository: &Path, reference: &str) -> Result<bool, AdapterError> {
        let output = run_command(
            repository,
            "git",
            &arguments(&["show-ref", "--verify", "--quiet", reference]),
        )?;
        if output.success {
            return Ok(true);
        }
        if output.status == "1" {
            return Ok(false);
        }

        Err(unexpected_exit("git show-ref", repository, output))
    }

    pub fn ref_commit(&self, repository: &Path, reference: &str) -> Result<String, AdapterError> {
        let output = run_command(
            repository,
            "git",
            &arguments(&["rev-parse", "--verify", reference]),
        )?;
        if !output.success {
            return Err(unexpected_exit("git rev-parse", repository, output));
        }
        Ok(output.stdout.trim().to_owned())
    }

    pub fn optional_ref_commit(
        &self,
        repository: &Path,
        reference: &str,
    ) -> Result<Option<String>, AdapterError> {
        if self.ref_exists(repository, reference)? {
            self.ref_commit(repository, reference).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn reset_hard(&self, bench: &Path) -> Result<(), AdapterError> {
        let output = run_command(bench, "git", &arguments(&["reset", "--hard", "HEAD"]))?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git reset", bench, output))
        }
    }

    pub fn clean_untracked(&self, bench: &Path) -> Result<(), AdapterError> {
        let output = run_command(bench, "git", &arguments(&["clean", "-fd"]))?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git clean", bench, output))
        }
    }

    pub fn fetch_main(
        &self,
        repository: &Path,
        remote: &str,
        main_branch: &str,
    ) -> Result<(), AdapterError> {
        let remote_ref = format!("refs/remotes/{remote}/{main_branch}");
        let source_ref = format!("refs/heads/{main_branch}:{remote_ref}");
        let output = run_command(
            repository,
            "git",
            &arguments(&["fetch", "--no-tags", remote, &source_ref]),
        )?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git fetch", repository, output))
        }
    }

    pub fn advance_standin(
        &self,
        repository: &Path,
        standin_branch: &str,
        expected_commit: &str,
        upstream_commit: &str,
    ) -> Result<(), AdapterError> {
        let standin_ref = format!("refs/heads/{standin_branch}");
        let output = run_command(
            repository,
            "git",
            &arguments(&["update-ref", &standin_ref, upstream_commit, expected_commit]),
        )?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git update-ref", repository, output))
        }
    }

    pub fn switch_branch(&self, bench: &Path, branch: &str) -> Result<(), AdapterError> {
        let output = run_command(
            bench,
            "git",
            &arguments(&["switch", "--no-overwrite-ignore", branch]),
        )?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git switch", bench, output))
        }
    }

    fn standin_worktree(
        &self,
        repository: &Path,
        bench: &Path,
        standin_ref: &str,
    ) -> Result<Option<PathBuf>, AdapterError> {
        let standin_branch = standin_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(standin_ref);
        Ok(self
            .worktrees(repository)?
            .into_iter()
            .find(|worktree| {
                worktree.branch.as_deref() == Some(standin_branch) && worktree.path != bench
            })
            .map(|worktree| worktree.path))
    }
}

fn parse_worktrees(output: &[u8], repository: &Path) -> Result<Vec<GitWorktree>, AdapterError> {
    if output.is_empty() {
        return Ok(Vec::new());
    }
    if !output.ends_with(b"\0\0") {
        return Err(AdapterError::InvalidWorktreeList {
            cwd: repository.to_path_buf(),
            message: "output is not terminated by an empty NUL-delimited record".to_owned(),
        });
    }

    let mut worktrees = Vec::new();
    let mut path = None;
    let mut branch = None;
    let mut saw_field = false;
    for field in output.split(|byte| *byte == b'\0') {
        if field.is_empty() {
            if let Some(path) = path.take() {
                worktrees.push(GitWorktree {
                    path,
                    branch: branch.take(),
                });
                saw_field = false;
            } else if saw_field {
                return Err(AdapterError::InvalidWorktreeList {
                    cwd: repository.to_path_buf(),
                    message: "record is missing its worktree path".to_owned(),
                });
            }
            continue;
        }

        saw_field = true;
        if let Some(value) = field.strip_prefix(b"worktree ") {
            if path.is_some() {
                return Err(AdapterError::InvalidWorktreeList {
                    cwd: repository.to_path_buf(),
                    message: "record contains more than one worktree path".to_owned(),
                });
            }
            if value.is_empty() {
                return Err(AdapterError::InvalidWorktreeList {
                    cwd: repository.to_path_buf(),
                    message: "record has an empty worktree path".to_owned(),
                });
            }
            path = Some(path_from_bytes(value));
        } else if let Some(value) = field.strip_prefix(b"branch refs/heads/") {
            branch = Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    Ok(worktrees)
}

#[cfg(unix)]
fn path_from_bytes(value: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(OsString::from_vec(value.to_vec()))
}

#[cfg(not(unix))]
fn path_from_bytes(value: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(value).into_owned())
}

fn parse_worktree_state(output: &[u8], bench: &Path) -> Result<WorktreeState, AdapterError> {
    if output.is_empty() {
        return Ok(WorktreeState::Clean);
    }

    let mut files = Vec::new();
    let mut offset = 0;
    while offset < output.len() {
        let record_end = output[offset..]
            .iter()
            .position(|byte| *byte == b'\0')
            .map(|position| offset + position)
            .ok_or_else(|| invalid_status(bench, "record is not NUL-terminated"))?;
        let record = &output[offset..record_end];
        offset = record_end + 1;

        if record.len() < 4 || record[2] != b' ' {
            return Err(invalid_status(
                bench,
                "record is missing its two-character status",
            ));
        }
        let index_status = char::from(record[0]);
        let worktree_status = char::from(record[1]);
        if !is_status_code(index_status) || !is_status_code(worktree_status) {
            return Err(invalid_status(bench, "status code is not valid porcelain"));
        }
        let path = PathBuf::from(String::from_utf8_lossy(&record[3..]).into_owned());
        if path.as_os_str().is_empty() {
            return Err(invalid_status(bench, "record is missing a path"));
        }

        let original_path = if matches!(index_status, 'R' | 'C')
            || matches!(worktree_status, 'R' | 'C')
        {
            let source_end = output[offset..]
                .iter()
                .position(|byte| *byte == b'\0')
                .map(|position| offset + position)
                .ok_or_else(|| {
                    invalid_status(bench, "rename or copy record is missing its source path")
                })?;
            let source =
                PathBuf::from(String::from_utf8_lossy(&output[offset..source_end]).into_owned());
            offset = source_end + 1;
            if source.as_os_str().is_empty() {
                return Err(invalid_status(
                    bench,
                    "rename or copy record has an empty source path",
                ));
            }
            Some(source)
        } else {
            None
        };

        files.push(DirtyFile {
            index_status,
            worktree_status,
            path,
            original_path,
        });
    }

    Ok(WorktreeState::Dirty { files })
}

fn invalid_status(bench: &Path, message: &str) -> AdapterError {
    AdapterError::InvalidStatus {
        cwd: bench.to_path_buf(),
        message: message.to_owned(),
    }
}

fn is_status_code(status: char) -> bool {
    matches!(
        status,
        ' ' | 'M' | 'T' | 'A' | 'D' | 'R' | 'C' | 'U' | '?' | '!'
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_worktree_state, parse_worktrees};
    use crate::domain::WorktreeState;
    use std::path::Path;

    #[test]
    fn parses_nul_delimited_worktree_paths_with_newlines() {
        let worktrees = parse_worktrees(
            b"worktree /repository-01\nscratch\0HEAD abc123\0branch refs/heads/feature/newline\0\0",
            Path::new("/repository"),
        )
        .expect("valid NUL-delimited worktree list");

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].path, Path::new("/repository-01\nscratch"));
        assert_eq!(worktrees[0].branch.as_deref(), Some("feature/newline"));
    }

    #[test]
    fn parses_nul_delimited_dirty_files_and_rename_sources() {
        let state = parse_worktree_state(
            b"M  staged.txt\0 M unstaged.txt\0?? untracked.txt\0R  renamed.txt\0original.txt\0C  copied.txt\0source.txt\0",
            Path::new("/bench"),
        )
        .expect("valid porcelain");

        let WorktreeState::Dirty { files } = state else {
            panic!("dirty output should produce files");
        };
        assert_eq!(files.len(), 5);
        assert_eq!(files[0].index_status, 'M');
        assert_eq!(files[1].worktree_status, 'M');
        assert_eq!(files[2].path, Path::new("untracked.txt"));
        assert_eq!(files[3].path, Path::new("renamed.txt"));
        assert_eq!(
            files[3].original_path.as_deref(),
            Some(Path::new("original.txt"))
        );
        assert_eq!(files[4].path, Path::new("copied.txt"));
        assert_eq!(
            files[4].original_path.as_deref(),
            Some(Path::new("source.txt"))
        );
    }

    #[test]
    fn rejects_a_rename_without_its_source_path() {
        let error = parse_worktree_state(b"R  renamed.txt\0", Path::new("/bench"))
            .expect_err("incomplete rename is invalid");

        assert!(error.to_string().contains("source path"));
    }

    #[test]
    fn rejects_an_invalid_porcelain_status_code() {
        let error = parse_worktree_state(b"Z  invalid.txt\0", Path::new("/bench"))
            .expect_err("unknown status code is invalid");

        assert!(error.to_string().contains("not valid porcelain"));
    }
}

pub struct GitHubAdapter;

impl GitHubAdapter {
    pub fn new() -> Self {
        Self
    }

    pub fn pull_requests(
        &self,
        repository: &Path,
        branch: &str,
    ) -> Result<PullRequestState, AdapterError> {
        let output = run_command(
            repository,
            "gh",
            &arguments(&[
                "pr",
                "list",
                "--state",
                "all",
                "--head",
                branch,
                "--json",
                "number,state,mergedAt,headRefName,headRefOid",
                "--limit",
                "100",
            ]),
        )?;
        if !output.success {
            return Err(unexpected_exit("gh pr list", repository, output));
        }

        let pull_requests: Vec<GitHubPullRequest> =
            serde_json::from_str(&output.stdout).map_err(|source| AdapterError::InvalidJson {
                program: "gh pr list".to_owned(),
                cwd: repository.to_path_buf(),
                source,
            })?;
        Ok(PullRequestState::Matches(
            pull_requests
                .into_iter()
                .filter(|pull_request| pull_request.head_ref_name == branch)
                .map(|pull_request| PullRequest {
                    number: pull_request.number,
                    merged: pull_request.state == "MERGED" && pull_request.merged_at.is_some(),
                    head_commit: pull_request.head_ref_oid,
                })
                .collect(),
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubPullRequest {
    number: u64,
    state: String,
    merged_at: Option<String>,
    head_ref_name: String,
    head_ref_oid: String,
}
