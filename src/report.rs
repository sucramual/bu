use std::fmt::Write;

use crate::adapters::{GitAdapter, GitHubAdapter};
use crate::domain::{
    BenchConfig, BenchDecision, BenchObservation, Config, CurrentBranch, DirtyFile, OperationState,
    PullRequestState, SkipReason, StandinState, WorktreeState, decide,
};

#[derive(Debug)]
pub struct RunReport {
    pub items: Vec<RunItem>,
}

#[derive(Debug)]
pub enum RunItem {
    Observed {
        observation: BenchObservation,
        decision: BenchDecision,
    },
    Recycled {
        bench: String,
        previous_branch: String,
        standin_branch: String,
        upstream_commit: String,
        discarded_files: Vec<DirtyFile>,
    },
    Failed {
        bench: String,
        branch: Option<CurrentBranch>,
        summary: &'static str,
        error: String,
    },
    CleanupFailed {
        bench: String,
        branch: String,
        phase: &'static str,
        error: String,
        remaining_files: Option<Vec<DirtyFile>>,
        inspection_error: Option<String>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct StatusFormat {
    pub verbose: bool,
    pub use_color: bool,
}

impl RunReport {
    pub fn has_failures(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item, RunItem::Failed { .. } | RunItem::CleanupFailed { .. }))
    }
}

pub fn status(config: &Config, git: &GitAdapter, github: &GitHubAdapter) -> RunReport {
    RunReport {
        items: config
            .benches
            .iter()
            .map(|bench| observed_item(config, bench, git, github))
            .collect(),
    }
}

pub fn recycle(
    config: &Config,
    git: &GitAdapter,
    github: &GitHubAdapter,
    force: bool,
) -> RunReport {
    if let Err(error) = git.fetch_main(
        &config.repository.path,
        &config.repository.remote,
        &config.repository.main_branch,
    ) {
        return RunReport {
            items: config
                .benches
                .iter()
                .map(|bench| RunItem::Failed {
                    bench: bench.path.display().to_string(),
                    branch: None,
                    summary: "upstream fetch failed",
                    error: format!("could not fetch upstream main before recycling: {error}"),
                })
                .collect(),
        };
    }

    RunReport {
        items: config
            .benches
            .iter()
            .map(|bench| recycle_item(config, bench, git, github, force))
            .collect(),
    }
}

fn observed_item(
    config: &Config,
    bench: &BenchConfig,
    git: &GitAdapter,
    github: &GitHubAdapter,
) -> RunItem {
    match observe(config, bench, git, github) {
        Ok(observation) => {
            let decision = decide(&observation);
            RunItem::Observed {
                observation,
                decision,
            }
        }
        Err(failure) => RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: failure.branch,
            summary: failure.summary,
            error: failure.message,
        },
    }
}

