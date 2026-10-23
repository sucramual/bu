use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub repository: RepositoryConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub benches: Vec<BenchConfig>,
    #[serde(default, skip_serializing_if = "PruneConfig::is_empty")]
    pub prune: PruneConfig,
}

/// Optional `[prune]` table.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PruneConfig {
    /// Git glob pathspecs, relative to the worktree root. A worktree that holds
    /// an ignored, untracked file matching one of them is never pruned.
    #[serde(default)]
    pub keep: Vec<String>,
}

impl PruneConfig {
    fn is_empty(&self) -> bool {
        self.keep.is_empty()
    }
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

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub lifecycle: PullRequestLifecycle,
    pub head_commit: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestLifecycle {
    Open,
    Merged,
    ClosedUnmerged,
}

impl PullRequest {
    pub fn is_merged(&self) -> bool {
        self.lifecycle == PullRequestLifecycle::Merged
    }
}

#[derive(Debug)]
pub enum BenchDecision {
    Eligible {
        branch: String,
        pull_request: u64,
    },
    Forceable {
        branch: String,
        pull_request: u64,
        files: Vec<DirtyFile>,
    },
    Skip(SkipReason),
}

#[derive(Debug)]
pub enum SkipReason {
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
    let OperationState::Normal = &observation.operation else {
        let OperationState::InProgress(operations) = &observation.operation else {
            unreachable!()
        };
        return BenchDecision::Skip(SkipReason::OperationInProgress(operations.clone()));
    };
    let CurrentBranch::Attached { name, commit } = &observation.branch else {
        return BenchDecision::Skip(SkipReason::DetachedHead);
    };
    if name == &observation.bench.standin_branch {
        return BenchDecision::Skip(SkipReason::AlreadyOnStandin);
    }
    match &observation.standin {
        StandinState::Missing => return BenchDecision::Skip(SkipReason::StandinMissing),
        StandinState::UpstreamNotFetched => {
            return BenchDecision::Skip(SkipReason::UpstreamNotFetched);
        }
        StandinState::NotFastForwardable => {
            return BenchDecision::Skip(SkipReason::StandinNotFastForwardable);
        }
        StandinState::CheckedOutElsewhere(path) => {
            return BenchDecision::Skip(SkipReason::StandinCheckedOutElsewhere(path.clone()));
        }
        StandinState::Ready { .. } => {}
    }
    let PullRequestState::Matches(pull_requests) = &observation.pull_requests else {
        return BenchDecision::Skip(SkipReason::PullRequestNotChecked);
    };
    if pull_requests.len() != 1
        || !pull_requests[0].is_merged()
        || pull_requests[0].head_commit != *commit
    {
        return BenchDecision::Skip(SkipReason::PullRequestDoesNotMatch);
    }

    match &observation.worktree {
        WorktreeState::Clean => BenchDecision::Eligible {
            branch: name.clone(),
            pull_request: pull_requests[0].number,
        },
        WorktreeState::Dirty { files } => BenchDecision::Forceable {
            branch: name.clone(),
            pull_request: pull_requests[0].number,
            files: files.clone(),
        },
    }
}

/// Branches that `bu prune` never deletes: the configured main branch and every
/// bench stand-in branch.
#[derive(Debug)]
pub struct ProtectedBranches {
    pub main_branch: String,
    pub standin_branches: Vec<String>,
}

impl ProtectedBranches {
    pub fn from_config(config: &Config) -> Self {
        Self {
            main_branch: config.repository.main_branch.clone(),
            standin_branches: config
                .benches
                .iter()
                .map(|bench| bench.standin_branch.clone())
                .collect(),
        }
    }

