use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub repository: RepositoryConfig,
    pub benches: Vec<BenchConfig>,
}

#[derive(Debug, Deserialize)]
pub struct RepositoryConfig {
    pub path: PathBuf,
    pub remote: String,
    pub main_branch: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BenchConfig {
    pub path: PathBuf,
    pub standin_branch: String,
}

#[derive(Debug)]
pub struct BenchObservation {
    pub bench: BenchConfig,
    pub worktree: WorktreeState,
    pub branch: CurrentBranch,
    pub operation: OperationState,
    pub standin: StandinState,
    pub pull_requests: PullRequestState,
}

#[derive(Debug)]
pub enum WorktreeState {
    Clean,
    Dirty,
}

#[derive(Debug)]
pub enum CurrentBranch {
    Attached(String),
    Detached,
}

#[derive(Debug)]
pub enum OperationState {
    Normal,
    InProgress(Vec<GitOperation>),
}

#[derive(Clone, Debug)]
pub enum GitOperation {
    Merge,
    Rebase,
    CherryPick,
}

#[derive(Debug)]
pub enum StandinState {
    Ready {
        standin_commit: String,
        upstream_commit: String,
    },
    Missing,
    CheckedOutElsewhere(PathBuf),
    UpstreamNotFetched,
    NotFastForwardable,
}

#[derive(Debug)]
pub enum PullRequestState {
    NotChecked,
    Matches(Vec<PullRequest>),
}

#[derive(Debug)]
pub struct PullRequest {
    pub number: u64,
    pub merged: bool,
}

#[derive(Debug)]
pub enum BenchDecision {
    Eligible { branch: String, pull_request: u64 },
    Skip(SkipReason),
}

#[derive(Debug)]
pub enum SkipReason {
    DirtyWorktree,
    DetachedHead,
    OperationInProgress(Vec<GitOperation>),
    StandinMissing,
    StandinCheckedOutElsewhere(PathBuf),
    UpstreamNotFetched,
    StandinNotFastForwardable,
    PullRequestNotChecked,
    PullRequestDoesNotMatch,
}

pub fn decide(observation: &BenchObservation) -> BenchDecision {
    match observation.worktree {
        WorktreeState::Dirty => BenchDecision::Skip(SkipReason::DirtyWorktree),
        WorktreeState::Clean => match &observation.operation {
            OperationState::InProgress(operations) => {
                BenchDecision::Skip(SkipReason::OperationInProgress(operations.clone()))
            }
            OperationState::Normal => match &observation.branch {
                CurrentBranch::Detached => BenchDecision::Skip(SkipReason::DetachedHead),
                CurrentBranch::Attached(branch) => match &observation.standin {
                    StandinState::Missing => BenchDecision::Skip(SkipReason::StandinMissing),
                    StandinState::UpstreamNotFetched => {
                        BenchDecision::Skip(SkipReason::UpstreamNotFetched)
                    }
                    StandinState::NotFastForwardable => {
                        BenchDecision::Skip(SkipReason::StandinNotFastForwardable)
                    }
                    StandinState::CheckedOutElsewhere(path) => {
                        BenchDecision::Skip(SkipReason::StandinCheckedOutElsewhere(path.clone()))
                    }
                    StandinState::Ready { .. } => match &observation.pull_requests {
                        PullRequestState::NotChecked => {
                            BenchDecision::Skip(SkipReason::PullRequestNotChecked)
                        }
                        PullRequestState::Matches(pull_requests)
                            if pull_requests.len() == 1 && pull_requests[0].merged =>
                        {
                            BenchDecision::Eligible {
                                branch: branch.clone(),
                                pull_request: pull_requests[0].number,
                            }
                        }
                        PullRequestState::Matches(_) => {
                            BenchDecision::Skip(SkipReason::PullRequestDoesNotMatch)
                        }
                    },
                },
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BenchConfig, BenchDecision, BenchObservation, CurrentBranch, OperationState,
        PullRequestState, SkipReason, StandinState, WorktreeState, decide,
    };
    use std::path::PathBuf;

    fn observation() -> BenchObservation {
        BenchObservation {
            bench: BenchConfig {
                path: PathBuf::from("/bench"),
                standin_branch: "main-01".to_owned(),
            },
            worktree: WorktreeState::Clean,
            branch: CurrentBranch::Attached("feature".to_owned()),
            operation: OperationState::Normal,
            standin: StandinState::Ready {
                standin_commit: "def456".to_owned(),
                upstream_commit: "abc123".to_owned(),
            },
            pull_requests: PullRequestState::Matches(vec![super::PullRequest {
                number: 42,
                merged: true,
            }]),
        }
    }

    #[test]
    fn dirty_worktree_wins_over_other_eligibility_signals() {
        let mut observation = observation();
        observation.worktree = WorktreeState::Dirty;

        assert!(matches!(
            decide(&observation),
            BenchDecision::Skip(SkipReason::DirtyWorktree)
        ));
    }

    #[test]
    fn one_merged_pull_request_is_eligible() {
        assert!(matches!(
            decide(&observation()),
            BenchDecision::Eligible {
                pull_request: 42,
                ..
            }
        ));
    }
}
