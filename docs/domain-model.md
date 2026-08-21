# `bu` domain model

**Bench:** A durable Git worktree paired with one stand-in branch. The default config discovers numbered sibling benches, while explicit config files list them directly.
_Avoid_: branch, work bench
_Code_: NEW ENTITY `BenchConfig`

**Worktree cleanliness:** The absence of staged, unstaged, and untracked changes in a managed bench.
_Avoid_: clean branch
_Code_: NEW ENTITY `BenchObservation`

**Dirty file:** A staged, unstaged, untracked, renamed, or copied path reported by Git for a configured bench, including its index and worktree status.
_Code_: NEW ENTITY `DirtyFile`

**Feature branch:** The current attached local branch whose pull-request state determines whether a bench may be recycled.
_Code_: Git symbolic ref represented in `BenchObservation`

**Stand-in branch:** The long-lived `main-NN` branch that represents `main` for one durable bench.
_Avoid_: feature branch, bench branch
_Code_: NEW ENTITY `BenchConfig.standin_branch`

**Eligible bench:** A bench whose worktree is clean, Git operation state is normal, current branch maps unambiguously to one merged pull request, and stand-in branch can fast-forward to fetched `origin/main`.
_Code_: NEW ENTITY `EligibleBench`

**Forceable bench:** A dirty bench that passes every non-cleanliness safety check and has exactly one merged pull request whose head commit equals local `HEAD`. Status is read-only; only `bu recycle --force` may discard its staged, unstaged, and untracked changes.
_Avoid_: eligible bench, generally safe dirty bench
_Code_: `BenchDecision::Forceable`

**Recycled bench:** A bench checked out on its stand-in branch at fetched `origin/main` while its former feature branch ref remains unchanged.
_Code_: NEW ENTITY `RecycleOutcome`

**Ordinary skip:** A classified ineligible state that leaves the bench unchanged and does not make the process fail.
_Avoid_: error
_Code_: NEW ENTITY `SkipReason`

**Operational error:** A configuration, subprocess, network, inspection, or mutation failure that prevents reliable processing and makes the overall process exit nonzero.
_Avoid_: skip
_Code_: NEW ENTITY `OperationalError`
