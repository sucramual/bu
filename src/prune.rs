use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use crate::adapters::{GitAdapter, GitHubAdapter, GitWorktree};
use crate::domain::{
    BranchProtection, Config, CurrentBranch, ProtectedBranches, PruneSkipReason, PullRequestState,
    ScratchDecision, ScratchObservation, decide_scratch, scratch_needs_pull_requests,
};
use crate::report::{
    CYAN, DIM_GRAY, GREEN, RED, RowStyle, YELLOW, branch_name, format_operations, unique_labels,
    write_dirty_files, write_marker_row,
};

#[derive(Debug)]
pub struct PruneReport {
    pub dry_run: bool,
    pub listing_failure: Option<String>,
    pub items: Vec<PruneItem>,
}

#[derive(Debug)]
pub struct PruneItem {
    pub path: PathBuf,
    pub branch: String,
    pub outcome: PruneOutcome,
}

#[derive(Debug)]
pub enum PruneOutcome {
    Prunable {
        pull_request: u64,
    },
    Pruned {
        pull_request: u64,
    },
    BranchKept {
        pull_request: u64,
        current_commit: Option<String>,
    },
    Stale,
    Cleaned,
    StaleKept {
        missing: PathBuf,
        is_bench: bool,
    },
    Skipped(PruneSkipReason),
    Failed {
        summary: &'static str,
        error: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct PruneFormat {
    pub verbose: bool,
    pub use_color: bool,
}

impl PruneReport {
    pub fn has_failures(&self) -> bool {
        self.listing_failure.is_some()
            || self
                .items
                .iter()
                .any(|item| matches!(item.outcome, PruneOutcome::Failed { .. }))
    }
}

/// Paths `bu prune` never considers: the main checkout, the configured
/// repository path, and every configured bench.
struct ExcludedWorktrees {
    main: Option<PathBuf>,
    repository: PathBuf,
    benches: Vec<PathBuf>,
}

impl ExcludedWorktrees {
    fn new(config: &Config, worktrees: &[GitWorktree]) -> Self {
        Self {
            main: worktrees.first().map(|worktree| canonical(&worktree.path)),
            repository: canonical(&config.repository.path),
            benches: config
                .benches
                .iter()
                .map(|bench| canonical(&bench.path))
                .collect(),
        }
    }

    fn contains(&self, path: &Path) -> bool {
        let path = canonical(path);
        self.main.as_deref() == Some(path.as_path())
            || self.repository == path
            || self.benches.contains(&path)
    }

    fn is_bench(&self, path: &Path) -> bool {
        self.benches.contains(&canonical(path))
    }
}

/// Resolves symlinks in the longest existing prefix so a missing folder still
/// compares equal to the path Git recorded for it.
fn canonical(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(resolved) = fs::canonicalize(ancestor) {
            let suffix = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return if suffix.as_os_str().is_empty() {
                resolved
            } else {
                resolved.join(suffix)
            };
        }
    }
    path.to_path_buf()
}

pub fn prune(
    config: &Config,
    git: &GitAdapter,
    github: &GitHubAdapter,
    dry_run: bool,
) -> PruneReport {
    let repository = &config.repository.path;
    let worktrees = match git.worktrees(repository) {
        Ok(worktrees) => worktrees,
        Err(error) => {
            return PruneReport {
                dry_run,
                listing_failure: Some(format!("could not list worktrees: {error}")),
                items: Vec::new(),
            };
        }
    };
    let excluded = ExcludedWorktrees::new(config, &worktrees);
    let protected = ProtectedBranches::from_config(config);
    let current_directory = std::env::current_dir().ok().map(|path| canonical(&path));

    let mut items = Vec::new();
    let mut stale = Vec::new();
    let mut prunable = Vec::new();
    for worktree in worktrees
        .iter()
        .skip(1)
        .filter(|worktree| !worktree.bare && !excluded.contains(&worktree.path))
    {
        let branch = worktree
            .branch
            .clone()
            .unwrap_or_else(|| "detached".to_owned());
        let outcome = if worktree.locked {
            PruneOutcome::Skipped(PruneSkipReason::Locked)
        } else if !worktree.path.exists() {
            stale.push(items.len());
            PruneOutcome::Stale
        } else if current_directory
            .as_deref()
            .is_some_and(|directory| directory.starts_with(canonical(&worktree.path)))
        {
            PruneOutcome::Skipped(PruneSkipReason::ContainsCurrentDirectory)
        } else {
            match observe_scratch(git, github, repository, &worktree.path, &protected) {
                Err(failure) => failure,
                Ok(observation) => match decide_scratch(&observation, &protected) {
                    ScratchDecision::Prunable { pull_request, .. } => {
                        prunable.push(items.len());
                        PruneOutcome::Prunable { pull_request }
                    }
                    ScratchDecision::Skip(reason) => PruneOutcome::Skipped(reason),
                },
            }
        };
        items.push(PruneItem {
            path: worktree.path.clone(),
            branch,
            outcome,
        });
    }

    if !stale.is_empty() {
        let missing_protected = worktrees
            .iter()
            .find(|worktree| {
                !worktree.bare
                    && !worktree.locked
                    && excluded.contains(&worktree.path)
                    && !worktree.path.exists()
            })
            .map(|worktree| worktree.path.clone());
        if let Some(missing) = missing_protected {
            let is_bench = excluded.is_bench(&missing);
            for &index in &stale {
                items[index].outcome = PruneOutcome::StaleKept {
                    missing: missing.clone(),
                    is_bench,
                };
            }
        } else if !dry_run {
            clean_stale_metadata(git, repository, &mut items, &stale);
        }
    }

    if !dry_run {
        for &index in &prunable {
            let item = &mut items[index];
            item.outcome = remove_scratch(
                git,
                github,
                repository,
                &item.path,
                &protected,
                &mut item.branch,
            );
        }
    }

    PruneReport {
        dry_run,
        listing_failure: None,
        items,
    }
}

fn clean_stale_metadata(
    git: &GitAdapter,
    repository: &Path,
    items: &mut [PruneItem],
    stale: &[usize],
) {
    let result = git
        .prune_worktree_metadata(repository)
        .map_err(|error| ("stale metadata prune failed", error.to_string()))
        .and_then(|()| {
            git.worktrees(repository).map_err(|error| {
                (
                    "stale metadata verification failed",
                    format!("could not list worktrees after `git worktree prune`: {error}"),
                )
            })
        });
    for &index in stale {
        items[index].outcome = match &result {
            Err((summary, error)) => PruneOutcome::Failed {
                summary,
                error: error.clone(),
            },
            Ok(remaining)
                if remaining
                    .iter()
                    .any(|worktree| worktree.path == items[index].path) =>
            {
                PruneOutcome::Failed {
                    summary: "stale metadata remained",
                    error: "`git worktree prune` left this missing worktree registered".to_owned(),
                }
            }
            Ok(_) => PruneOutcome::Cleaned,
        };
    }
}

/// Repeats every check immediately before mutation, then removes the worktree
/// and compare-and-deletes its branch.
fn remove_scratch(
    git: &GitAdapter,
    github: &GitHubAdapter,
    repository: &Path,
    path: &Path,
    protected: &ProtectedBranches,
    displayed_branch: &mut String,
) -> PruneOutcome {
    match git.worktrees(repository) {
        Ok(worktrees) => match worktrees.iter().find(|worktree| worktree.path == path) {
            Some(worktree) if worktree.locked => {
                return PruneOutcome::Skipped(PruneSkipReason::Locked);
            }
            Some(_) if path.exists() => {}
            _ => {
                return PruneOutcome::Failed {
                    summary: "pre-removal recheck failed",
                    error: "the worktree folder or registration disappeared before removal"
                        .to_owned(),
                };
            }
        },
        Err(error) => {
            return PruneOutcome::Failed {
                summary: "pre-removal recheck failed",
                error: format!("could not list worktrees before removal: {error}"),
            };
        }
    }
    let observation = match observe_scratch(git, github, repository, path, protected) {
        Ok(observation) => observation,
        Err(PruneOutcome::Failed { summary, error }) => {
            return PruneOutcome::Failed {
                summary: "pre-removal recheck failed",
                error: format!("{summary}: {error}"),
            };
        }
        Err(outcome) => return outcome,
    };
    *displayed_branch = branch_name(&observation.branch).to_owned();
    let (branch, commit, pull_request) = match decide_scratch(&observation, protected) {
        ScratchDecision::Prunable {
            branch,
            commit,
            pull_request,
        } => (branch, commit, pull_request),
        ScratchDecision::Skip(reason) => return PruneOutcome::Skipped(reason),
    };

    if let Err(error) = git.remove_worktree(repository, path) {
        return PruneOutcome::Failed {
            summary: "worktree removal failed",
            error: error.to_string(),
        };
    }
    match git.delete_branch_if_unchanged(repository, &branch, &commit) {
        Ok(()) => PruneOutcome::Pruned { pull_request },
        Err(delete_error) => {
            match git.optional_ref_commit(repository, &format!("refs/heads/{branch}")) {
                Ok(current) if current.as_deref() != Some(commit.as_str()) => {
                    PruneOutcome::BranchKept {
                        pull_request,
                        current_commit: current,
                    }
                }
                Ok(_) => PruneOutcome::Failed {
                    summary: "branch deletion failed",
                    error: format!(
                        "worktree was removed, but branch {branch} remains at {commit}: {delete_error}"
                    ),
                },
                Err(lookup_error) => PruneOutcome::Failed {
                    summary: "branch deletion failed",
                    error: format!(
                        "worktree was removed, but deleting branch {branch} failed ({delete_error}) and its current state is unknown: {lookup_error}"
                    ),
                },
            }
        }
    }
}

fn observe_scratch(
    git: &GitAdapter,
    github: &GitHubAdapter,
    repository: &Path,
    path: &Path,
    protected: &ProtectedBranches,
) -> Result<ScratchObservation, PruneOutcome> {
    let failed = |summary: &'static str| {
        move |error: crate::error::AdapterError| PruneOutcome::Failed {
            summary,
            error: error.to_string(),
        }
    };
    let toplevel = git
        .toplevel(path)
        .map_err(failed("worktree root check failed"))?;
    if toplevel != canonical(path) {
        return Err(PruneOutcome::Failed {
            summary: "worktree root check failed",
            error: format!(
                "Git resolves this folder to {}, not to the registered worktree",
                toplevel.display()
            ),
        });
    }
    let branch = git
        .current_branch(path)
        .map_err(failed("branch lookup failed"))?;
    let worktree = git
        .worktree_state(path)
        .map_err(failed("worktree status lookup failed"))?;
    let operation = git
        .operation_state(path)
        .map_err(failed("Git operation lookup failed"))?;
    let pull_requests = match &branch {
        CurrentBranch::Attached { name, .. }
            if scratch_needs_pull_requests(&worktree, &branch, &operation, protected) =>
        {
            github
                .pull_requests(repository, name)
                .map_err(failed("pull-request lookup failed"))?
        }
        _ => PullRequestState::NotChecked,
    };
    Ok(ScratchObservation {
        worktree,
        branch,
        operation,
        pull_requests,
    })
}

