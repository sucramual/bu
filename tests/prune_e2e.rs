use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const MISMATCHED_HEAD: &str = "0000000000000000000000000000000000000000";

fn run(program: &str, arguments: &[&str], cwd: &Path) -> Output {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .output()
        .expect("command should start");
    assert!(
        output.status.success(),
        "{program} {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git(repository: &Path, arguments: &[&str]) -> String {
    String::from_utf8(run("git", arguments, repository).stdout).expect("Git output is UTF-8")
}

fn ref_exists(repository: &Path, reference: &str) -> bool {
    Command::new("git")
        .args(["show-ref", "--verify", "--quiet", reference])
        .current_dir(repository)
        .status()
        .expect("git show-ref should start")
        .success()
}

fn real_git() -> String {
    String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("find real git")
            .stdout,
    )
    .expect("Git path is UTF-8")
    .trim()
    .to_owned()
}

/// A repository whose main checkout is on `home`, so `main` stays free for scratch worktrees.
fn prune_fixture() -> (TempDir, PathBuf) {
    let temporary = TempDir::new().expect("temporary directory");
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).expect("repository directory");

    git(&repository, &["init", "--initial-branch=main"]);
    git(&repository, &["config", "user.name", "bu test"]);
    git(&repository, &["config", "user.email", "bu@example.test"]);
    fs::write(repository.join("README.md"), "fixture\n").expect("fixture file");
    fs::write(repository.join(".gitignore"), "ignored.log\n").expect("ignore fixture");
    git(&repository, &["add", "README.md", ".gitignore"]);
    git(&repository, &["commit", "-m", "initial fixture"]);
    git(
        &repository,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(&repository, &["switch", "-c", "home"]);

    (temporary, repository)
}

/// Adds a sibling scratch worktree on a new branch with one unique commit.
fn add_scratch(repository: &Path, name: &str, branch: &str) -> PathBuf {
    let path = repository
        .parent()
        .expect("repository parent")
        .join(format!("scratch-{name}"));
    git(
        repository,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().expect("scratch path is UTF-8"),
            "main",
        ],
    );
    fs::write(path.join(format!("{name}.md")), format!("{name}\n")).expect("scratch change");
    git(&path, &["add", &format!("{name}.md")]);
    git(&path, &["commit", "-m", &format!("scratch {name}")]);
    path
}

fn write_repository_config(temporary: &TempDir, repository: &Path) -> PathBuf {
    let config = temporary.path().join("bu.toml");
    fs::write(
        &config,
        format!(
            "[repository]\npath = \"{}\"\nremote = \"origin\"\nmain_branch = \"main\"\n",
            repository.display(),
        ),
    )
    .expect("config file");
    config
}

fn write_bench_config(
    temporary: &TempDir,
    repository: &Path,
    bench: &Path,
    standin_branch: &str,
) -> PathBuf {
    let config = temporary.path().join("bu.toml");
    fs::write(
        &config,
        format!(
            "[repository]\npath = \"{}\"\nremote = \"origin\"\nmain_branch = \"main\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"{standin_branch}\"\n",
            repository.display(),
            bench.display(),
        ),
    )
    .expect("config file");
    config
}

fn write_executable(path: &Path, source: &str) {
    fs::write(path, source).expect("fake executable");
    let mut permissions = fs::metadata(path)
        .expect("fake executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake executable");
}

/// Answers `gh api graphql -f bN=<branch>...` like GitHub: one aliased
/// `nodes` list per branch, filled by the fake's `pull_requests <branch>`.
const GRAPHQL_DISPATCH: &str = r#"printf '{"data":{"repository":{'
separator=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -f)
      shift
      case "$1" in
        b[0-9]*=*)
          printf '%s"%s":{"nodes":' "$separator" "${1%%=*}"
          pull_requests "${1#*=}" | tr -d '
'
          printf '}'
          separator=',' ;;
      esac ;;
  esac
  shift
done
printf '}}}
'"#;

/// Fake `gh`: every branch has one merged pull request at its local tip unless a
/// branch-specific case below says otherwise. Also installs a fake `lsof` that
/// reports no processes, so tests never depend on the host's process table.
fn fake_gh(temporary: &TempDir) -> PathBuf {
    let bin = temporary.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    fake_lsof(&bin, "exit 0");
    write_executable(
        &bin.join("gh"),
        &format!(
            r#"#!/bin/sh
pull_requests() {{
head="$1"
oid=$(git rev-parse --verify --quiet "refs/heads/$head")
case "$head" in
  scratch/none)
    printf '[]\n' ;;
  scratch/open)
    printf '[{{"number":7,"state":"OPEN","mergedAt":null,"headRefName":"%s","headRefOid":"%s"}}]\n' "$head" "$oid" ;;
  scratch/closed)
    printf '[{{"number":8,"state":"CLOSED","mergedAt":null,"headRefName":"%s","headRefOid":"%s"}}]\n' "$head" "$oid" ;;
  scratch/mismatch)
    printf '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"%s","headRefOid":"{MISMATCHED_HEAD}"}}]\n' "$head" ;;
  scratch/ambiguous)
    printf '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"%s","headRefOid":"%s"}},{{"number":43,"state":"MERGED","mergedAt":"2026-07-29T00:00:00Z","headRefName":"%s","headRefOid":"%s"}}]\n' "$head" "$oid" "$head" "$oid" ;;
  scratch/other-branch)
    printf '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"someone/else","headRefOid":"%s"}}]\n' "$oid" ;;
  *)
    printf '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"%s","headRefOid":"%s"}}]\n' "$head" "$oid" ;;
