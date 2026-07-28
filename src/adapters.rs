use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::domain::{
    CurrentBranch, GitOperation, OperationState, PullRequest, PullRequestState, StandinState,
    WorktreeState,
};
use crate::error::AdapterError;

#[derive(Debug)]
struct CommandOutput {
    arguments: Vec<String>,
    success: bool,
    status: String,
    stdout: String,
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

impl GitAdapter {
    pub fn new() -> Self {
        Self
    }

    pub fn worktree_state(&self, bench: &Path) -> Result<WorktreeState, AdapterError> {
        let output = run_command(
            bench,
            "git",
            &arguments(&["status", "--porcelain=v1", "--untracked-files=all"]),
        )?;
        if !output.success {
            return Err(unexpected_exit("git status", bench, output));
        }

        Ok(if output.stdout.is_empty() {
            WorktreeState::Clean
        } else {
            WorktreeState::Dirty
        })
    }

    pub fn same_repository(&self, repository: &Path, bench: &Path) -> Result<bool, AdapterError> {
        Ok(self.git_common_dir(repository)? == self.git_common_dir(bench)?)
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
        let output = run_command(bench, "git", &arguments(&["switch", branch]))?;
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
        let output = run_command(
            repository,
            "git",
            &arguments(&["worktree", "list", "--porcelain"]),
        )?;
        if !output.success {
            return Err(unexpected_exit("git worktree list", repository, output));
        }

        let mut worktree = None;
        for line in output.stdout.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                worktree = Some(PathBuf::from(path));
            } else if let Some(branch) = line.strip_prefix("branch ")
                && branch == standin_ref
                && worktree.as_deref() != Some(bench)
            {
                return Ok(worktree);
            }
        }
        Ok(None)
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
