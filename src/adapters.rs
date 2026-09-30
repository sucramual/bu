use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::domain::{
    CurrentBranch, DirtyFile, GitOperation, OperationState, PullRequest, PullRequestLifecycle,
    PullRequestState, StandinState, WorktreeState,
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

/// What `bu prune` reads about a scratch worktree before checking its status.
pub struct ScratchHead {
    pub toplevel: PathBuf,
    pub branch: CurrentBranch,
    pub operation: OperationState,
}

#[derive(Debug)]
pub struct GitWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
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

    /// Resolves the worktree root, any in-progress operation, and the branch
    /// with its commit in one `rev-parse`, because each Git spawn costs more
    /// than the lookups. `--abbrev-ref=loose` shortens the branch exactly like
    /// `git symbolic-ref --short` and prints `HEAD` when HEAD is detached.
    pub fn scratch_head(&self, worktree: &Path) -> Result<ScratchHead, AdapterError> {
        let lines = rev_parse_with_operation_markers(
            worktree,
            &["--show-toplevel"],
            &["HEAD", "--abbrev-ref=loose", "HEAD"],
        )?;
        let [toplevel, markers @ .., commit, name] = lines.as_slice() else {
            unreachable!("rev_parse_with_operation_markers checks the line count");
        };
        let toplevel = PathBuf::from(toplevel);
        let toplevel = fs::canonicalize(&toplevel).map_err(|source| AdapterError::FileSystem {
            path: toplevel,
            source,
        })?;
        let branch = if name == "HEAD" {
            CurrentBranch::Detached
        } else {
            CurrentBranch::Attached {
                name: name.clone(),
                commit: commit.clone(),
            }
        };
        Ok(ScratchHead {
            toplevel,
            branch,
            operation: operation_state_from(worktree, markers),
        })
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
        let lines = rev_parse_with_operation_markers(bench, &[], &[])?;
        Ok(operation_state_from(bench, &lines))
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
        self.reject_ignored_reset_obstructions(bench)?;
        let output = run_command(bench, "git", &arguments(&["reset", "--hard", "HEAD"]))?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git reset", bench, output))
        }
    }

    fn reject_ignored_reset_obstructions(&self, bench: &Path) -> Result<(), AdapterError> {
        let ignored = run_command(
            bench,
            "git",
            &arguments(&[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
            ]),
        )?;
        if !ignored.success {
            return Err(unexpected_exit("git ls-files", bench, ignored));
        }
        let tracked = run_command(
            bench,
            "git",
            &arguments(&["ls-tree", "-r", "--name-only", "-z", "HEAD"]),
        )?;
        if !tracked.success {
            return Err(unexpected_exit("git ls-tree", bench, tracked));
        }
        let tracked_paths = tracked
            .raw_stdout
            .split(|byte| *byte == b'\0')
            .filter(|path| !path.is_empty())
            .map(path_from_bytes)
            .collect::<std::collections::HashSet<_>>();

        for ignored_path in ignored
            .raw_stdout
            .split(|byte| *byte == b'\0')
            .filter(|path| !path.is_empty())
            .map(path_from_bytes)
        {
            let tracked_obstruction = ignored_path
                .ancestors()
                .skip(1)
                .find(|ancestor| tracked_paths.contains(*ancestor))
                .map(Path::to_path_buf)
                .or_else(|| {
                    tracked_paths
                        .iter()
                        .find(|tracked_path| tracked_path.starts_with(&ignored_path))
                        .cloned()
                });
            if let Some(tracked_path) = tracked_obstruction {
                return Err(AdapterError::IgnoredResetObstruction {
                    cwd: bench.to_path_buf(),
                    ignored_path,
                    tracked_path,
                });
            }
        }
        Ok(())
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

    /// Removes a clean, unlocked linked worktree. Git itself refuses dirty or
    /// locked worktrees because `--force` is never passed.
    pub fn remove_worktree(&self, repository: &Path, worktree: &Path) -> Result<(), AdapterError> {
        let output = run_command(
            repository,
            "git",
            &[
                "worktree".to_owned(),
                "remove".to_owned(),
                worktree.to_string_lossy().into_owned(),
            ],
        )?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git worktree remove", repository, output))
        }
    }

    /// Deletes a local branch only while it still points at `expected_commit`.
    pub fn delete_branch_if_unchanged(
        &self,
        repository: &Path,
        branch: &str,
        expected_commit: &str,
    ) -> Result<(), AdapterError> {
        let branch_ref = format!("refs/heads/{branch}");
        let output = run_command(
            repository,
            "git",
            &arguments(&["update-ref", "-d", &branch_ref, expected_commit]),
        )?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git update-ref", repository, output))
        }
    }

    /// Removes `branch.<name>.*` settings. Git exits 128 with "no such section"
    /// when the branch never had any, which counts as success.
    pub fn remove_branch_config(
        &self,
        repository: &Path,
        branch: &str,
    ) -> Result<(), AdapterError> {
        let output = run_command(
            repository,
            "git",
            &arguments(&["config", "--remove-section", &format!("branch.{branch}")]),
        )?;
        if output.success || output.status == "128" && output.stderr.contains("no such section") {
            Ok(())
        } else {
            Err(unexpected_exit("git config", repository, output))
        }
    }

    pub fn prune_worktree_metadata(&self, repository: &Path) -> Result<(), AdapterError> {
        let output = run_command(repository, "git", &arguments(&["worktree", "prune"]))?;
        if output.success {
            Ok(())
        } else {
            Err(unexpected_exit("git worktree prune", repository, output))
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

/// Git paths whose presence means an operation is in progress, in the order
/// `rev_parse_with_operation_markers` asks for them.
const OPERATION_MARKERS: [(GitOperation, &str); 5] = [
    (GitOperation::Merge, "MERGE_HEAD"),
    (GitOperation::Rebase, "REBASE_HEAD"),
    (GitOperation::Rebase, "rebase-merge"),
    (GitOperation::Rebase, "rebase-apply"),
    (GitOperation::CherryPick, "CHERRY_PICK_HEAD"),
];

/// Runs `git rev-parse [leading] --git-path <marker>... [trailing]` and
/// returns one line per requested value. Each leading argument prints a line,
/// as does each trailing argument that is not an option. A path containing a newline would shift the lines, so
/// any other line count is an error rather than a guess.
fn rev_parse_with_operation_markers(
    cwd: &Path,
    leading: &[&str],
    trailing: &[&str],
) -> Result<Vec<String>, AdapterError> {
    let mut rev_parse = vec!["rev-parse".to_owned()];
    rev_parse.extend(leading.iter().map(|argument| (*argument).to_owned()));
    for (_, marker) in OPERATION_MARKERS {
        rev_parse.extend(["--git-path".to_owned(), marker.to_owned()]);
    }
    rev_parse.extend(trailing.iter().map(|argument| (*argument).to_owned()));
    let output = run_command(cwd, "git", &rev_parse)?;
    if !output.success {
        return Err(unexpected_exit("git rev-parse", cwd, output));
    }
    let expected = leading.len()
        + OPERATION_MARKERS.len()
        + trailing
            .iter()
            .filter(|argument| !argument.starts_with("--"))
            .count();
    parse_rev_parse_lines(&output.stdout, expected, cwd)
}

fn parse_rev_parse_lines(
    stdout: &str,
    expected: usize,
    cwd: &Path,
) -> Result<Vec<String>, AdapterError> {
    let lines: Vec<String> = stdout
        .strip_suffix('\n')
        .unwrap_or(stdout)
        .split('\n')
        .map(str::to_owned)
        .collect();
    if lines.len() != expected || lines.iter().any(String::is_empty) {
        return Err(AdapterError::InvalidRevParse {
            cwd: cwd.to_path_buf(),
            message: format!("expected {expected} non-empty lines, got {}", lines.len()),
        });
    }
    Ok(lines)
}

/// `marker_paths` holds one `--git-path` result per `OPERATION_MARKERS` entry.
fn operation_state_from(cwd: &Path, marker_paths: &[String]) -> OperationState {
    let mut operations: Vec<GitOperation> = Vec::new();
    for ((operation, _), marker_path) in OPERATION_MARKERS.iter().zip(marker_paths) {
        let marker_path = PathBuf::from(marker_path);
        let exists = if marker_path.is_absolute() {
            marker_path.exists()
        } else {
            cwd.join(marker_path).exists()
        };
        if exists && !operations.contains(operation) {
            operations.push(operation.clone());
        }
    }
    if operations.is_empty() {
        OperationState::Normal
    } else {
        OperationState::InProgress(operations)
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
    let mut bare = false;
    let mut locked = false;
    let mut saw_field = false;
    for field in output.split(|byte| *byte == b'\0') {
        if field.is_empty() {
            if let Some(path) = path.take() {
                worktrees.push(GitWorktree {
                    path,
                    branch: branch.take(),
                    bare: std::mem::take(&mut bare),
                    locked: std::mem::take(&mut locked),
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
        } else if field == b"bare" {
            bare = true;
        } else if field == b"locked" || field.starts_with(b"locked ") {
            locked = true;
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
    use super::{
        OPERATION_MARKERS, operation_state_from, parse_cwd_processes, parse_rev_parse_lines,
        parse_worktree_state, parse_worktrees,
    };
    use crate::domain::{CurrentBranch, GitOperation, OperationState, WorktreeState};
    use std::fs;
    use std::path::Path;

    #[test]
    fn scratch_head_matches_symbolic_ref_and_reports_detached_heads() {
        let repository =
            std::env::temp_dir().join(format!("bu-scratch-head-{}", std::process::id()));
        let git = |arguments: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=bu", "-c", "user.email=bu@example.com"])
                .args(arguments)
                .current_dir(&repository)
                .output()
                .expect("git runs");
            assert!(output.status.success(), "git {arguments:?} failed");
            String::from_utf8(output.stdout)
                .expect("utf-8")
                .trim()
                .to_owned()
        };
        fs::create_dir_all(&repository).expect("repository folder");
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["commit", "--quiet", "--allow-empty", "-m", "initial"]);
        git(&["tag", "feature/ambiguous"]);
        git(&["switch", "--quiet", "-c", "feature/ambiguous"]);
        let commit = git(&["rev-parse", "HEAD"]);
        let short_name = git(&["symbolic-ref", "--short", "HEAD"]);

        let attached = super::GitAdapter::new().scratch_head(&repository);
        git(&["switch", "--quiet", "--detach"]);
        let detached = super::GitAdapter::new().scratch_head(&repository);
        fs::remove_dir_all(&repository).expect("cleanup");

        let attached = attached.expect("attached head");
        assert_eq!(
            attached.toplevel,
            fs::canonicalize(std::env::temp_dir())
                .expect("temp")
                .join(repository.file_name().expect("name"))
        );
        assert!(matches!(attached.operation, OperationState::Normal));
        assert!(matches!(
            attached.branch,
            CurrentBranch::Attached { ref name, commit: ref head } if *name == short_name && *head == commit
        ));
        assert!(matches!(
            detached.expect("detached head").branch,
            CurrentBranch::Detached
        ));
    }

    #[test]
    fn splits_rev_parse_output_into_one_line_per_argument() {
        let lines = parse_rev_parse_lines("/top\n/git/MERGE_HEAD\n", 2, Path::new("/top"))
            .expect("two lines");

        assert_eq!(lines, ["/top", "/git/MERGE_HEAD"]);
    }

    #[test]
    fn rejects_rev_parse_output_with_an_unexpected_line_count() {
        let error = parse_rev_parse_lines(
            "/top\nwith-newline\n/git/MERGE_HEAD\n",
            2,
            Path::new("/top"),
        )
        .expect_err("a path with a newline shifts the lines");

        assert!(
            error
                .to_string()
                .contains("expected 2 non-empty lines, got 3")
        );
    }

    #[test]
    fn reports_each_in_progress_operation_once_in_marker_order() {
        let git_dir = std::env::temp_dir().join(format!("bu-markers-{}", std::process::id()));
        fs::create_dir_all(git_dir.join("rebase-merge")).expect("marker directory");
        fs::write(git_dir.join("CHERRY_PICK_HEAD"), "").expect("marker file");
        fs::write(git_dir.join("REBASE_HEAD"), "").expect("marker file");
        let marker_paths: Vec<String> = OPERATION_MARKERS
            .iter()
            .map(|(_, marker)| git_dir.join(marker).to_string_lossy().into_owned())
            .collect();

        let state = operation_state_from(Path::new("/unused"), &marker_paths);
        fs::remove_dir_all(&git_dir).expect("cleanup");

        let OperationState::InProgress(operations) = state else {
            panic!("markers exist, so operations are in progress");
        };
        assert_eq!(operations, [GitOperation::Rebase, GitOperation::CherryPick]);
    }

    #[test]
    fn resolves_relative_marker_paths_from_the_worktree() {
        let worktree = std::env::temp_dir().join(format!("bu-relative-{}", std::process::id()));
        fs::create_dir_all(worktree.join(".git")).expect("git directory");
        fs::write(worktree.join(".git/MERGE_HEAD"), "").expect("marker file");
        let marker_paths: Vec<String> = OPERATION_MARKERS
            .iter()
            .map(|(_, marker)| format!(".git/{marker}"))
            .collect();

        let state = operation_state_from(&worktree, &marker_paths);
        fs::remove_dir_all(&worktree).expect("cleanup");

        assert!(
            matches!(state, OperationState::InProgress(operations) if operations == [GitOperation::Merge])
        );
    }

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
        assert!(!worktrees[0].locked);
    }

    #[test]
    fn parses_locked_and_bare_worktree_records() {
        let worktrees = parse_worktrees(
            b"worktree /repository\0bare\0\0worktree /scratch\0HEAD abc123\0branch refs/heads/scratch\0locked on a removable drive\0\0worktree /plain-lock\0detached\0locked\0\0",
            Path::new("/repository"),
        )
        .expect("valid NUL-delimited worktree list");

        assert_eq!(worktrees.len(), 3);
        assert!(worktrees[0].bare && !worktrees[0].locked);
        assert!(!worktrees[1].bare && worktrees[1].locked);
        assert!(worktrees[2].locked);
        assert_eq!(worktrees[2].branch, None);
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

    #[test]
    fn parses_one_record_per_process_and_ignores_other_fields() {
        let processes = parse_cwd_processes(
            b"p265\nczsh\nfcwd\nn/Users/marcus/work tree\np302\ncGoogle Chrome Helper\nfcwd\nn/\np400\nfcwd\nn/tmp\n",
        )
        .expect("valid lsof output");

        assert_eq!(processes.len(), 3);
        assert_eq!(processes[0].pid, 265);
        assert_eq!(processes[0].command, "zsh");
        assert_eq!(processes[0].cwd, Path::new("/Users/marcus/work tree"));
        assert_eq!(processes[1].command, "Google Chrome Helper");
        assert_eq!(processes[1].cwd, Path::new("/"));
        assert_eq!(processes[2].command, "unknown");
    }

    #[test]
    fn parses_empty_output_and_processes_without_a_readable_directory() {
        assert!(parse_cwd_processes(b"").expect("empty output").is_empty());
        let processes = parse_cwd_processes(b"p1\nclaunchd\np2\ncnode\nfcwd\nn/work\n")
            .expect("valid lsof output");

        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 2);
    }

    #[test]
    fn rejects_a_file_name_before_any_process_id() {
        let error = parse_cwd_processes(b"fcwd\nn/work\n").expect_err("orphan file name");

        assert!(error.to_string().contains("before a process id"));
    }

    #[test]
    fn rejects_a_process_id_that_is_not_a_number() {
        let error = parse_cwd_processes(b"pabc\ncnode\nn/work\n").expect_err("invalid pid");

        assert!(error.to_string().contains("not a number"));
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
        Ok(pull_request_state(branch, pull_requests))
    }

    /// Looks up the pull requests of many head branches with one
    /// `gh api graphql` call per `MAX_BRANCHES_PER_QUERY` branches, because
    /// each `gh` start costs a process, Git calls to resolve the repository,
    /// and a network round trip. Each alias asks what
    /// `gh pr list --state all --head <branch> --limit 100` asks: any author,
    /// newest 100 first. `{owner}` and `{repo}` resolve the repository the same
    /// way `gh pr list` does, including `gh repo set-default`. Results keep the
    /// order of `branches`.
    pub fn pull_requests_for_branches(
        &self,
        repository: &Path,
        branches: &[&str],
    ) -> Result<Vec<PullRequestState>, AdapterError> {
        let mut states = Vec::with_capacity(branches.len());
        for batch in branches.chunks(MAX_BRANCHES_PER_QUERY) {
            states.extend(self.pull_request_batch(repository, batch)?);
        }
        Ok(states)
    }

    fn pull_request_batch(
        &self,
        repository: &Path,
        branches: &[&str],
    ) -> Result<Vec<PullRequestState>, AdapterError> {
        let mut query = "query($owner: String!, $name: String!".to_owned();
        let mut selections = String::new();
        let mut gh = arguments(&["api", "graphql", "-F", "owner={owner}", "-F", "name={repo}"]);
        for (index, branch) in branches.iter().enumerate() {
            query.push_str(&format!(", $b{index}: String!"));
            selections.push_str(&format!(
                " b{index}: pullRequests(headRefName: $b{index}, states: [OPEN, CLOSED, MERGED], \
                 first: 100, orderBy: {{field: CREATED_AT, direction: DESC}}) \
                 {{ nodes {{ number state mergedAt headRefName headRefOid }} }}"
            ));
            gh.extend(["-f".to_owned(), format!("b{index}={branch}")]);
        }
        query.push_str(&format!(
            ") {{ repository(owner: $owner, name: $name) {{{selections} }} }}"
        ));
        gh.extend(["-f".to_owned(), format!("query={query}")]);

        let output = run_command(repository, "gh", &gh)?;
        if !output.success {
            return Err(unexpected_exit("gh api graphql", repository, output));
        }
        let invalid = |source| AdapterError::InvalidJson {
            program: "gh api graphql".to_owned(),
            cwd: repository.to_path_buf(),
            source,
        };
        let mut response: GraphQlResponse =
            serde_json::from_str(&output.stdout).map_err(invalid)?;
        branches
            .iter()
            .enumerate()
            .map(|(index, branch)| {
                let connection = response
                    .data
                    .repository
                    .remove(&format!("b{index}"))
                    .ok_or_else(|| {
                        invalid(serde::de::Error::custom(format!(
                            "response is missing pull requests for {branch}"
                        )))
                    })?;
                Ok(pull_request_state(branch, connection.nodes))
            })
            .collect()
    }
}

/// Keeps each GraphQL document small enough for GitHub's limits on query
/// size and requested nodes (100 per branch).
const MAX_BRANCHES_PER_QUERY: usize = 50;

fn pull_request_state(branch: &str, pull_requests: Vec<GitHubPullRequest>) -> PullRequestState {
    PullRequestState::Matches(
        pull_requests
            .into_iter()
            .filter(|pull_request| pull_request.head_ref_name == branch)
            .map(|pull_request| PullRequest {
                number: pull_request.number,
                lifecycle: match pull_request.state.as_str() {
                    "MERGED" if pull_request.merged_at.is_some() => PullRequestLifecycle::Merged,
                    "OPEN" => PullRequestLifecycle::Open,
                    _ => PullRequestLifecycle::ClosedUnmerged,
                },
                head_commit: pull_request.head_ref_oid,
            })
            .collect(),
    )
}

#[derive(Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Deserialize)]
struct GraphQlData {
    repository: std::collections::HashMap<String, PullRequestConnection>,
}

#[derive(Deserialize)]
struct PullRequestConnection {
    nodes: Vec<GitHubPullRequest>,
}

pub struct ProcessAdapter;

/// A running process and its current working directory, as `lsof` reports it.
#[derive(Debug)]
pub struct CwdProcess {
    pub pid: u32,
    pub command: String,
    pub cwd: PathBuf,
}

impl ProcessAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Lists the current directory of every process `lsof` can inspect. It
    /// never uses `+D`, which walks whole trees such as `node_modules`.
    pub fn cwd_processes(&self, cwd: &Path) -> Result<Vec<CwdProcess>, AdapterError> {
        let output = run_command(cwd, "lsof", &arguments(&["-d", "cwd", "-Fpcn"]))?;
        // lsof exits 1 when it cannot inspect some processes but still prints
        // the rest; empty output with that status is a real failure.
        let partial = output.status == "1" && !output.stdout.trim().is_empty();
        if !output.success && !partial {
            return Err(unexpected_exit("lsof", cwd, output));
        }

        parse_cwd_processes(&output.raw_stdout)
    }
}

fn parse_cwd_processes(output: &[u8]) -> Result<Vec<CwdProcess>, AdapterError> {
    let invalid = |message: String| AdapterError::InvalidProcessList { message };
    let mut processes = Vec::new();
    let mut current: Option<(u32, Option<String>)> = None;
    for line in output.split(|byte| *byte == b'\n') {
        let Some((&field, value)) = line.split_first() else {
            continue;
        };
        match field {
            b'p' => {
                let pid = std::str::from_utf8(value)
                    .ok()
                    .and_then(|pid| pid.parse().ok())
                    .ok_or_else(|| {
                        invalid(format!(
                            "process id {:?} is not a number",
                            String::from_utf8_lossy(value)
                        ))
                    })?;
                current = Some((pid, None));
            }
            b'c' => {
                let (_, command) = current
                    .as_mut()
                    .ok_or_else(|| invalid("command appears before a process id".to_owned()))?;
                *command = Some(String::from_utf8_lossy(value).into_owned());
            }
            b'n' => {
                let (pid, command) = current
                    .as_ref()
                    .ok_or_else(|| invalid("file name appears before a process id".to_owned()))?;
                if value.is_empty() {
                    return Err(invalid(format!("process {pid} has an empty file name")));
                }
                processes.push(CwdProcess {
                    pid: *pid,
                    command: command.clone().unwrap_or_else(|| "unknown".to_owned()),
                    cwd: path_from_bytes(value),
                });
            }
            _ => {}
        }
    }
    Ok(processes)
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