esac
}}
{GRAPHQL_DISPATCH}"#
        ),
    );
    bin
}

/// Wraps real Git; `body` runs first and may `exit` before delegating.
fn fake_git(bin: &Path, body: &str) {
    write_executable(
        &bin.join("git"),
        &format!("#!/bin/sh\n{body}\nexec \"{}\" \"$@\"\n", real_git()),
    );
}

/// Replaces `lsof` with `body`. `$PPID` inside `body` is the pid of `bu`.
fn fake_lsof(bin: &Path, body: &str) {
    write_executable(&bin.join("lsof"), &format!("#!/bin/sh\n{body}\n"));
}

/// One `lsof -Fpcn` record for a process whose current directory is `cwd`.
fn lsof_record(pid: &str, command: &str, cwd: &Path) -> String {
    format!("p{pid}\nc{command}\nfcwd\nn{}\n", cwd.display())
}

/// A fake `lsof` body that prints `records` and exits with `status`.
fn lsof_printing(records: &str, status: u8) -> String {
    format!("cat <<'EOF'\n{records}EOF\nexit {status}")
}

fn bu_command(fake_bin: &Path) -> Command {
    let original_path = env::var_os("PATH").expect("PATH is set");
    let path = env::join_paths(
        std::iter::once(fake_bin.to_path_buf()).chain(env::split_paths(&original_path)),
    )
    .expect("valid PATH");
    let mut command = Command::new(env!("CARGO_BIN_EXE_bu"));
    command.env("PATH", path).env_remove("NO_COLOR");
    command
}

fn prune_command(config: &Path, fake_bin: &Path, arguments: &[&str]) -> Command {
    let mut command = bu_command(fake_bin);
    command
        .args(["--config"])
        .arg(config)
        .arg("prune")
        .args(arguments);
    command
}

fn prune(config: &Path, fake_bin: &Path, arguments: &[&str]) -> Output {
    prune_command(config, fake_bin, arguments)
        .output()
        .expect("bu should start")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn registered_worktrees(repository: &Path) -> String {
    git(repository, &["worktree", "list", "--porcelain"])
}

#[test]
fn prune_removes_a_merged_scratch_worktree_and_deletes_its_branch_with_a_zero_bench_config() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "done", "scratch/done");
    fs::write(scratch.join("ignored.log"), "ignored files do not block\n").expect("ignored file");
    git(
        &repository,
        &[
            "update-ref",
            "refs/remotes/origin/scratch/done",
            "scratch/done",
        ],
    );
    let remote_before = git(
        &repository,
        &["rev-parse", "refs/remotes/origin/scratch/done"],
    );
    let config = write_repository_config(&temporary, &repository);
    assert!(
        !fs::read_to_string(&config)
            .expect("config")
            .contains("[[benches]]")
    );
    let fake_bin = fake_gh(&temporary);

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains(
            "▎ pruned   scratch-done scratch/done merged pull request #42; removed worktree and branch\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "\nChecked 1 scratch worktree\n1 pruned, 0 cleaned, 0 kept, 0 blocked, 0 skipped, 0 failed\n"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains('\x1b'));
    assert!(!scratch.exists());
    assert!(!registered_worktrees(&repository).contains("scratch-done"));
    assert!(!ref_exists(&repository, "refs/heads/scratch/done"));
    assert_eq!(
        git(
            &repository,
            &["rev-parse", "refs/remotes/origin/scratch/done"]
        ),
        remote_before
    );
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "home"
    );
}

#[test]
fn prune_removes_a_squash_merged_branch_whose_commits_are_not_on_main() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "squashed", "scratch/squashed");
    let main_worktree = temporary.path().join("main-advance");
    git(
        &repository,
        &[
            "worktree",
            "add",
            main_worktree.to_str().expect("path is UTF-8"),
            "main",
        ],
    );
    fs::write(main_worktree.join("squashed.md"), "squashed\n").expect("squash content");
    git(&main_worktree, &["add", "squashed.md"]);
    git(&main_worktree, &["commit", "-m", "squash merge (#42)"]);
    git(
        &repository,
        &[
            "worktree",
            "remove",
            main_worktree.to_str().expect("path is UTF-8"),
        ],
    );
    let is_ancestor = Command::new("git")
        .args(["merge-base", "--is-ancestor", "scratch/squashed", "main"])
        .current_dir(&repository)
        .status()
        .expect("git merge-base should start");
    assert!(!is_ancestor.success(), "fixture must model a squash merge");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    assert!(stdout(&output).contains("▎ pruned   scratch-squashed scratch/squashed"));
    assert!(!scratch.exists());
    assert!(!ref_exists(&repository, "refs/heads/scratch/squashed"));
}

