use std::fmt::Write;

use crate::adapters::{GitAdapter, GitHubAdapter};
use crate::domain::{
    BenchConfig, BenchDecision, BenchObservation, Config, CurrentBranch, OperationState,
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
    },
    Failed {
        bench: String,
        branch: Option<CurrentBranch>,
        summary: &'static str,
        error: String,
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
            .any(|item| matches!(item, RunItem::Failed { .. }))
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

pub fn recycle(config: &Config, git: &GitAdapter, github: &GitHubAdapter) -> RunReport {
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
            .map(|bench| recycle_item(config, bench, git, github))
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
    let BenchDecision::Eligible { .. } = decision else {
        return RunItem::Observed {
            observation,
            decision,
        };
    };

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
    let BenchDecision::Eligible {
        branch,
        pull_request: _,
    } = rechecked_decision
    else {
        return RunItem::Observed {
            observation: recheck,
            decision: rechecked_decision,
        };
    };

    let StandinState::Ready {
        standin_commit,
        upstream_commit,
    } = recheck.standin
    else {
        return RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: None,
            summary: "stand-in state was invalid",
            error: "eligible bench was missing a ready stand-in state".to_owned(),
        };
    };

    let feature_ref = format!("refs/heads/{branch}");
    let feature_commit = match git.ref_commit(&config.repository.path, &feature_ref) {
        Ok(commit) => commit,
        Err(error) => {
            return RunItem::Failed {
                bench: bench.path.display().to_string(),
                branch: None,
                summary: "feature branch lookup failed",
                error: format!("could not record feature branch before mutation: {error}"),
            };
        }
    };

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
        },
        Err(error) => RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: None,
            summary: "post-recycle verification failed",
            error: format!("recycle completed with a failed postcondition: {error}"),
        },
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

    let pull_requests = match (&worktree, &operation, &branch, &standin) {
        (
            WorktreeState::Clean,
            OperationState::Normal,
            CurrentBranch::Attached { name, .. },
            StandinState::Ready { .. },
        ) => match github.pull_requests(&config.repository.path, name) {
            Ok(state) => state,
            Err(error) => {
                return Err(observation_failure(
                    branch,
                    "pull-request lookup failed",
                    error,
                ));
            }
        },
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
                    StatusRole::Skipped => skipped += 1,
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
                ..
            } => {
                recycled += 1;
                write_status_row(
                    &mut formatted,
                    StatusRole::Recycled,
                    &labels[index],
                    standin_branch,
                    &format!("{previous_branch} preserved; {standin_branch} -> {upstream_commit}"),
                    use_color,
                );
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
            RunItem::Recycled {
                bench,
                previous_branch,
                standin_branch,
                upstream_commit,
            } => {
                write_status_row(
                    &mut formatted,
                    StatusRole::Idle,
                    &labels[index],
                    standin_branch,
                    &format!("{previous_branch} preserved; {standin_branch} -> {upstream_commit}"),
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
    let _ = writeln!(formatted);
    let _ = writeln!(formatted, "Checked {} {bench_word}", report.items.len());
    let _ = writeln!(
        formatted,
        "{eligible} {eligible_word} eligible for `bu recycle`"
    );
    formatted
}

#[derive(Clone, Copy)]
enum StatusRole {
    Eligible,
    Blocked,
    Idle,
    Recycled,
    Skipped,
    Failed,
}

impl StatusRole {
    fn label(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::Recycled => "recycled",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    fn ansi(self) -> &'static str {
        match self {
            Self::Eligible => "\x1b[36m",
            Self::Blocked => "\x1b[33m",
            Self::Idle => "\x1b[2;90m",
            Self::Recycled => "\x1b[32m",
            Self::Skipped => "\x1b[2;90m",
            Self::Failed => "\x1b[31m",
        }
    }
}

fn status_labels(report: &RunReport) -> Vec<String> {
    let paths = report.items.iter().map(item_bench_path).collect::<Vec<_>>();
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
        RunItem::Recycled { bench, .. } | RunItem::Failed { bench, .. } => bench.clone(),
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
        BenchDecision::Skip(reason) => {
            let role = match reason {
                SkipReason::AlreadyOnStandin | SkipReason::PullRequestDoesNotMatch => settled_role,
                _ => StatusRole::Blocked,
            };
            let description = match (&observation.worktree, reason) {
                (WorktreeState::Dirty { files }, SkipReason::DirtyWorktree) => {
                    let file_word = if files.len() == 1 { "file" } else { "files" };
                    format!("dirty worktree ({} {file_word})", files.len())
                }
                _ => format_skip_reason(reason),
            };
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
    let marker = if use_color {
        format!("{}▎\x1b[0m", role.ansi())
    } else {
        "▎".to_owned()
    };
    let _ = writeln!(
        formatted,
        "{marker} {:<8} {bench} {branch} {reason}",
        role.label()
    );
}

fn write_status_details(formatted: &mut String, observation: &BenchObservation) {
    let _ = writeln!(formatted, "    path: {}", observation.bench.path.display());
    if let WorktreeState::Dirty { files } = &observation.worktree {
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
}

fn format_path(path: &std::path::Path) -> String {
    path.to_string_lossy().escape_default().to_string()
}

fn branch_name(branch: &CurrentBranch) -> &str {
    match branch {
        CurrentBranch::Attached { name, .. } => name,
        CurrentBranch::Detached => "detached",
    }
}

fn format_skip_reason(reason: &SkipReason) -> String {
    match reason {
        SkipReason::DirtyWorktree => "worktree is dirty".to_owned(),
        SkipReason::DetachedHead => "HEAD is detached".to_owned(),
        SkipReason::AlreadyOnStandin => "already on the stand-in branch".to_owned(),
        SkipReason::OperationInProgress(operations) => format!(
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
        ),
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
                    bench: "/benches/one/multiplier-01".to_owned(),
                    branch: None,
                    summary: "repository check failed",
                    error: "first failure".to_owned(),
                },
                RunItem::Failed {
                    bench: "/benches/two/multiplier-01".to_owned(),
                    branch: None,
                    summary: "repository check failed",
                    error: "second failure".to_owned(),
                },
            ],
        };

        assert_eq!(
            status_labels(&report),
            [
                "/benches/one/multiplier-01".to_owned(),
                "/benches/two/multiplier-01".to_owned(),
            ]
        );
    }
}
