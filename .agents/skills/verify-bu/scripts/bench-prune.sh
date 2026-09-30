#!/usr/bin/env bash
# Benchmark and correctness gate for `bu prune --dry-run`.
#
# Builds a baseline binary from a Git ref and a candidate binary from the
# working tree, then runs both against the real configured repository in
# alternating rounds. The two outputs from each round must be identical.
# Never runs `bu prune` without `--dry-run`.
#
# Usage: bench-prune.sh [--baseline-ref REF] [--runs N] [--artifacts DIR] [--doctor]
set -euo pipefail

repo_root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
baseline_ref=main
runs=3
artifacts="${BU_VERIFY_ARTIFACTS:-${TMPDIR:-/tmp}/bu-verify}"
doctor_only=0
while [ $# -gt 0 ]; do
  case "$1" in
    --baseline-ref) baseline_ref=$2; shift 2 ;;
    --runs) runs=$2; shift 2 ;;
    --artifacts) artifacts=$2; shift 2 ;;
    --doctor) doctor_only=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

source_config="${BU_CONFIG:-$HOME/.config/bu/config.toml}"

doctor() {
  local ok=1
  command -v cargo >/dev/null || { echo "doctor: cargo missing"; ok=0; }
  command -v perl >/dev/null || { echo "doctor: perl missing"; ok=0; }
  gh auth status >/dev/null 2>&1 || { echo "doctor: gh is not authenticated"; ok=0; }
  [ -f "$source_config" ] || { echo "doctor: config $source_config missing"; ok=0; }
  if [ -f "$source_config" ]; then
    local repository
    repository=$(sed -n 's/^path = "\(.*\)"$/\1/p' "$source_config" | head -1)
    git -C "$repository" rev-parse --git-dir >/dev/null 2>&1 \
      || { echo "doctor: configured repository $repository is not a Git repository"; ok=0; }
    echo "doctor: repository $repository ($(git -C "$repository" worktree list | wc -l | tr -d ' ') registered worktrees)"
  fi
  git -C "$repo_root" rev-parse --verify --quiet "$baseline_ref^{commit}" >/dev/null \
    || { echo "doctor: baseline ref $baseline_ref not found"; ok=0; }
  echo "doctor: candidate checkout $repo_root at $(git -C "$repo_root" rev-parse --short HEAD)$(git -C "$repo_root" diff --quiet HEAD || echo ' (+ uncommitted changes)')"
  [ "$ok" = 1 ] && echo "doctor: ok"
  [ "$ok" = 1 ]
}

doctor
[ "$doctor_only" = 1 ] && exit 0

run_id=$(date +%Y%m%d-%H%M%S)-$$
run_dir="$artifacts/$run_id"
mkdir -p "$run_dir" "$artifacts/cache"

# A copied config passed with --config is never rewritten by bu, so the
# user's live config cannot gain auto-discovered bench entries.
config="$run_dir/config.toml"
cp "$source_config" "$config"
repository=$(sed -n 's/^path = "\(.*\)"$/\1/p' "$config" | head -1)

# Baseline: build the ref once per commit and cache the binary.
baseline_sha=$(git -C "$repo_root" rev-parse "$baseline_ref^{commit}")
baseline_bin="$artifacts/cache/bu-$baseline_sha"
if [ ! -x "$baseline_bin" ]; then
  build_dir="$artifacts/cache/src-$baseline_sha"
  rm -rf "$build_dir" && mkdir -p "$build_dir"
  git -C "$repo_root" archive "$baseline_sha" | tar -x -C "$build_dir"
  cargo build --quiet --release --manifest-path "$build_dir/Cargo.toml" --target-dir "$build_dir/target"
  cp "$build_dir/target/release/bu" "$baseline_bin"
  rm -rf "$build_dir"
fi

cargo build --quiet --release --manifest-path "$repo_root/Cargo.toml"
candidate_bin="$run_dir/bu-candidate"
cp "$repo_root/target/release/bu" "$candidate_bin"

snapshot() {
  { git -C "$repository" worktree list --porcelain; git -C "$repository" for-each-ref --format='%(refname) %(objectname)' refs/heads; } > "$1"
}

# Run from a neutral folder so the "current directory" skip never varies.
cd "$run_dir"
snapshot "$run_dir/state-before.txt"

time_run() { # binary, output file -> prints seconds
  perl -MTime::HiRes=time -e '
    my ($bin, $config, $out) = @ARGV;
    my $start = time;
    my $status = system("$bin --config \Q$config\E prune --dry-run --verbose --color never > \Q$out\E 2>&1");
    printf "%.3f %d\n", time - $start, $status >> 8;
  ' "$1" "$config" "$2"
}