#[test]
fn prune_skips_every_unproven_scratch_worktree_and_leaves_it_untouched() {
    let (temporary, repository) = prune_fixture();
    let dirty = add_scratch(&repository, "dirty", "scratch/dirty");
    fs::write(dirty.join("README.md"), "tracked change\n").expect("dirty change");
    let untracked = add_scratch(&repository, "untracked", "scratch/untracked");
    fs::write(untracked.join("notes.txt"), "untracked only\n").expect("untracked file");
    let locked = add_scratch(&repository, "locked", "scratch/locked");
    git(
        &repository,
        &["worktree", "lock", locked.to_str().expect("path is UTF-8")],
    );
    let merging = add_scratch(&repository, "merging", "scratch/merging");
    let merge_head = git(&merging, &["rev-parse", "--git-path", "MERGE_HEAD"]);
    let merge_head = PathBuf::from(merge_head.trim());
    let merge_head = if merge_head.is_absolute() {
        merge_head
    } else {
        merging.join(merge_head)
    };
    fs::write(&merge_head, git(&repository, &["rev-parse", "main"])).expect("merge state");
    let detached = repository
        .parent()
        .expect("repository parent")
        .join("scratch-detached");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--detach",
            detached.to_str().expect("path is UTF-8"),
            "main",
        ],
    );
    let protected = repository
        .parent()
        .expect("repository parent")
        .join("scratch-protected");
    git(
        &repository,
        &[
            "worktree",
            "add",
            protected.to_str().expect("path is UTF-8"),
            "main",
        ],
    );
    let open = add_scratch(&repository, "open", "scratch/open");
    let closed = add_scratch(&repository, "closed", "scratch/closed");
    let mismatch = add_scratch(&repository, "mismatch", "scratch/mismatch");
    let ambiguous = add_scratch(&repository, "ambiguous", "scratch/ambiguous");
    let none = add_scratch(&repository, "none", "scratch/none");
    let other_branch = add_scratch(&repository, "other-branch", "scratch/other-branch");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    let worktrees_before = registered_worktrees(&repository);
    let refs_before = git(&repository, &["show-ref"]);

    let output = prune(&config, &fake_bin, &["--verbose"]);

    assert_success(&output);
    let stdout = stdout(&output);
    for expected in [
        "▎ blocked  scratch-dirty scratch/dirty dirty worktree (1 file)\n",
        "▎ blocked  scratch-untracked scratch/untracked dirty worktree (1 file)\n",
        "▎ blocked  scratch-locked scratch/locked worktree is locked\n",
        "▎ blocked  scratch-merging scratch/merging Git operation in progress: merge\n",
        "▎ blocked  scratch-detached detached HEAD is detached\n",
        "▎ skipped  scratch-protected main main is the configured main branch\n",
        "▎ skipped  scratch-open scratch/open pull request #7 is open\n",
        "▎ skipped  scratch-closed scratch/closed pull request #8 was closed without merging\n",
        "▎ blocked  scratch-mismatch scratch/mismatch merged pull request #42 has head 0000000, not local HEAD ",
        "▎ blocked  scratch-ambiguous scratch/ambiguous 2 merged pull requests; expected exactly one\n",
        "▎ skipped  scratch-none scratch/none no pull request found\n",
        "▎ skipped  scratch-other-branch scratch/other-branch no pull request found\n",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in\n{stdout}"
        );
    }
    assert!(stdout.contains("     M README.md\n"), "{stdout}");
    assert!(stdout.contains("    ?? notes.txt\n"), "{stdout}");
    assert!(stdout.contains(&format!(
        "    path: {}",
        fs::canonicalize(&dirty).expect("path").display()
    )));
    assert!(
        stdout.contains(
            "\nChecked 12 scratch worktrees\n0 pruned, 0 cleaned, 0 kept, 7 blocked, 5 skipped, 0 failed\n"
        ),
        "{stdout}"
    );
    for path in [
        &dirty,
        &untracked,
        &locked,
        &merging,
        &detached,
        &protected,
        &open,
        &closed,
        &mismatch,
        &ambiguous,
        &none,
        &other_branch,
    ] {
        assert!(path.exists(), "{} was removed", path.display());
    }
    assert_eq!(registered_worktrees(&repository), worktrees_before);
    assert_eq!(git(&repository, &["show-ref"]), refs_before);
    assert_eq!(
        fs::read_to_string(dirty.join("README.md")).expect("dirty file"),
        "tracked change\n"
    );
    assert!(untracked.join("notes.txt").exists());
}

#[test]
fn prune_never_considers_the_main_checkout_or_configured_benches() {
    let (temporary, repository) = prune_fixture();
    git(&repository, &["switch", "-c", "feature/main-checkout"]);
    let bench = repository
        .parent()
        .expect("repository parent")
        .join("repository-01");
    git(&repository, &["branch", "main-01", "main"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "feature/bench",
            bench.to_str().expect("path is UTF-8"),
            "main",
        ],
    );
    let standin_scratch = repository
        .parent()
        .expect("repository parent")
        .join("scratch-standin");
    git(
        &repository,
        &[
            "worktree",
            "add",
            standin_scratch.to_str().expect("path is UTF-8"),
            "main-01",
        ],
    );
    let scratch = add_scratch(&repository, "done", "scratch/done");
    let config = write_bench_config(&temporary, &repository, &bench, "main-01");
    let fake_bin = fake_gh(&temporary);

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(!stdout.contains(" repository "), "{stdout}");
    assert!(!stdout.contains("repository-01"), "{stdout}");
    assert!(
        stdout.contains("▎ skipped  scratch-standin main-01 main-01 is a bench stand-in branch\n"),
        "{stdout}"
    );
    assert!(stdout.contains("▎ pruned   scratch-done scratch/done"));
    assert!(stdout.contains("\nChecked 2 scratch worktrees\n"));
    assert!(repository.exists());
    assert!(bench.exists());
    assert!(standin_scratch.exists());
    assert!(!scratch.exists());
    assert!(ref_exists(&repository, "refs/heads/feature/main-checkout"));
    assert!(ref_exists(&repository, "refs/heads/feature/bench"));
    assert!(ref_exists(&repository, "refs/heads/main-01"));
}