fn recycle_item(
    config: &Config,
    bench: &BenchConfig,
    git: &GitAdapter,
    github: &GitHubAdapter,
    force: bool,
) -> RunItem {
    let observation = match observe(config, bench, git, github) {
        Ok(observation) => observation,
        Err(failure) => {
            return RunItem::Failed {
                bench: bench.path.display().to_string(),
                branch: failure.branch,
                summary: failure.summary,
                error: failure.message,
            };
        }
    };
    let decision = decide(&observation);
    let force_cleanup = matches!(&decision, BenchDecision::Forceable { .. });
    if !(matches!(&decision, BenchDecision::Eligible { .. }) || force && force_cleanup) {
        return RunItem::Observed {
            observation,
            decision,
        };
    }

    let recheck = match observe(config, bench, git, github) {
        Ok(observation) => observation,
        Err(failure) => {
            return RunItem::Failed {
                bench: bench.path.display().to_string(),
                branch: failure.branch,
                summary: "pre-mutation recheck failed",
                error: format!(
                    "could not recheck bench before mutation: {}",
                    failure.message
                ),
            };
        }
    };
    let rechecked_decision = decide(&recheck);
    let (branch, discarded_files) = match (force_cleanup, rechecked_decision) {
        (false, BenchDecision::Eligible { branch, .. }) => (branch, Vec::new()),
        (true, BenchDecision::Forceable { branch, files, .. }) => (branch, files),
        (_, decision) => {
            return RunItem::Observed {
                observation: recheck,
                decision,
            };
        }
    };
    let expected_head = match &recheck.branch {
        CurrentBranch::Attached { name, commit } if name == &branch => commit.clone(),
        _ => {
            return RunItem::Failed {
                bench: bench.path.display().to_string(),
                branch: None,
                summary: "pre-mutation recheck was inconsistent",
                error: "forceable decision did not retain its attached branch and HEAD".to_owned(),
            };
        }
    };

    let StandinState::Ready {
        standin_commit,
        upstream_commit,
    } = &recheck.standin
    else {
        return RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: None,
            summary: "stand-in state was invalid",
            error: "eligible bench was missing a ready stand-in state".to_owned(),
        };
    };
    let standin_commit = standin_commit.clone();
    let upstream_commit = upstream_commit.clone();

    let feature_ref = format!("refs/heads/{branch}");
    let feature_commit = expected_head;

    let stash_before = if force_cleanup {
        match git.optional_ref_commit(&config.repository.path, "refs/stash") {
            Ok(commit) => Some(commit),
            Err(error) => {
                return RunItem::Failed {
                    bench: bench.path.display().to_string(),
                    branch: None,
                    summary: "stash lookup failed",
                    error: format!("could not record stash state before cleanup: {error}"),
                };
            }
        }
    } else {
        None
    };

    if force_cleanup {
        match git.current_branch(&bench.path) {
            Ok(CurrentBranch::Attached { name, commit })
                if name == branch && commit == feature_commit => {}
            Ok(_) => return observed_item(config, bench, git, github),
            Err(error) => {
                return RunItem::Failed {
                    bench: bench.path.display().to_string(),
                    branch: None,
                    summary: "pre-cleanup HEAD check failed",
                    error: format!("could not verify HEAD immediately before cleanup: {error}"),
                };
            }
        }
        match git.operation_state(&bench.path) {
            Ok(OperationState::Normal) => {}
            Ok(OperationState::InProgress(_)) => {
                return observed_item(config, bench, git, github);
            }
            Err(error) => {
                return RunItem::Failed {
                    bench: bench.path.display().to_string(),
                    branch: None,
                    summary: "pre-cleanup Git operation check failed",
                    error: format!(
                        "could not verify Git operation state immediately before cleanup: {error}"
                    ),
                };
            }
        }
        match git.standin_state(
            &config.repository.path,
            &bench.path,
            &config.repository.remote,
            &config.repository.main_branch,
            &bench.standin_branch,
        ) {
            Ok(StandinState::Ready {
                standin_commit: current_standin,
                upstream_commit: current_upstream,
            }) if current_standin == standin_commit && current_upstream == upstream_commit => {}
            Ok(_) => return observed_item(config, bench, git, github),
            Err(error) => {
                return RunItem::Failed {
                    bench: bench.path.display().to_string(),
                    branch: None,
                    summary: "pre-cleanup stand-in check failed",
                    error: format!(
                        "could not verify stand-in state immediately before cleanup: {error}"
                    ),
                };
            }
        }
        if let Err(error) = git.reset_hard(&bench.path) {
            return cleanup_failure(bench, &branch, "tracked reset", error, git);
        }
        if let Err(error) = git.clean_untracked(&bench.path) {
            return cleanup_failure(bench, &branch, "untracked cleanup", error, git);
        }
        if let Err(error) = verify_cleanup(
            config,
            bench,
            git,
            &branch,
            &feature_commit,
            stash_before
                .as_ref()
                .expect("force cleanup recorded stash state"),
        ) {
            return cleanup_failure(bench, &branch, "cleanup verification", error, git);
        }
    }

    if let Err(error) = git.advance_standin(
        &config.repository.path,
        &bench.standin_branch,
        &standin_commit,
        &upstream_commit,
    ) {
        return RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: None,
            summary: "stand-in update failed",
            error: format!("could not advance stand-in branch: {error}"),
        };
    }
    if let Err(error) = git.switch_branch(&bench.path, &bench.standin_branch) {
        return RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: None,
            summary: "worktree switch failed",
            error: format!(
                "stand-in branch advanced, but switching the worktree failed; feature branch remains {feature_ref} at {feature_commit}: {error}"
            ),
        };
    }

    match verify_recycle(
        config,
        bench,
        git,
        &feature_ref,
        &feature_commit,
        &upstream_commit,
    ) {
        Ok(()) => RunItem::Recycled {
            bench: bench.path.display().to_string(),
            previous_branch: branch,
            standin_branch: bench.standin_branch.clone(),
            upstream_commit,
            discarded_files,
        },
        Err(error) => RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: None,
            summary: "post-recycle verification failed",
            error: format!("recycle completed with a failed postcondition: {error}"),
        },
    }
}