#[derive(Clone, Copy)]
enum Role {
    Prunable,
    Stale,
    Pruned,
    Cleaned,
    Kept,
    Blocked,
    Skipped,
    Failed,
}

impl Role {
    fn style(self) -> RowStyle {
        let (label, ansi) = match self {
            Self::Prunable => ("prunable", CYAN),
            Self::Stale => ("stale", CYAN),
            Self::Pruned => ("pruned", GREEN),
            Self::Cleaned => ("cleaned", GREEN),
            Self::Kept => ("kept", YELLOW),
            Self::Blocked => ("blocked", YELLOW),
            Self::Skipped => ("skipped", DIM_GRAY),
            Self::Failed => ("failed", RED),
        };
        RowStyle { label, ansi }
    }
}

pub fn format_prune_report(report: &PruneReport, format: PruneFormat) -> String {
    let mut formatted = String::new();
    let paths: Vec<_> = report
        .items
        .iter()
        .map(|item| item.path.display().to_string())
        .collect();
    let labels = unique_labels(&paths);
    let mut counts = Counts::default();

    if let Some(error) = &report.listing_failure {
        counts.failed += 1;
        write_marker_row(
            &mut formatted,
            Role::Failed.style(),
            "repository",
            "unknown",
            "worktree listing failed",
            format.use_color,
        );
        let _ = writeln!(formatted, "    error: {error}");
    }

    for (item, label) in report.items.iter().zip(&labels) {
        let (role, reason) = describe(&item.outcome);
        counts.record(role);
        write_marker_row(
            &mut formatted,
            role.style(),
            label,
            &item.branch,
            &reason,
            format.use_color,
        );
        if format.verbose {
            let _ = writeln!(formatted, "    path: {}", item.path.display());
            if let PruneOutcome::Skipped(PruneSkipReason::Dirty(files)) = &item.outcome {
                write_dirty_files(&mut formatted, files);
            }
        }
        if let PruneOutcome::Failed { error, .. } = &item.outcome {
            let _ = writeln!(formatted, "    error: {error}");
        }
    }

    let worktree_word = if report.items.len() == 1 {
        "worktree"
    } else {
        "worktrees"
    };
    let _ = writeln!(formatted);
    let _ = writeln!(
        formatted,
        "Checked {} scratch {worktree_word}",
        report.items.len()
    );
    let Counts {
        prunable,
        stale,
        pruned,
        cleaned,
        kept,
        blocked,
        skipped,
        failed,
    } = counts;
    if report.dry_run {
        let _ = writeln!(
            formatted,
            "{prunable} prunable, {stale} stale, {blocked} blocked, {skipped} skipped, {failed} failed"
        );
        let _ = writeln!(formatted, "Dry run: no changes were made");
    } else {
        let _ = writeln!(
            formatted,
            "{pruned} pruned, {cleaned} cleaned, {kept} kept, {blocked} blocked, {skipped} skipped, {failed} failed"
        );
    }
    formatted
}