#[test]
fn prune_keeps_a_branch_ref_that_moves_between_the_check_and_the_delete() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "moved", "scratch/moved");
    let moved_to = git(&repository, &["rev-parse", "main"]);
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_git(
        &fake_bin,
        &format!(
            r#"if [ "$1" = "worktree" ] && [ "$2" = "remove" ]; then
  "{real}" "$@" || exit $?
  exec "{real}" update-ref refs/heads/scratch/moved '{moved_to}'
fi"#,
            real = real_git(),
            moved_to = moved_to.trim(),
        ),
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains(&format!(
            "▎ kept     scratch-moved scratch/moved merged pull request #42; removed worktree; kept branch because it moved to {}\n",
            &moved_to.trim()[..7]
        )),
        "{stdout}"
    );
    assert!(stdout.contains("0 pruned, 0 cleaned, 1 kept, 0 blocked, 0 skipped, 0 failed\n"));
    assert!(!scratch.exists());
    assert_eq!(
        git(&repository, &["rev-parse", "refs/heads/scratch/moved"]),
        moved_to
    );
}

#[test]
fn prune_cleans_stale_metadata_for_missing_worktree_folders() {
    let (temporary, repository) = prune_fixture();
    let gone = add_scratch(&repository, "gone", "scratch/gone");
    let gone_commit = git(&repository, &["rev-parse", "scratch/gone"]);
    fs::remove_dir_all(&gone).expect("remove worktree folder");
    let locked_gone = add_scratch(&repository, "locked-gone", "scratch/locked-gone");
    git(
        &repository,
        &[
            "worktree",
            "lock",
            locked_gone.to_str().expect("path is UTF-8"),
        ],
    );
    fs::remove_dir_all(&locked_gone).expect("remove locked worktree folder");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains(
            "▎ cleaned  scratch-gone scratch/gone folder missing; pruned stale worktree metadata\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("▎ blocked  scratch-locked-gone scratch/locked-gone worktree is locked\n"),
        "{stdout}"
    );
    assert!(stdout.contains("0 pruned, 1 cleaned, 0 kept, 1 blocked, 0 skipped, 0 failed\n"));
    let worktrees = registered_worktrees(&repository);
    assert!(!worktrees.contains("scratch-gone\n"), "{worktrees}");
    assert!(worktrees.contains("scratch-locked-gone"), "{worktrees}");
    assert_eq!(
        git(&repository, &["rev-parse", "scratch/gone"]),
        gone_commit
    );
}

#[test]
fn prune_keeps_stale_metadata_when_a_configured_bench_folder_is_also_missing() {
    let (temporary, repository) = prune_fixture();
    let bench = repository
        .parent()
        .expect("repository parent")
        .join("repository-01");
    git(&repository, &["branch", "main-01", "main"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "feature/bench",
            bench.to_str().expect("path is UTF-8"),
            "main",
        ],
    );
    let gone = add_scratch(&repository, "gone", "scratch/gone");
    fs::remove_dir_all(&gone).expect("remove scratch folder");
    fs::remove_dir_all(&bench).expect("remove bench folder");
    let config = write_bench_config(&temporary, &repository, &bench, "main-01");
    let fake_bin = fake_gh(&temporary);
    let worktrees_before = registered_worktrees(&repository);

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    assert!(
        stdout(&output).contains("▎ blocked  scratch-gone scratch/gone folder missing; stale metadata kept because configured bench "),
        "{}",
        stdout(&output)
    );
    assert_eq!(registered_worktrees(&repository), worktrees_before);
}