fn verify_cleanup(
    config: &Config,
    bench: &BenchConfig,
    git: &GitAdapter,
    branch: &str,
    feature_commit: &str,
    stash_before: &Option<String>,
) -> Result<(), String> {
    match git
        .current_branch(&bench.path)
        .map_err(|error| error.to_string())?
    {
        CurrentBranch::Attached { name, commit } if name == branch && commit == feature_commit => {}
        CurrentBranch::Attached { name, commit } => {
            return Err(format!("HEAD changed to {name} at {commit}"));
        }
        CurrentBranch::Detached => return Err("HEAD became detached".to_owned()),
    }
    if !matches!(
        git.operation_state(&bench.path)
            .map_err(|error| error.to_string())?,
        OperationState::Normal
    ) {
        return Err("a Git operation started during cleanup".to_owned());
    }
    if !matches!(
        git.worktree_state(&bench.path)
            .map_err(|error| error.to_string())?,
        WorktreeState::Clean
    ) {
        return Err("worktree is still dirty after cleanup".to_owned());
    }
    let stash_after = git
        .optional_ref_commit(&config.repository.path, "refs/stash")
        .map_err(|error| error.to_string())?;
    if &stash_after != stash_before {
        return Err("stash state changed during cleanup".to_owned());
    }
    Ok(())
}

fn cleanup_failure(
    bench: &BenchConfig,
    branch: &str,
    phase: &'static str,
    error: impl std::fmt::Display,
    git: &GitAdapter,
) -> RunItem {
    let (remaining_files, inspection_error) = match git.worktree_state(&bench.path) {
        Ok(WorktreeState::Clean) => (Some(Vec::new()), None),
        Ok(WorktreeState::Dirty { files }) => (Some(files), None),
        Err(inspect_error) => (None, Some(inspect_error.to_string())),
    };
    RunItem::CleanupFailed {
        bench: bench.path.display().to_string(),
        branch: branch.to_owned(),
        phase,
        error: error.to_string(),
        remaining_files,
        inspection_error,
    }
}

