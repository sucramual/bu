# `bu` domain model

**Bench:** A configured durable Git worktree paired with one stand-in branch.
_Avoid_: branch, work bench
_Code_: NEW ENTITY `BenchConfig`

**Worktree cleanliness:** The absence of staged, unstaged, and untracked changes in a configured bench.
_Avoid_: clean branch
_Code_: NEW ENTITY `BenchObservation`

**Feature branch:** The current attached local branch whose pull-request state determines whether a bench may be recycled.
_Code_: Git symbolic ref represented in `BenchObservation`

**Stand-in branch:** The configured long-lived `main-NN` branch that represents `main` for one durable bench.
_Avoid_: feature branch, bench branch
_Code_: NEW ENTITY `BenchConfig.standin_branch`

**Eligible bench:** A bench whose worktree is clean, Git operation state is normal, current branch maps unambiguously to one merged pull request, and stand-in branch can fast-forward to fetched `origin/main`.
_Code_: NEW ENTITY `EligibleBench`

**Recycled bench:** A bench checked out on its stand-in branch at fetched `origin/main` while its former feature branch ref remains unchanged.
_Code_: NEW ENTITY `RecycleOutcome`

**Ordinary skip:** A classified ineligible state that leaves the bench unchanged and does not make the process fail.
_Avoid_: error
_Code_: NEW ENTITY `SkipReason`

**Operational error:** A configuration, subprocess, network, inspection, or mutation failure that prevents reliable processing and makes the overall process exit nonzero.
_Avoid_: skip
_Code_: NEW ENTITY `OperationalError`