#[test]
fn prune_dry_run_classifies_without_changing_anything() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "done", "scratch/done");
    let gone = add_scratch(&repository, "gone", "scratch/gone");
    fs::remove_dir_all(&gone).expect("remove worktree folder");
    let open = add_scratch(&repository, "open", "scratch/open");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    let worktrees_before = registered_worktrees(&repository);
    let refs_before = git(&repository, &["show-ref"]);

    let output = prune(&config, &fake_bin, &["--dry-run", "--color", "always"]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains(
            "\x1b[36m▎\x1b[0m prunable scratch-done scratch/done merged pull request #42; would remove worktree and branch\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "\x1b[36m▎\x1b[0m stale    scratch-gone scratch/gone folder missing; would prune stale worktree metadata\n"
        ),
        "{stdout}"
    );
    assert!(stdout.contains("\x1b[2;90m▎\x1b[0m skipped  scratch-open scratch/open"));
    assert!(
        stdout.contains(
            "\nChecked 3 scratch worktrees\n1 prunable, 1 stale, 0 blocked, 1 skipped, 0 failed\nDry run: no changes were made\n"
        ),
        "{stdout}"
    );
    assert_eq!(stdout.matches('\x1b').count(), 6);
    assert!(scratch.exists());
    assert!(open.exists());
    assert_eq!(registered_worktrees(&repository), worktrees_before);
    assert_eq!(git(&repository, &["show-ref"]), refs_before);
}

#[test]
fn prune_rechecks_each_worktree_immediately_before_removing_it() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "raced", "scratch/raced");
    let counter = temporary.path().join("gh-count");
    let config = write_repository_config(&temporary, &repository);
    let bin = temporary.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    fake_lsof(&bin, "exit 0");
    write_executable(
        &bin.join("gh"),
        &format!(
            r#"#!/bin/sh
count_file='{counter}'
count=$(cat "$count_file" 2>/dev/null || printf 0)
printf '%s' "$((count + 1))" > "$count_file"
oid=$(git rev-parse refs/heads/scratch/raced)
pull_requests() {{
if [ "$count" -eq 0 ]; then
  printf '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"scratch/raced","headRefOid":"%s"}}]\n' "$oid"
else
  printf '[{{"number":42,"state":"OPEN","mergedAt":null,"headRefName":"scratch/raced","headRefOid":"%s"}}]\n' "$oid"
fi
}}
{GRAPHQL_DISPATCH}"#,
            counter = counter.display(),
        ),
    );

    let output = prune(&config, &bin, &[]);

    assert_success(&output);
    assert!(
        stdout(&output)
            .contains("▎ skipped  scratch-raced scratch/raced pull request #42 is open\n"),
        "{}",
        stdout(&output)
    );
    assert_eq!(fs::read_to_string(&counter).expect("gh count"), "2");
    assert!(scratch.exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/raced"));
}

#[test]
fn prune_dry_run_looks_up_every_branch_in_one_github_call() {
    let (temporary, repository) = prune_fixture();
    add_scratch(&repository, "done", "scratch/done");
    add_scratch(&repository, "open", "scratch/open");
    add_scratch(&repository, "closed", "scratch/closed");
    let dirty = add_scratch(&repository, "dirty", "scratch/dirty");
    fs::write(dirty.join("new.txt"), "untracked\n").expect("untracked file");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fs::rename(fake_bin.join("gh"), fake_bin.join("gh-answers")).expect("move fake gh");
    let counter = temporary.path().join("gh-count");
    write_executable(
        &fake_bin.join("gh"),
        &format!(
            "#!/bin/sh\nprintf x >> '{}'\nexec '{}' \"$@\"\n",
            counter.display(),
            fake_bin.join("gh-answers").display()
        ),
    );

    let output = prune(&config, &fake_bin, &["--dry-run"]);

    assert_success(&output);
    let stdout = stdout(&output);
    for row in [
        "▎ prunable scratch-done scratch/done merged pull request #42; would remove worktree and branch\n",
        "▎ skipped  scratch-open scratch/open pull request #7 is open\n",
        "▎ skipped  scratch-closed scratch/closed pull request #8 was closed without merging\n",
        "▎ blocked  scratch-dirty scratch/dirty dirty worktree (1 file)\n",
    ] {
        assert!(stdout.contains(row), "missing {row:?} in:\n{stdout}");
    }
    assert_eq!(fs::read_to_string(&counter).expect("gh count"), "x");
}

#[test]
fn prune_looks_up_an_observed_branch_name_that_the_worktree_list_did_not_report() {
    let (temporary, repository) = prune_fixture();
    add_scratch(&repository, "done", "scratch/done");
    add_scratch(&repository, "tagged", "scratch/tagged");
    git(&repository, &["tag", "scratch/tagged", "scratch/tagged"]);
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fs::rename(fake_bin.join("gh"), fake_bin.join("gh-answers")).expect("move fake gh");
    let calls = temporary.path().join("gh-calls");
    write_executable(
        &fake_bin.join("gh"),
        &format!(
            "#!/bin/sh\nfor argument in \"$@\"; do case \"$argument\" in b[0-9]*=*) printf '%s ' \"$argument\" >> '{calls}' ;; esac; done\necho >> '{calls}'\nexec '{}' \"$@\"\n",
            fake_bin.join("gh-answers").display(),
            calls = calls.display(),
        ),
    );

    let output = prune(&config, &fake_bin, &["--dry-run"]);

    assert_success(&output);
    assert!(
        stdout(&output).contains(
            "▎ prunable scratch-done scratch/done merged pull request #42; would remove worktree and branch\n"
        ),
        "{}",
        stdout(&output)
    );
    assert_eq!(
        fs::read_to_string(&calls).expect("gh calls"),
        "b0=scratch/done b1=scratch/tagged \nb0=heads/scratch/tagged \n"
    );
}