fn verify_recycle(
    config: &Config,
    bench: &BenchConfig,
    git: &GitAdapter,
    feature_ref: &str,
    feature_commit: &str,
    upstream_commit: &str,
) -> Result<(), String> {
    match git
        .current_branch(&bench.path)
        .map_err(|error| error.to_string())?
    {
        CurrentBranch::Attached { name, .. } if name == bench.standin_branch => {}
        CurrentBranch::Attached { name, .. } => {
            return Err(format!(
                "worktree is on {name}, not {}",
                bench.standin_branch
            ));
        }
        CurrentBranch::Detached => return Err("worktree is detached".to_owned()),
    }
    if !matches!(
        git.worktree_state(&bench.path)
            .map_err(|error| error.to_string())?,
        WorktreeState::Clean
    ) {
        return Err("worktree is dirty".to_owned());
    }
    if git
        .ref_commit(&config.repository.path, feature_ref)
        .map_err(|error| error.to_string())?
        != feature_commit
    {
        return Err(format!("feature ref {feature_ref} changed"));
    }
    let standin_ref = format!("refs/heads/{}", bench.standin_branch);
    if git
        .ref_commit(&config.repository.path, &standin_ref)
        .map_err(|error| error.to_string())?
        != upstream_commit
    {
        return Err(format!(
            "stand-in ref {standin_ref} is not at fetched upstream"
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ObservationFailure {
    branch: Option<CurrentBranch>,
    summary: &'static str,
    message: String,
}

fn observe(
    config: &Config,
    bench: &BenchConfig,
    git: &GitAdapter,
    github: &GitHubAdapter,
) -> Result<BenchObservation, ObservationFailure> {
    if !git
        .same_repository(&config.repository.path, &bench.path)
        .map_err(|error| ObservationFailure {
            branch: None,
            summary: "repository check failed",
            message: error.to_string(),
        })?
    {
        return Err(ObservationFailure {
            branch: None,
            summary: "repository check failed",
            message: "bench is not a worktree of the configured repository".to_owned(),
        });
    }
    let branch = git
        .current_branch(&bench.path)
        .map_err(|error| ObservationFailure {
            branch: None,
            summary: "branch lookup failed",
            message: error.to_string(),
        })?;
    let worktree = match git.worktree_state(&bench.path) {
        Ok(state) => state,
        Err(error) => {
            return Err(observation_failure(
                branch,
                "worktree status lookup failed",
                error,
            ));
        }
    };
    let operation = match git.operation_state(&bench.path) {
        Ok(state) => state,
        Err(error) => {
            return Err(observation_failure(
                branch,
                "Git operation lookup failed",
                error,
            ));
        }
    };
    let standin = match git.standin_state(
        &config.repository.path,
        &bench.path,
        &config.repository.remote,
        &config.repository.main_branch,
        &bench.standin_branch,
    ) {
        Ok(state) => state,
        Err(error) => return Err(observation_failure(branch, "stand-in lookup failed", error)),
    };

    let pull_requests = match (&operation, &branch, &standin) {
        (
            OperationState::Normal,
            CurrentBranch::Attached { name, .. },
            StandinState::Ready { .. },
        ) if name != &bench.standin_branch => {
            match github.pull_requests(&config.repository.path, name) {
                Ok(state) => state,
                Err(error) => {
                    return Err(observation_failure(
                        branch,
                        "pull-request lookup failed",
                        error,
                    ));
                }
            }
        }
        _ => PullRequestState::NotChecked,
    };

    Ok(BenchObservation {
        bench: bench.clone(),
        worktree,
        branch,
        operation,
        standin,
        pull_requests,
    })
}

fn observation_failure(
    branch: CurrentBranch,
    summary: &'static str,
    error: impl std::fmt::Display,
) -> ObservationFailure {
    ObservationFailure {
        branch: Some(branch),
        summary,
        message: error.to_string(),
    }
}

pub fn format_report(report: &RunReport, use_color: bool) -> String {
    let mut formatted = String::new();
    let labels = status_labels(report);
    let mut recycled = 0;
    let mut blocked = 0;
    let mut skipped = 0;
    let mut failed = 0;

    for (index, item) in report.items.iter().enumerate() {
        match item {
            RunItem::Observed {
                observation,
                decision,
            } => {
                let (role, reason) =
                    observed_role_and_reason(observation, decision, StatusRole::Skipped);
                match role {
                    StatusRole::Blocked => blocked += 1,
                    StatusRole::Forceable | StatusRole::Skipped => skipped += 1,
                    StatusRole::Eligible
                    | StatusRole::Idle
                    | StatusRole::Recycled
                    | StatusRole::Failed => {}
                }
                write_status_row(
                    &mut formatted,
                    role,
                    &labels[index],
                    branch_name(&observation.branch),
                    &reason,
                    use_color,
                );
            }
            RunItem::Recycled {
                previous_branch,
                standin_branch,
                upstream_commit,
                discarded_files,
                ..
            } => {
                recycled += 1;
                write_status_row(
                    &mut formatted,
                    StatusRole::Recycled,
                    &labels[index],
                    standin_branch,
                    &format!(
                        "{previous_branch} merged; preserved; {standin_branch} -> {upstream_commit}"
                    ),
                    use_color,
                );
                for file in discarded_files {
                    let _ = writeln!(formatted, "    discarded: {}", dirty_file_path(file));
                }
            }
            RunItem::Failed {
                branch,
                summary,
                error,
                ..
            } => {
                failed += 1;
                write_status_row(
                    &mut formatted,
                    StatusRole::Failed,
                    &labels[index],
                    branch.as_ref().map(branch_name).unwrap_or("unknown"),
                    summary,
                    use_color,
                );
                let _ = writeln!(formatted, "    error: {error}");
            }
            RunItem::CleanupFailed {
                branch,
                phase,
                error,
                remaining_files,
                inspection_error,
                ..
            } => {
                failed += 1;
                write_status_row(
                    &mut formatted,
                    StatusRole::Failed,
                    &labels[index],
                    branch,
                    "cleanup failed; worktree may be partially cleaned",
                    use_color,
                );
                let _ = writeln!(formatted, "    failed phase: {phase}");
                let _ = writeln!(formatted, "    error: {error}");
                if let Some(files) = remaining_files {
                    if files.is_empty() {
                        let _ = writeln!(formatted, "    remaining dirty paths: none");
                    } else {
                        for file in files {
                            let _ = writeln!(formatted, "    remaining: {}", dirty_file_path(file));
                        }
                    }
                }
                if let Some(error) = inspection_error {
                    let _ = writeln!(formatted, "    remaining-state inspection failed: {error}");
                }
            }
        }
    }

    let bench_word = if report.items.len() == 1 {
        "bench"
    } else {
        "benches"
    };
    let _ = writeln!(formatted);
    let _ = writeln!(formatted, "Checked {} {bench_word}", report.items.len());
    let _ = writeln!(
        formatted,
        "{recycled} recycled, {blocked} blocked, {skipped} skipped, {failed} failed"
    );
    formatted
}

pub fn format_status_report(report: &RunReport, format: StatusFormat) -> String {
    let mut formatted = String::new();
    let labels = status_labels(report);
    let eligible = report
        .items
        .iter()
        .filter(|item| {
            matches!(
                item,
                RunItem::Observed {
                    decision: BenchDecision::Eligible { .. },
                    ..
                }
            )
        })
        .count();
    let forceable = report
        .items
        .iter()
        .filter(|item| {
            matches!(
                item,
                RunItem::Observed {
                    decision: BenchDecision::Forceable { .. },
                    ..
                }
            )
        })
        .count();

    for (index, item) in report.items.iter().enumerate() {
        match item {
            RunItem::Observed {
                observation,
                decision,
            } => {
                let (role, reason) =
                    observed_role_and_reason(observation, decision, StatusRole::Idle);
                write_status_row(
                    &mut formatted,
                    role,
                    &labels[index],
                    branch_name(&observation.branch),
                    &reason,
                    format.use_color,
                );
                if format.verbose {
                    write_status_details(&mut formatted, observation);
                }
            }
            RunItem::Failed {
                bench,
                branch,
                summary,
                error,
            } => {
                write_status_row(
                    &mut formatted,
                    StatusRole::Failed,
                    &labels[index],
                    branch.as_ref().map(branch_name).unwrap_or("unknown"),
                    summary,
                    format.use_color,
                );
                if format.verbose {
                    let _ = writeln!(formatted, "    path: {bench}");
                    let _ = writeln!(formatted, "    error: {error}");
                }
            }
            RunItem::CleanupFailed {
                bench,
                branch,
                phase,
                error,
                remaining_files,
                inspection_error,
            } => {
                write_status_row(
                    &mut formatted,
                    StatusRole::Failed,
                    &labels[index],
                    branch,
                    "cleanup failed; worktree may be partially cleaned",
                    format.use_color,
                );
                if format.verbose {
                    let _ = writeln!(formatted, "    path: {bench}");
                    let _ = writeln!(formatted, "    failed phase: {phase}");
                    let _ = writeln!(formatted, "    error: {error}");
                    if let Some(files) = remaining_files {
                        for file in files {
                            let _ = writeln!(formatted, "    remaining: {}", dirty_file_path(file));
                        }
                    }
                    if let Some(error) = inspection_error {
                        let _ =
                            writeln!(formatted, "    remaining-state inspection failed: {error}");
                    }
                }
            }
            RunItem::Recycled {
                bench,
                previous_branch,
                standin_branch,
                upstream_commit,
                discarded_files: _,
            } => {
                write_status_row(
                    &mut formatted,
                    StatusRole::Idle,
                    &labels[index],
                    standin_branch,
                    &format!(
                        "{previous_branch} merged; preserved; {standin_branch} -> {upstream_commit}"
                    ),
                    format.use_color,
                );
                if format.verbose {
                    let _ = writeln!(formatted, "    path: {bench}");
                }
            }
        }
    }

    let bench_word = if report.items.len() == 1 {
        "bench"
    } else {
        "benches"
    };
    let eligible_word = if eligible == 1 { "bench" } else { "benches" };
    let forceable_word = if forceable == 1 { "bench" } else { "benches" };
    let _ = writeln!(formatted);
    let _ = writeln!(formatted, "Checked {} {bench_word}", report.items.len());
    let _ = writeln!(
        formatted,
        "{eligible} {eligible_word} eligible for `bu recycle`"
    );
    let _ = writeln!(
        formatted,
        "{forceable} {forceable_word} forceable with `bu recycle --force`"
    );
    formatted
}

#[derive(Clone, Copy)]
enum StatusRole {
    Eligible,
    Forceable,
    Blocked,
    Idle,
    Recycled,
    Skipped,
    Failed,
}

impl StatusRole {
    fn label(self) -> &'static str {
        match self {
            Self::Eligible => "merged",
            Self::Forceable => "forceable",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::Recycled => "recycled",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    fn ansi(self) -> &'static str {
        match self {
            Self::Eligible => CYAN,
            Self::Forceable | Self::Blocked => YELLOW,
            Self::Idle | Self::Skipped => DIM_GRAY,
            Self::Recycled => GREEN,
            Self::Failed => RED,
        }
    }
}

fn status_labels(report: &RunReport) -> Vec<String> {
    let paths = report.items.iter().map(item_bench_path).collect::<Vec<_>>();
    unique_labels(&paths)
}

/// Labels each path by its basename unless another path shares that basename.
pub(crate) fn unique_labels(paths: &[String]) -> Vec<String> {
    let basenames = paths
        .iter()
        .map(|path| {
            std::path::Path::new(path)
                .file_name()
                .filter(|name| !name.is_empty())
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();

    basenames
        .iter()
        .enumerate()
        .map(|(index, basename)| match basename {
            Some(basename)
                if basenames
                    .iter()
                    .filter(|other| other.as_ref() == Some(basename))
                    .count()
                    == 1 =>
            {
                basename.clone()
            }
            _ => paths[index].clone(),
        })
        .collect()
}

fn item_bench_path(item: &RunItem) -> String {
    match item {
        RunItem::Observed { observation, .. } => observation.bench.path.display().to_string(),
        RunItem::Recycled { bench, .. }
        | RunItem::Failed { bench, .. }
        | RunItem::CleanupFailed { bench, .. } => bench.clone(),
    }
}

fn observed_role_and_reason(
    observation: &BenchObservation,
    decision: &BenchDecision,
    settled_role: StatusRole,
) -> (StatusRole, String) {
    match decision {
        BenchDecision::Eligible { pull_request, .. } => (
            StatusRole::Eligible,
            format!("merged pull request #{pull_request}"),
        ),
        BenchDecision::Forceable {
            pull_request,
            files,
            ..
        } => {
            let file_word = if files.len() == 1 { "file" } else { "files" };
            let reason = if matches!(settled_role, StatusRole::Skipped) {
                format!(
                    "merged pull request #{pull_request}; run `bu recycle --force` to discard {} dirty {file_word}",
                    files.len()
                )
            } else {
                format!(
                    "merged pull request #{pull_request}; dirty worktree ({} {file_word})",
                    files.len()
                )
            };
            (StatusRole::Forceable, reason)
        }
        BenchDecision::Skip(reason) => {
            let role = match reason {
                SkipReason::AlreadyOnStandin => settled_role,
                SkipReason::PullRequestDoesNotMatch
                    if matches!(observation.worktree, WorktreeState::Clean) =>
                {
                    settled_role
                }
                _ => StatusRole::Blocked,
            };
            let description = format_skip_reason(reason);
            (role, description)
        }
    }
}

fn write_status_row(
    formatted: &mut String,
    role: StatusRole,
    bench: &str,
    branch: &str,
    reason: &str,
    use_color: bool,
) {
    write_marker_row(
        formatted,
        RowStyle {
            label: role.label(),
            ansi: role.ansi(),
        },
        bench,
        branch,
        reason,
        use_color,
    );
}

/// The leading label of a report row and the ANSI style of its marker.
#[derive(Clone, Copy)]
pub(crate) struct RowStyle {
    pub label: &'static str,
    pub ansi: &'static str,
}

pub(crate) const CYAN: &str = "\x1b[36m";
pub(crate) const YELLOW: &str = "\x1b[33m";
pub(crate) const DIM_GRAY: &str = "\x1b[2;90m";
pub(crate) const GREEN: &str = "\x1b[32m";
pub(crate) const RED: &str = "\x1b[31m";

pub(crate) fn write_marker_row(
    formatted: &mut String,
    style: RowStyle,
    name: &str,
    branch: &str,
    reason: &str,
    use_color: bool,
) {
    let marker = if use_color {
        format!("{}▎\x1b[0m", style.ansi)
    } else {
        "▎".to_owned()
    };
    let _ = writeln!(
        formatted,
        "{marker} {:<8} {name} {branch} {reason}",
        style.label
    );
}

fn write_status_details(formatted: &mut String, observation: &BenchObservation) {
    let _ = writeln!(formatted, "    path: {}", observation.bench.path.display());
    if let WorktreeState::Dirty { files } = &observation.worktree {
        write_dirty_files(formatted, files);
    }
}

pub(crate) fn write_dirty_files(formatted: &mut String, files: &[DirtyFile]) {
    for file in files {
        let path = format_path(&file.path);
        match &file.original_path {
            Some(original_path) => {
                let _ = writeln!(
                    formatted,
                    "    {}{} {} -> {path}",
                    file.index_status,
                    file.worktree_status,
                    format_path(original_path),
                );
            }
            None => {
                let _ = writeln!(
                    formatted,
                    "    {}{} {path}",
                    file.index_status, file.worktree_status,
                );
            }
        }
    }
}

fn format_path(path: &std::path::Path) -> String {
    path.to_string_lossy().escape_default().to_string()
}

fn dirty_file_path(file: &DirtyFile) -> String {
    match &file.original_path {
        Some(original_path) => format!(
            "{} -> {}",
            format_path(original_path),
            format_path(&file.path)
        ),
        None => format_path(&file.path),
    }
}

pub(crate) fn branch_name(branch: &CurrentBranch) -> &str {
    match branch {
        CurrentBranch::Attached { name, .. } => name,
        CurrentBranch::Detached => "detached",
    }
}

pub(crate) fn format_operations(operations: &[crate::domain::GitOperation]) -> String {
    format!(
        "Git operation in progress: {}",
        operations
            .iter()
            .map(|operation| match operation {
                crate::domain::GitOperation::Merge => "merge",
                crate::domain::GitOperation::Rebase => "rebase",
                crate::domain::GitOperation::CherryPick => "cherry-pick",
            })
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn format_skip_reason(reason: &SkipReason) -> String {
    match reason {
        SkipReason::DetachedHead => "HEAD is detached".to_owned(),
        SkipReason::AlreadyOnStandin => "already on the stand-in branch".to_owned(),
        SkipReason::OperationInProgress(operations) => format_operations(operations),
        SkipReason::StandinMissing => "stand-in branch is missing".to_owned(),
        SkipReason::StandinCheckedOutElsewhere(path) => {
            format!("stand-in branch is checked out at {}", path.display())
        }
        SkipReason::UpstreamNotFetched => "upstream main has not been fetched".to_owned(),
        SkipReason::StandinNotFastForwardable => {
            "stand-in branch cannot fast-forward to upstream main".to_owned()
        }
        SkipReason::PullRequestNotChecked => "pull request was not queried".to_owned(),
        SkipReason::PullRequestDoesNotMatch => {
            "current branch does not have exactly one merged pull request".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RunItem, RunReport, status_labels};

    #[test]
    fn duplicate_basenames_use_full_configured_paths() {
        let report = RunReport {
            items: vec![
                RunItem::Failed {
                    bench: "/benches/one/example-repo-01".to_owned(),
                    branch: None,
                    summary: "repository check failed",
                    error: "first failure".to_owned(),
                },
                RunItem::Failed {
                    bench: "/benches/two/example-repo-01".to_owned(),
                    branch: None,
                    summary: "repository check failed",
                    error: "second failure".to_owned(),
                },
            ],
        };

        assert_eq!(
            status_labels(&report),
            [
                "/benches/one/example-repo-01".to_owned(),
                "/benches/two/example-repo-01".to_owned(),
            ]
        );
    }
}