    pub(crate) fn protection(&self, branch: &str) -> Option<BranchProtection> {
        if branch == self.main_branch {
            Some(BranchProtection::MainBranch)
        } else if self
            .standin_branches
            .iter()
            .any(|standin| standin == branch)
        {
            Some(BranchProtection::StandinBranch)
        } else {
            None
        }
    }
}

/// Git state of one existing, unlocked scratch worktree.
#[derive(Debug)]
pub struct ScratchObservation {
    pub worktree: WorktreeState,
    pub branch: CurrentBranch,
    pub operation: OperationState,
    /// Ignored, untracked files that match a `prune.keep` pattern.
    pub kept_ignored: Vec<PathBuf>,
    pub pull_requests: PullRequestState,
    /// How local `HEAD` relates to the head of the one merged pull request,
    /// asked only when the two commits differ.
    pub head_ancestry: HeadAncestry,
}

/// What Git proved about local `HEAD` and a different merged head commit.
#[derive(Clone, Copy, Debug)]
pub enum HeadAncestry {
    /// Not asked. Never counts as proof.
    NotChecked,
    /// Local `HEAD` is a strict ancestor of the merged head, this many commits
    /// behind it.
    Behind(u64),
    /// Local `HEAD` has commits that the merged head lacks.
    NotAncestor,
    /// The merged head commit is not in the local object store.
    MissingHead,
}

/// The proof that made a scratch worktree prunable.
#[derive(Clone, Copy, Debug)]
pub enum HeadProof {
    /// The merged head commit equals local `HEAD`.
    Exact,
    /// Local `HEAD` is a strict ancestor of the merged head, this many commits
    /// behind it.
    Behind(u64),
}

#[derive(Debug)]
pub enum ScratchDecision {
    Prunable {
        branch: String,
        /// Local `HEAD`, which the branch delete pins. Never the merged head.
        commit: String,
        pull_request: u64,
        proof: HeadProof,
    },
    Skip(PruneSkipReason),
}

#[derive(Clone, Copy, Debug)]
pub enum BranchProtection {
    MainBranch,
    StandinBranch,
}

#[derive(Debug)]
pub enum PruneSkipReason {
    Locked,
    ContainsCurrentDirectory,
    /// Running processes have their current directory in the worktree.
    InUse {
        processes: usize,
        commands: Vec<String>,
    },
    OperationInProgress(Vec<GitOperation>),
    DetachedHead,
    ProtectedBranch {
        branch: String,
        protection: BranchProtection,
    },
    Dirty(Vec<DirtyFile>),
    /// Ignored files that a `prune.keep` pattern matches.
    KeptIgnored(Vec<PathBuf>),
    PullRequestNotChecked,
    NoPullRequest,
    OpenPullRequest(u64),
    ClosedPullRequest(u64),
    AmbiguousPullRequests(usize),
    HeadMismatch {
        pull_request: u64,
        pull_request_head: String,
        local_head: String,
    },
    /// The merged head differs from local `HEAD` and is not in the local
    /// object store, so its ancestry is unknown. `bu` never fetches it.
    MergedHeadMissing {
        pull_request: u64,
        pull_request_head: String,
        local_head: String,
    },
}

/// Returns whether local pull-request lookup is worth doing: every local check
/// already passes and only the GitHub proof remains.
pub fn scratch_needs_pull_requests(
    worktree: &WorktreeState,
    branch: &CurrentBranch,
    operation: &OperationState,
    protected: &ProtectedBranches,
) -> bool {
    matches!(operation, OperationState::Normal)
        && matches!(worktree, WorktreeState::Clean)
        && matches!(branch, CurrentBranch::Attached { name, .. } if protected.protection(name).is_none())
}

pub fn decide_scratch(
    observation: &ScratchObservation,
    protected: &ProtectedBranches,
) -> ScratchDecision {
    if let OperationState::InProgress(operations) = &observation.operation {
        return ScratchDecision::Skip(PruneSkipReason::OperationInProgress(operations.clone()));
    }
    let CurrentBranch::Attached { name, commit } = &observation.branch else {
        return ScratchDecision::Skip(PruneSkipReason::DetachedHead);
    };
    if let Some(protection) = protected.protection(name) {
        return ScratchDecision::Skip(PruneSkipReason::ProtectedBranch {
            branch: name.clone(),
            protection,
        });
    }
    if let WorktreeState::Dirty { files } = &observation.worktree {
        return ScratchDecision::Skip(PruneSkipReason::Dirty(files.clone()));
    }
    if !observation.kept_ignored.is_empty() {
        return ScratchDecision::Skip(PruneSkipReason::KeptIgnored(
            observation.kept_ignored.clone(),
        ));
    }
    let PullRequestState::Matches(pull_requests) = &observation.pull_requests else {
        return ScratchDecision::Skip(PruneSkipReason::PullRequestNotChecked);
    };
    if let Some(open) = pull_requests
        .iter()
        .find(|pull_request| pull_request.lifecycle == PullRequestLifecycle::Open)
    {
        return ScratchDecision::Skip(PruneSkipReason::OpenPullRequest(open.number));
    }
    let merged: Vec<_> = pull_requests
        .iter()
        .filter(|pull_request| pull_request.is_merged())
        .collect();
    match merged.as_slice() {
        [] => ScratchDecision::Skip(match pull_requests.first() {
            Some(closed) => PruneSkipReason::ClosedPullRequest(closed.number),
            None => PruneSkipReason::NoPullRequest,
        }),
        [pull_request] if pull_request.head_commit == *commit => ScratchDecision::Prunable {
            branch: name.clone(),
            commit: commit.clone(),
            pull_request: pull_request.number,
            proof: HeadProof::Exact,
        },
        [pull_request] => match observation.head_ancestry {
            // A distance of 0 means Git resolved both names to one commit,
            // which is not a strict ancestor.
            HeadAncestry::Behind(commits) if commits > 0 => ScratchDecision::Prunable {
                branch: name.clone(),
                commit: commit.clone(),
                pull_request: pull_request.number,
                proof: HeadProof::Behind(commits),
            },
            HeadAncestry::MissingHead => {
                ScratchDecision::Skip(PruneSkipReason::MergedHeadMissing {
                    pull_request: pull_request.number,
                    pull_request_head: pull_request.head_commit.clone(),
                    local_head: commit.clone(),
                })
            }
            HeadAncestry::NotChecked | HeadAncestry::NotAncestor | HeadAncestry::Behind(_) => {
                ScratchDecision::Skip(PruneSkipReason::HeadMismatch {
                    pull_request: pull_request.number,
                    pull_request_head: pull_request.head_commit.clone(),
                    local_head: commit.clone(),
                })
            }
        },
        many => ScratchDecision::Skip(PruneSkipReason::AmbiguousPullRequests(many.len())),
    }
}

#[cfg(test)]
mod scratch_tests {
    use super::{
        CurrentBranch, HeadAncestry, HeadProof, OperationState, ProtectedBranches, PruneSkipReason,
        PullRequest, PullRequestLifecycle, PullRequestState, ScratchDecision, ScratchObservation,
        WorktreeState, decide_scratch,
    };

