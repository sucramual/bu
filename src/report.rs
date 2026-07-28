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
        error: String,
    },
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
            error: format!("could not advance stand-in branch: {error}"),
        };
    }
    if let Err(error) = git.switch_branch(&bench.path, &bench.standin_branch) {
        return RunItem::Failed {
            bench: bench.path.display().to_string(),
            branch: None,
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
            message: error.to_string(),
        })?
    {
        return Err(ObservationFailure {
            branch: None,
            message: "bench is not a worktree of the configured repository".to_owned(),
        });
    }
    let branch = git
        .current_branch(&bench.path)
        .map_err(|error| ObservationFailure {
            branch: None,
            message: error.to_string(),
        })?;
    let worktree = match git.worktree_state(&bench.path) {
        Ok(state) => state,
        Err(error) => return Err(observation_failure(branch, error)),
    };
    let operation = match git.operation_state(&bench.path) {
        Ok(state) => state,
        Err(error) => return Err(observation_failure(branch, error)),
    };
    let standin = match git.standin_state(
        &config.repository.path,
        &bench.path,
        &config.repository.remote,
        &config.repository.main_branch,
        &bench.standin_branch,
    ) {
        Ok(state) => state,
        Err(error) => return Err(observation_failure(branch, error)),
    };

    let pull_requests = match (&worktree, &operation, &branch, &standin) {
        (
            WorktreeState::Clean,
            OperationState::Normal,
            CurrentBranch::Attached { name, .. },
            StandinState::Ready { .. },
        ) => match github.pull_requests(&config.repository.path, name) {
            Ok(state) => state,
            Err(error) => return Err(observation_failure(branch, error)),
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

fn observation_failure(branch: CurrentBranch, error: impl std::fmt::Display) -> ObservationFailure {
    ObservationFailure {
        branch: Some(branch),
        message: error.to_string(),
    }
}

pub fn format_report(report: &RunReport) -> String {
    let mut formatted = String::new();
    let mut eligible = 0;
    let mut recycled = 0;
    let mut skipped = 0;
    let mut failed = 0;

    for item in &report.items {
        match item {
            RunItem::Observed {
                observation,
                decision,
            } => {
                let (status, reason) = match decision {
                    BenchDecision::Eligible { pull_request, .. } => {
                        eligible += 1;
                        ("eligible", format!("merged pull request #{pull_request}"))
                    }
                    BenchDecision::Skip(reason) => {
                        skipped += 1;
                        ("skipped", format_skip_reason(reason))
                    }
                };
                write_observed_block(&mut formatted, observation, status, &reason);
            }
            RunItem::Recycled {
                bench,
                previous_branch,
                standin_branch,
                upstream_commit,
            } => {
                recycled += 1;
                write_block(
                    &mut formatted,
                    bench,
                    Some(previous_branch),
                    "recycled",
                    &format!("{previous_branch} preserved; {standin_branch} -> {upstream_commit}"),
                );
            }
            RunItem::Failed {
                bench,
                branch,
                error,
            } => {
                failed += 1;
                write_block(
                    &mut formatted,
                    bench,
                    branch.as_ref().map(branch_name),
                    "failed",
                    error,
                );
            }
        }
    }

    let _ = writeln!(
        formatted,
        "summary: {eligible} eligible, {recycled} recycled, {skipped} skipped, {failed} failed"
    );
    formatted
}

fn write_observed_block(
    formatted: &mut String,
    observation: &BenchObservation,
    status: &str,
    reason: &str,
) {
    write_block(
        formatted,
        &observation.bench.path.display().to_string(),
        Some(branch_name(&observation.branch)),
        status,
        reason,
    );
    if let WorktreeState::Dirty { files } = &observation.worktree {
        let _ = writeln!(formatted, "  dirty:");
        for file in files {
            let path = file.path.display();
            match &file.original_path {
                Some(original_path) => {
                    let _ = writeln!(
                        formatted,
                        "    {}{} {} -> {path}",
                        file.index_status,
                        file.worktree_status,
                        original_path.display(),
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

fn write_block(
    formatted: &mut String,
    bench: &str,
    branch: Option<&str>,
    status: &str,
    reason: &str,
) {
    let _ = writeln!(formatted, "{bench}");
    let _ = writeln!(formatted, "  branch: {}", branch.unwrap_or("unknown"));
    let _ = writeln!(formatted, "  status: {status}");
    let _ = writeln!(formatted, "  reason: {reason}");
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