#[test]
fn prune_fails_only_the_worktrees_whose_batched_pull_request_lookup_failed() {
    let (temporary, repository) = prune_fixture();
    let first = add_scratch(&repository, "first", "scratch/first");
    let second = add_scratch(&repository, "second", "scratch/second");
    let dirty = add_scratch(&repository, "dirty", "scratch/dirty");
    fs::write(dirty.join("new.txt"), "untracked\n").expect("untracked file");
    let config = write_repository_config(&temporary, &repository);
    let bin = temporary.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    fake_lsof(&bin, "exit 0");
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\necho 'GraphQL: API rate limit exceeded' >&2\nexit 1\n",
    );

    let output = prune(&config, &bin, &[]);

    assert!(!output.status.success(), "{}", stdout(&output));
    let stdout = stdout(&output);
    for row in [
        "▎ failed   scratch-first scratch/first pull-request lookup failed\n",
        "▎ failed   scratch-second scratch/second pull-request lookup failed\n",
        "▎ blocked  scratch-dirty scratch/dirty dirty worktree (1 file)\n",
    ] {
        assert!(stdout.contains(row), "missing {row:?} in:\n{stdout}");
    }
    assert_eq!(
        stdout.matches("API rate limit exceeded").count(),
        2,
        "{stdout}"
    );
    assert!(first.exists() && second.exists() && dirty.exists());
}

#[test]
fn prune_reports_a_failed_removal_and_exits_nonzero() {
    let (temporary, repository) = prune_fixture();
    let failing = add_scratch(&repository, "failing", "scratch/failing");
    let done = add_scratch(&repository, "done", "scratch/done");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_git(
        &fake_bin,
        r#"if [ "$1" = "worktree" ] && [ "$2" = "remove" ]; then
  case "$3" in
    *scratch-failing) printf 'simulated removal failure\n' >&2; exit 1 ;;
  esac
fi"#,
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_eq!(output.status.code(), Some(1));
    let stdout = stdout(&output);
    assert!(
        stdout.contains("▎ failed   scratch-failing scratch/failing worktree removal failed\n"),
        "{stdout}"
    );
    assert!(stdout.contains("simulated removal failure"), "{stdout}");
    assert!(
        stdout.contains("▎ pruned   scratch-done scratch/done"),
        "{stdout}"
    );
    assert!(stdout.contains("1 pruned, 0 cleaned, 0 kept, 0 blocked, 0 skipped, 1 failed\n"));
    assert!(failing.exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/failing"));
    assert!(!done.exists());
}

#[test]
fn prune_skips_the_worktree_that_contains_the_current_directory() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "here", "scratch/here");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);

    let output = prune_command(&config, &fake_bin, &[])
        .current_dir(&scratch)
        .output()
        .expect("bu should start");

    assert_success(&output);
    assert!(
        stdout(&output).contains(
            "▎ blocked  scratch-here scratch/here current directory is inside this worktree\n"
        ),
        "{}",
        stdout(&output)
    );
    assert!(scratch.exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/here"));
}

#[test]
fn prune_fails_a_registered_folder_that_git_resolves_to_another_worktree() {
    let (temporary, repository) = prune_fixture();
    let nested = repository.join("nested-scratch");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "scratch/nested",
            nested.to_str().expect("path is UTF-8"),
            "main",
        ],
    );
    fs::remove_file(nested.join(".git")).expect("break the worktree link");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);

    let output = prune(&config, &fake_bin, &[]);

    assert_eq!(output.status.code(), Some(1));
    let stdout = stdout(&output);
    assert!(
        stdout.contains("▎ failed   nested-scratch scratch/nested worktree root check failed\n"),
        "{stdout}"
    );
    assert!(nested.join("README.md").exists());
    assert!(repository.join("README.md").exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/nested"));
    assert!(ref_exists(&repository, "refs/heads/home"));
}

fn branch_config(repository: &Path, branch: &str) -> String {
    let output = Command::new("git")
        .args([
            "config",
            "--get-regexp",
            &format!("^branch\\.{}\\.", branch.replace('.', "\\.")),
        ])
        .current_dir(repository)
        .output()
        .expect("git config should start");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn set_upstream_config(repository: &Path, branch: &str) {
    git(
        repository,
        &["config", &format!("branch.{branch}.remote"), "origin"],
    );
    git(
        repository,
        &[
            "config",
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{branch}"),
        ],
    );
}

#[test]
fn prune_removes_the_config_section_of_a_deleted_branch() {
    let (temporary, repository) = prune_fixture();
    add_scratch(&repository, "tracked", "scratch/tracked.v1");
    set_upstream_config(&repository, "scratch/tracked.v1");
    add_scratch(&repository, "untracked", "scratch/untracked");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains("2 pruned, 0 cleaned, 0 kept, 0 blocked, 0 skipped, 0 failed\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("note:"), "{stdout}");
    assert!(!ref_exists(&repository, "refs/heads/scratch/tracked.v1"));
    assert_eq!(branch_config(&repository, "scratch/tracked.v1"), "");
    assert!(
        !git(&repository, &["config", "--list", "--local"]).contains("branch.scratch/tracked.v1"),
        "the empty section header should be removed too"
    );
}

#[test]
fn prune_keeps_the_config_section_of_a_branch_that_moved() {
    let (temporary, repository) = prune_fixture();
    add_scratch(&repository, "moved", "scratch/moved");
    set_upstream_config(&repository, "scratch/moved");
    let moved_to = git(&repository, &["rev-parse", "main"]);
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_git(
        &fake_bin,
        &format!(
            r#"if [ "$1" = "worktree" ] && [ "$2" = "remove" ]; then
  "{real}" "$@" || exit $?
  exec "{real}" update-ref refs/heads/scratch/moved '{moved_to}'
fi"#,
            real = real_git(),
            moved_to = moved_to.trim(),
        ),
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    assert!(stdout(&output).contains("▎ kept     scratch-moved scratch/moved"));
    assert_eq!(
        branch_config(&repository, "scratch/moved"),
        "branch.scratch/moved.remote origin\nbranch.scratch/moved.merge refs/heads/scratch/moved\n"
    );
}

#[test]
fn prune_notes_a_config_cleanup_failure_without_failing_the_run() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "done", "scratch/done");
    set_upstream_config(&repository, "scratch/done");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_git(
        &fake_bin,
        r#"if [ "$1" = "config" ] && [ "$2" = "--remove-section" ]; then
  printf 'error: could not lock config file\n' >&2
  exit 255
fi"#,
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains(
            "▎ pruned   scratch-done scratch/done merged pull request #42; removed worktree and branch\n    note: could not remove config section branch.scratch/done: "
        ),
        "{stdout}"
    );
    assert!(stdout.contains("could not lock config file"), "{stdout}");
    assert!(stdout.contains("1 pruned, 0 cleaned, 0 kept, 0 blocked, 0 skipped, 0 failed\n"));
    assert!(!scratch.exists());
    assert!(!ref_exists(&repository, "refs/heads/scratch/done"));
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).expect("canonical path")
}

