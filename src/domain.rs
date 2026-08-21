use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub repository: RepositoryConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benches: Vec<BenchConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RepositoryConfig {
    pub path: PathBuf,
    pub remote: String,
    pub main_branch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
    Dirty { files: Vec<DirtyFile> },
}

#[derive(Debug)]
pub struct DirtyFile {
    pub index_status: char,
    pub worktree_status: char,
    pub path: PathBuf,
    pub original_path: Option<PathBuf>,
}

#[derive(Debug)]
pub enum CurrentBranch {
    Attached { name: String, commit: String },
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
    pub head_commit: String,
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
    AlreadyOnStandin,
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
        WorktreeState::Dirty { .. } => BenchDecision::Skip(SkipReason::DirtyWorktree),
        WorktreeState::Clean => match &observation.operation {
            OperationState::InProgress(operations) => {
                BenchDecision::Skip(SkipReason::OperationInProgress(operations.clone()))
            }
            OperationState::Normal => match &observation.branch {
                CurrentBranch::Detached => BenchDecision::Skip(SkipReason::DetachedHead),
                CurrentBranch::Attached { name, .. }
                    if name == &observation.bench.standin_branch =>
                {
                    BenchDecision::Skip(SkipReason::AlreadyOnStandin)
                }
                CurrentBranch::Attached { name, commit } => match &observation.standin {
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
                            if pull_requests.len() == 1
                                && pull_requests[0].merged
                                && pull_requests[0].head_commit == *commit =>
                        {
                            BenchDecision::Eligible {
                                branch: name.clone(),
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
            branch: CurrentBranch::Attached {
                name: "feature".to_owned(),
                commit: "feature-commit".to_owned(),
            },
            operation: OperationState::Normal,
            standin: StandinState::Ready {
                standin_commit: "def456".to_owned(),
                upstream_commit: "abc123".to_owned(),
            },
            pull_requests: PullRequestState::Matches(vec![super::PullRequest {
                number: 42,
                merged: true,
                head_commit: "feature-commit".to_owned(),
            }]),
        }
    }

    #[test]
    fn dirty_worktree_wins_over_other_eligibility_signals() {
        let mut observation = observation();
        observation.worktree = WorktreeState::Dirty {
            files: vec![super::DirtyFile {
                index_status: 'M',
                worktree_status: ' ',
                path: PathBuf::from("README.md"),
                original_path: None,
            }],
        };

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

    #[test]
    fn current_standin_branch_is_skipped() {
        let mut observation = observation();
        observation.branch = CurrentBranch::Attached {
            name: "main-01".to_owned(),
            commit: "feature-commit".to_owned(),
        };

        assert!(matches!(
            decide(&observation),
            BenchDecision::Skip(SkipReason::AlreadyOnStandin)
        ));
    }

    #[test]
    fn merged_pull_request_with_a_different_head_commit_is_skipped() {
        let mut observation = observation();
        let PullRequestState::Matches(pull_requests) = &mut observation.pull_requests else {
            panic!("fixture has pull request matches");
        };
        pull_requests[0].head_commit = "different-commit".to_owned();

        assert!(matches!(
            decide(&observation),
            BenchDecision::Skip(SkipReason::PullRequestDoesNotMatch)
        ));
    }
}