# Each round runs baseline, candidate, baseline. Other sessions can change the
# live repository mid-run, so a round counts only when both baseline outputs
# agree; otherwise it is retried, up to two extra attempts.
same() { [ "$2" = "$4" ] && cmp -s "$1" "$3"; }
mismatches=0
inconclusive=0
: > "$run_dir/times.tsv"
for round in $(seq 1 "$runs"); do
  for attempt in 1 2 3; do
    tag="$round-$attempt"
    read -r b1_seconds b1_exit < <(time_run "$baseline_bin" "$run_dir/baseline-$tag-a.txt")
    read -r cand_seconds cand_exit < <(time_run "$candidate_bin" "$run_dir/candidate-$tag.txt")
    read -r b2_seconds b2_exit < <(time_run "$baseline_bin" "$run_dir/baseline-$tag-b.txt")
    if same "$run_dir/baseline-$tag-a.txt" "$b1_exit" "$run_dir/baseline-$tag-b.txt" "$b2_exit"; then
      break
    fi
    echo "round $tag: live repository changed during the round; retrying" >&2
  done
  base_seconds=$(awk -v a="$b1_seconds" -v b="$b2_seconds" 'BEGIN { printf "%.3f", (a < b) ? a : b }')
  if ! same "$run_dir/baseline-$tag-a.txt" "$b1_exit" "$run_dir/baseline-$tag-b.txt" "$b2_exit"; then
    inconclusive=$((inconclusive + 1))
    continue
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' "$round" "$base_seconds" "$cand_seconds" "$b1_exit" "$cand_exit" >> "$run_dir/times.tsv"
  if ! same "$run_dir/baseline-$tag-a.txt" "$b1_exit" "$run_dir/candidate-$tag.txt" "$cand_exit"; then
    mismatches=$((mismatches + 1))
    diff "$run_dir/baseline-$tag-a.txt" "$run_dir/candidate-$tag.txt" > "$run_dir/diff-$tag.txt" || true
  fi
done
[ -s "$run_dir/times.tsv" ] || { echo "no conclusive rounds; the live repository kept changing" >&2; exit 3; }

# Count subprocesses in one extra run per binary. Shims log each call and
# exec the real tool; wall time is noisy on a busy machine, call counts are not.
shims="$run_dir/shims"
mkdir -p "$shims"
for tool in git gh; do
  real=$(command -v "$tool")
  printf '#!/bin/sh\necho %s >> "$BU_VERIFY_CALL_LOG"\nexec %s "$@"\n' "$tool" "$real" > "$shims/$tool"
  chmod +x "$shims/$tool"
done
count_calls() { # binary, label -> prints "git_calls gh_calls"
  local log="$run_dir/calls-$2.log"
  : > "$log"
  BU_VERIFY_CALL_LOG="$log" PATH="$shims:$PATH" "$1" --config "$config" prune --dry-run --color never > /dev/null 2>&1 || true
  echo "$(grep -cx git "$log") $(grep -cx gh "$log")"
}
read -r base_git base_gh < <(count_calls "$baseline_bin" baseline)
read -r cand_git cand_gh < <(count_calls "$candidate_bin" candidate)

snapshot "$run_dir/state-after.txt"
state_changed=0
cmp -s "$run_dir/state-before.txt" "$run_dir/state-after.txt" || state_changed=1

median() { sort -n | awk '{ v[NR] = $1 } END { print (NR % 2) ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2 }'; }
base_median=$(cut -f2 "$run_dir/times.tsv" | median)
cand_median=$(cut -f3 "$run_dir/times.tsv" | median)
speedup=$(awk -v b="$base_median" -v c="$cand_median" 'BEGIN { printf "%.2f", b / c }')

{
  echo "baseline_ref=$baseline_ref ($baseline_sha)"
  echo "runs=$runs"
  echo "baseline_median_s=$base_median"
  echo "candidate_median_s=$cand_median"
  echo "speedup=${speedup}x"
  echo "baseline_calls=git:$base_git gh:$base_gh"
  echo "candidate_calls=git:$cand_git gh:$cand_gh"
  echo "output_mismatches=$mismatches"
  echo "inconclusive_rounds=$inconclusive"
  echo "repository_state_changed=$state_changed (informational: other sessions may change it)"
  echo "artifacts=$run_dir"
} | tee "$run_dir/summary.txt"

[ "$mismatches" = 0 ]