#[derive(Default)]
struct Counts {
    prunable: usize,
    stale: usize,
    pruned: usize,
    cleaned: usize,
    kept: usize,
    blocked: usize,
    skipped: usize,
    failed: usize,
}

impl Counts {
    fn record(&mut self, role: Role) {
        let count = match role {
            Role::Prunable => &mut self.prunable,
            Role::Stale => &mut self.stale,
            Role::Pruned => &mut self.pruned,
            Role::Cleaned => &mut self.cleaned,
            Role::Kept => &mut self.kept,
            Role::Blocked => &mut self.blocked,
            Role::Skipped => &mut self.skipped,
            Role::Failed => &mut self.failed,
        };
        *count += 1;
    }
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn describe(outcome: &PruneOutcome) -> (Role, String) {
    match outcome {
        PruneOutcome::Prunable { pull_request } => (
            Role::Prunable,
            format!("merged pull request #{pull_request}; would remove worktree and branch"),
        ),
        PruneOutcome::Pruned { pull_request } => (
            Role::Pruned,
            format!("merged pull request #{pull_request}; removed worktree and branch"),
        ),
        PruneOutcome::BranchKept {
            pull_request,
            current_commit: Some(current),
        } => (
            Role::Kept,
            format!(
                "merged pull request #{pull_request}; removed worktree; kept branch because it moved to {}",
                short(current)
            ),
        ),
        PruneOutcome::BranchKept {
            pull_request,
            current_commit: None,
        } => (
            Role::Kept,
            format!(
                "merged pull request #{pull_request}; removed worktree; branch was already deleted"
            ),
        ),
        PruneOutcome::Stale => (
            Role::Stale,
            "folder missing; would prune stale worktree metadata".to_owned(),
        ),
        PruneOutcome::Cleaned => (
            Role::Cleaned,
            "folder missing; pruned stale worktree metadata".to_owned(),
        ),
        PruneOutcome::StaleKept { missing, is_bench } => (
            Role::Blocked,
            format!(
                "folder missing; stale metadata kept because {} {} is also missing",
                if *is_bench {
                    "configured bench"
                } else {
                    "main checkout"
                },
                missing.display()
            ),
        ),
        PruneOutcome::Skipped(reason) => describe_skip(reason),
        PruneOutcome::Failed { summary, .. } => (Role::Failed, (*summary).to_owned()),
    }
}

fn describe_skip(reason: &PruneSkipReason) -> (Role, String) {
    match reason {
        PruneSkipReason::Locked => (Role::Blocked, "worktree is locked".to_owned()),
        PruneSkipReason::ContainsCurrentDirectory => (
            Role::Blocked,
            "current directory is inside this worktree".to_owned(),
        ),
        PruneSkipReason::OperationInProgress(operations) => {
            (Role::Blocked, format_operations(operations))
        }
        PruneSkipReason::DetachedHead => (Role::Blocked, "HEAD is detached".to_owned()),
        PruneSkipReason::ProtectedBranch {
            branch,
            protection: BranchProtection::MainBranch,
        } => (
            Role::Skipped,
            format!("{branch} is the configured main branch"),
        ),
        PruneSkipReason::ProtectedBranch {
            branch,
            protection: BranchProtection::StandinBranch,
        } => (
            Role::Skipped,
            format!("{branch} is a bench stand-in branch"),
        ),
        PruneSkipReason::Dirty(files) => {
            let file_word = if files.len() == 1 { "file" } else { "files" };
            (
                Role::Blocked,
                format!("dirty worktree ({} {file_word})", files.len()),
            )
        }
        PruneSkipReason::PullRequestNotChecked => {
            (Role::Skipped, "pull request was not queried".to_owned())
        }
        PruneSkipReason::NoPullRequest => (Role::Skipped, "no pull request found".to_owned()),
        PruneSkipReason::OpenPullRequest(number) => {
            (Role::Skipped, format!("pull request #{number} is open"))
        }
        PruneSkipReason::ClosedPullRequest(number) => (
            Role::Skipped,
            format!("pull request #{number} was closed without merging"),
        ),
        PruneSkipReason::AmbiguousPullRequests(count) => (
            Role::Blocked,
            format!("{count} merged pull requests; expected exactly one"),
        ),
        PruneSkipReason::HeadMismatch {
            pull_request,
            pull_request_head,
            local_head,
        } => (
            Role::Blocked,
            format!(
                "merged pull request #{pull_request} has head {}, not local HEAD {}",
                short(pull_request_head),
                short(local_head)
            ),
        ),
    }
}