    fn protected() -> ProtectedBranches {
        ProtectedBranches {
            main_branch: "main".to_owned(),
            standin_branches: vec!["main-01".to_owned()],
        }
    }

    fn observation(pull_requests: Vec<PullRequest>) -> ScratchObservation {
        ScratchObservation {
            worktree: WorktreeState::Clean,
            branch: CurrentBranch::Attached {
                name: "scratch/one".to_owned(),
                commit: "tip".to_owned(),
            },
            operation: OperationState::Normal,
            kept_ignored: Vec::new(),
            pull_requests: PullRequestState::Matches(pull_requests),
            head_ancestry: HeadAncestry::NotChecked,
        }
    }

    fn pull_request(number: u64, lifecycle: PullRequestLifecycle, head: &str) -> PullRequest {
        PullRequest {
            number,
            lifecycle,
            head_commit: head.to_owned(),
        }
    }

    #[test]
    fn a_closed_attempt_does_not_block_a_later_exact_merge() {
        let decision = decide_scratch(
            &observation(vec![
                pull_request(1, PullRequestLifecycle::ClosedUnmerged, "old"),
                pull_request(2, PullRequestLifecycle::Merged, "tip"),
            ]),
            &protected(),
        );

        assert!(matches!(
            decision,
            ScratchDecision::Prunable {
                pull_request: 2,
                ..
            }
        ));
    }

    #[test]
    fn an_open_pull_request_blocks_even_beside_an_exact_merge() {
        let decision = decide_scratch(
            &observation(vec![
                pull_request(1, PullRequestLifecycle::Merged, "tip"),
                pull_request(2, PullRequestLifecycle::Open, "tip"),
            ]),
            &protected(),
        );

        assert!(matches!(
            decision,
            ScratchDecision::Skip(PruneSkipReason::OpenPullRequest(2))
        ));
    }

    /// The prune pipeline always asks Git before the final decision, so no
    /// end-to-end test reaches an unchecked or zero-distance answer.
    #[test]
    fn only_a_strict_ancestor_proves_a_different_merged_head() {
        for ancestry in [
            HeadAncestry::NotChecked,
            HeadAncestry::NotAncestor,
            HeadAncestry::Behind(0),
        ] {
            let mut observation = observation(vec![pull_request(
                1,
                PullRequestLifecycle::Merged,
                "merged",
            )]);
            observation.head_ancestry = ancestry;

            assert!(
                matches!(
                    decide_scratch(&observation, &protected()),
                    ScratchDecision::Skip(PruneSkipReason::HeadMismatch { .. })
                ),
                "{ancestry:?}"
            );
        }

        let mut behind = observation(vec![pull_request(
            1,
            PullRequestLifecycle::Merged,
            "merged",
        )]);
        behind.head_ancestry = HeadAncestry::Behind(2);
        assert!(matches!(
            decide_scratch(&behind, &protected()),
            ScratchDecision::Prunable {
                ref commit,
                proof: HeadProof::Behind(2),
                ..
            } if commit == "tip"
        ));
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
                lifecycle: super::PullRequestLifecycle::Merged,
                head_commit: "feature-commit".to_owned(),
            }]),
        }
    }

    #[test]
    fn dirty_exact_match_is_forceable() {
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
            BenchDecision::Forceable {
                pull_request: 42,
                ..
            }
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