#[test]
fn prune_blocks_worktrees_that_a_live_process_uses_in_dry_run_and_real_run() {
    let (temporary, repository) = prune_fixture();
    let busy = add_scratch(&repository, "busy", "scratch/busy");
    let nested = add_scratch(&repository, "nested-cwd", "scratch/nested-cwd");
    fs::create_dir_all(nested.join("docs/site")).expect("empty subdirectory");
    let own = add_scratch(&repository, "own", "scratch/own");
    let free = add_scratch(&repository, "free", "scratch/free");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    // The subdirectory record uses the uncanonicalized temp path (for example
    // `/var` instead of `/private/var` on macOS); bu must still match it.
    let records = [
        lsof_record("4242", "node", &canonical(&busy)),
        lsof_record("4243", "esbuild", &canonical(&busy).join("docs")),
        lsof_record("4244", "node", &canonical(&busy)),
        lsof_record("5001", "node", &nested.join("docs/site")),
    ]
    .concat();
    fake_lsof(
        &fake_bin,
        &format!(
            "printf 'p%s\\ncbu\\nfcwd\\nn%s\\n' \"$PPID\" '{own}'\n{body}",
            own = canonical(&own).display(),
            body = lsof_printing(&records, 0),
        ),
    );
    let worktrees_before = registered_worktrees(&repository);
    let refs_before = git(&repository, &["show-ref"]);

    let dry_run = prune(&config, &fake_bin, &["--dry-run"]);

    assert_success(&dry_run);
    let dry_stdout = stdout(&dry_run);
    for expected in [
        "▎ blocked  scratch-busy scratch/busy in use by 3 processes (esbuild, node)\n",
        "▎ blocked  scratch-nested-cwd scratch/nested-cwd in use by 1 process (node)\n",
        "▎ prunable scratch-own scratch/own merged pull request #42; would remove worktree and branch\n",
        "▎ prunable scratch-free scratch/free merged pull request #42; would remove worktree and branch\n",
        "\nChecked 4 scratch worktrees\n2 prunable, 0 stale, 2 blocked, 0 skipped, 0 failed\n",
    ] {
        assert!(
            dry_stdout.contains(expected),
            "missing {expected:?} in\n{dry_stdout}"
        );
    }
    assert_eq!(registered_worktrees(&repository), worktrees_before);
    assert_eq!(git(&repository, &["show-ref"]), refs_before);

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    for expected in [
        "▎ blocked  scratch-busy scratch/busy in use by 3 processes (esbuild, node)\n",
        "▎ blocked  scratch-nested-cwd scratch/nested-cwd in use by 1 process (node)\n",
        "▎ pruned   scratch-own scratch/own",
        "▎ pruned   scratch-free scratch/free",
        "2 pruned, 0 cleaned, 0 kept, 2 blocked, 0 skipped, 0 failed\n",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in\n{stdout}"
        );
    }
    assert!(busy.exists());
    assert!(nested.exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/busy"));
    assert!(ref_exists(&repository, "refs/heads/scratch/nested-cwd"));
    assert!(!own.exists());
    assert!(!free.exists());
}

#[test]
fn prune_matches_process_directories_on_path_component_boundaries() {
    let (temporary, repository) = prune_fixture();
    let short = add_scratch(&repository, "wt", "scratch/wt");
    let longer = add_scratch(&repository, "wt-2", "scratch/wt-2");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_lsof(
        &fake_bin,
        &lsof_printing(&lsof_record("4242", "node", &canonical(&longer)), 0),
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains("▎ pruned   scratch-wt scratch/wt merged pull request #42"),
        "{stdout}"
    );
    assert!(
        stdout.contains("▎ blocked  scratch-wt-2 scratch/wt-2 in use by 1 process (node)\n"),
        "{stdout}"
    );
    assert!(!short.exists());
    assert!(longer.exists());
}

#[test]
fn prune_accepts_lsof_output_when_lsof_exits_one_after_printing_records() {
    let (temporary, repository) = prune_fixture();
    let busy = add_scratch(&repository, "busy", "scratch/busy");
    let free = add_scratch(&repository, "free", "scratch/free");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_lsof(
        &fake_bin,
        &format!(
            "printf 'lsof: WARNING: can not inspect some processes\\n' >&2\n{}",
            lsof_printing(&lsof_record("4242", "node", &canonical(&busy)), 1)
        ),
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    let stdout = stdout(&output);
    assert!(
        stdout.contains("▎ blocked  scratch-busy scratch/busy in use by 1 process (node)\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("▎ pruned   scratch-free scratch/free"),
        "{stdout}"
    );
    assert!(busy.exists());
    assert!(!free.exists());
}

#[test]
fn prune_changes_nothing_and_exits_nonzero_when_the_process_check_fails() {
    for (case, body) in [
        (
            "exit one without output",
            "printf 'lsof: simulated failure\\n' >&2\nexit 1",
        ),
        ("other nonzero exit", "printf 'p1\\ncinit\\n'\nexit 2"),
    ] {
        let (temporary, repository) = prune_fixture();
        let done = add_scratch(&repository, "done", "scratch/done");
        let gone = add_scratch(&repository, "gone", "scratch/gone");
        fs::remove_dir_all(&gone).expect("remove worktree folder");
        let config = write_repository_config(&temporary, &repository);
        let fake_bin = fake_gh(&temporary);
        fake_lsof(&fake_bin, body);
        let worktrees_before = registered_worktrees(&repository);
        let refs_before = git(&repository, &["show-ref"]);

        let output = prune(&config, &fake_bin, &[]);

        assert_eq!(output.status.code(), Some(1), "{case}");
        let stdout = stdout(&output);
        assert!(
            stdout.contains("▎ failed   repository unknown process check failed\n    error: "),
            "{case}: {stdout}"
        );
        assert!(stdout.contains("lsof"), "{case}: {stdout}");
        assert!(done.exists(), "{case}");
        assert_eq!(
            registered_worktrees(&repository),
            worktrees_before,
            "{case}"
        );
        assert_eq!(git(&repository, &["show-ref"]), refs_before, "{case}");
    }
}

#[test]
fn prune_rechecks_live_processes_immediately_before_removing_a_worktree() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "raced", "scratch/raced");
    let counter = temporary.path().join("lsof-count");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_lsof(
        &fake_bin,
        &format!(
            r#"count_file='{counter}'
count=$(cat "$count_file" 2>/dev/null || printf 0)
printf '%s' "$((count + 1))" > "$count_file"
if [ "$count" -eq 0 ]; then
  printf 'p1\ncinit\nfcwd\nn/\n'
  exit 0
fi
{late}"#,
            counter = counter.display(),
            late = lsof_printing(&lsof_record("4242", "node", &canonical(&scratch)), 0),
        ),
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_success(&output);
    assert!(
        stdout(&output)
            .contains("▎ blocked  scratch-raced scratch/raced in use by 1 process (node)\n"),
        "{}",
        stdout(&output)
    );
    assert_eq!(fs::read_to_string(&counter).expect("lsof count"), "2");
    assert!(scratch.exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/raced"));
}

#[test]
fn prune_fails_the_worktree_when_the_pre_removal_process_check_fails() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "raced", "scratch/raced");
    let counter = temporary.path().join("lsof-count");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fake_lsof(
        &fake_bin,
        &format!(
            r#"count_file='{counter}'
count=$(cat "$count_file" 2>/dev/null || printf 0)
printf '%s' "$((count + 1))" > "$count_file"
if [ "$count" -eq 0 ]; then
  printf 'p1\ncinit\nfcwd\nn/\n'
  exit 0
fi
printf 'lsof: simulated recheck failure\n' >&2
exit 1"#,
            counter = counter.display(),
        ),
    );

    let output = prune(&config, &fake_bin, &[]);

    assert_eq!(output.status.code(), Some(1));
    let stdout = stdout(&output);
    assert!(
        stdout.contains("▎ failed   scratch-raced scratch/raced pre-removal recheck failed\n"),
        "{stdout}"
    );
    assert!(stdout.contains("process check failed: "), "{stdout}");
    assert!(stdout.contains("simulated recheck failure"), "{stdout}");
    assert!(scratch.exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/raced"));
}

/// Uses the host's real `lsof` against a real child process, so the parser and
/// path canonicalization are checked against genuine output.
#[test]
fn prune_blocks_a_worktree_that_a_real_process_uses_as_its_current_directory() {
    let (temporary, repository) = prune_fixture();
    let scratch = add_scratch(&repository, "sleeping", "scratch/sleeping");
    let config = write_repository_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary);
    fs::remove_file(fake_bin.join("lsof")).expect("use the real lsof");
    let mut sleeper = Command::new("sleep")
        .arg("60")
        .current_dir(&scratch)
        .spawn()
        .expect("sleep should start");

    let output = prune(&config, &fake_bin, &[]);

    sleeper.kill().expect("stop sleep");
    sleeper.wait().expect("reap sleep");
    assert_success(&output);
    assert!(
        stdout(&output)
            .contains("▎ blocked  scratch-sleeping scratch/sleeping in use by 1 process (sleep)\n"),
        "{}",
        stdout(&output)
    );
    assert!(scratch.exists());
    assert!(ref_exists(&repository, "refs/heads/scratch/sleeping"));
}
