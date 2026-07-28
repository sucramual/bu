use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

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

fn fixture_repository() -> (TempDir, std::path::PathBuf) {
    let temporary = TempDir::new().expect("temporary directory");
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).expect("repository directory");

    git(&repository, &["init", "--initial-branch=main"]);
    git(&repository, &["config", "user.name", "bu test"]);
    git(&repository, &["config", "user.email", "bu@example.test"]);
    fs::write(repository.join("README.md"), "fixture\n").expect("fixture file");
    git(&repository, &["add", "README.md"]);
    git(&repository, &["commit", "-m", "initial fixture"]);
    git(&repository, &["branch", "main-01"]);
    git(&repository, &["switch", "-c", "feature/merged"]);
    git(
        &repository,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );

    (temporary, repository)
}

fn recycle_fixture_repository() -> (TempDir, std::path::PathBuf) {
    let (temporary, repository) = fixture_repository();
    let remote = temporary.path().join("origin.git");
    run(
        "git",
        &[
            "init",
            "--bare",
            remote.to_str().expect("remote path is UTF-8"),
        ],
        temporary.path(),
    );
    git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path is UTF-8"),
        ],
    );
    git(&repository, &["switch", "main"]);
    git(&repository, &["push", "--set-upstream", "origin", "main"]);
    fs::write(repository.join("UPSTREAM.md"), "new upstream commit\n").expect("upstream fixture");
    git(&repository, &["add", "UPSTREAM.md"]);
    git(&repository, &["commit", "-m", "advance upstream"]);
    git(&repository, &["push", "origin", "main"]);
    git(&repository, &["switch", "feature/merged"]);

    (temporary, repository)
}

fn write_config(temporary: &TempDir, repository: &Path) -> std::path::PathBuf {
    let config = temporary.path().join("bu.toml");
    fs::write(
        &config,
        format!(
            "[repository]\npath = \"{}\"\nremote = \"origin\"\nmain_branch = \"main\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"main-01\"\n",
            repository.display(),
            repository.display(),
        ),
    )
    .expect("config file");
    config
}

fn fake_gh(temporary: &TempDir, body: &str) -> std::path::PathBuf {
    let bin = temporary.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    let executable = bin.join("gh");
    fs::write(&executable, format!("#!/bin/sh\n{body}\n")).expect("fake gh executable");
    let mut permissions = fs::metadata(&executable)
        .expect("fake gh metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).expect("make fake gh executable");
    bin
}

fn merged_pull_request_body(branch: &str, head_commit: &str) -> String {
    format!(
        "printf '%s\\n' '[{{\"number\":42,\"state\":\"MERGED\",\"mergedAt\":\"2026-07-28T00:00:00Z\",\"headRefName\":\"{branch}\",\"headRefOid\":\"{head_commit}\"}}]'"
    )
}

fn status(config: &Path, fake_bin: &Path) -> Output {
    status_with(config, fake_bin, &[], &[])
}

fn status_with(
    config: &Path,
    fake_bin: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Output {
    let original_path = env::var_os("PATH").expect("PATH is set");
    let path = env::join_paths(
        std::iter::once(fake_bin.to_path_buf()).chain(env::split_paths(&original_path)),
    )
    .expect("valid PATH");
    Command::new(env!("CARGO_BIN_EXE_bu"))
        .args(["--config"])
        .arg(config)
        .arg("status")
        .args(arguments)
        .env("PATH", path)
        .envs(environment.iter().copied())
        .output()
        .expect("bu should start")
}

fn recycle(config: &Path, fake_bin: &Path) -> Output {
    recycle_with(config, fake_bin, &[], &[])
}

fn recycle_with(
    config: &Path,
    fake_bin: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Output {
    let original_path = env::var_os("PATH").expect("PATH is set");
    let path = env::join_paths(
        std::iter::once(fake_bin.to_path_buf()).chain(env::split_paths(&original_path)),
    )
    .expect("valid PATH");
    Command::new(env!("CARGO_BIN_EXE_bu"))
        .args(["--config"])
        .arg(config)
        .arg("recycle")
        .args(arguments)
        .env("PATH", path)
        .envs(environment.iter().copied())
        .output()
        .expect("bu should start")
}

#[test]
fn status_reports_an_eligible_bench_without_changing_git_state() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    let before = git(&repository, &["status", "--porcelain=v1", "--branch"]);
    let before_refs = git(&repository, &["show-ref", "--head"]);

    let output = status(&config, &fake_bin);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ eligible repository feature/merged merged pull request #42\n"));
    assert!(stdout.contains("\nChecked 1 bench\n1 bench eligible for `bu recycle`\n"));
    assert!(!stdout.contains('\x1b'));
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(git(&repository, &["show-ref", "--head"]), before_refs);
}

#[test]
fn status_color_policy_styles_only_the_marker_and_honors_overrides() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);

    let always = status_with(
        &config,
        &fake_bin,
        &["--color", "always"],
        &[("NO_COLOR", "1")],
    );
    assert!(always.status.success());
    let always_stdout = String::from_utf8_lossy(&always.stdout);
    assert!(always_stdout.contains("\x1b[36m▎\x1b[0m eligible repository"));
    assert_eq!(always_stdout.matches('\x1b').count(), 2);

    let never = status_with(
        &config,
        &fake_bin,
        &["--color", "never"],
        &[("NO_COLOR", "1")],
    );
    assert!(never.status.success());
    assert!(!String::from_utf8_lossy(&never.stdout).contains('\x1b'));

    let no_color = status_with(
        &config,
        &fake_bin,
        &["--color", "auto"],
        &[("NO_COLOR", "1")],
    );
    assert!(no_color.status.success());
    assert!(!String::from_utf8_lossy(&no_color.stdout).contains('\x1b'));
}

#[test]
fn status_skips_a_dirty_bench_without_querying_github() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary, "exit 99");
    fs::write(repository.join("README.md"), "dirty fixture\n").expect("dirty fixture");
    let before = git(&repository, &["status", "--porcelain=v1", "--branch"]);
    let before_refs = git(&repository, &["show-ref", "--head"]);

    let output = status(&config, &fake_bin);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ blocked  repository feature/merged dirty worktree (1 file)\n"));
    assert!(!stdout.contains(" M README.md\n"));
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(git(&repository, &["show-ref", "--head"]), before_refs);
}

#[test]
fn recycle_fast_forwards_the_standin_and_preserves_the_feature_ref() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    let feature_before = git(&repository, &["rev-parse", "feature/merged"]);

    let output = recycle(&config, &fake_bin);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ recycled repository main-01 feature/merged preserved; main-01 -> "));
    assert!(stdout.contains("\n1 recycled, 0 blocked, 0 skipped, 0 failed\n"));
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "main-01"
    );
    assert_eq!(
        git(&repository, &["rev-parse", "feature/merged"]),
        feature_before
    );
    assert_eq!(
        git(&repository, &["rev-parse", "main-01"]),
        git(&repository, &["rev-parse", "origin/main"])
    );
    assert!(git(&repository, &["status", "--porcelain=v1"]).is_empty());

    let second = recycle(&config, &fake_bin);
    assert!(second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stdout)
            .contains("0 recycled, 0 blocked, 1 skipped, 0 failed")
    );
}

#[test]
fn recycle_color_policy_styles_only_the_marker_and_honors_overrides() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);

    let always = recycle_with(
        &config,
        &fake_bin,
        &["--color", "always"],
        &[("NO_COLOR", "1")],
    );
    assert!(always.status.success());
    let always_stdout = String::from_utf8_lossy(&always.stdout);
    assert!(always_stdout.contains("\x1b[32m▎\x1b[0m recycled repository"));
    assert_eq!(always_stdout.matches('\x1b').count(), 2);

    let never = recycle_with(
        &config,
        &fake_bin,
        &["--color", "never"],
        &[("NO_COLOR", "1")],
    );
    assert!(never.status.success());
    assert!(!String::from_utf8_lossy(&never.stdout).contains('\x1b'));
}

#[test]
fn recycle_fetch_failure_leaves_the_bench_unchanged() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = temporary.path().join("fetch-failure.toml");
    fs::write(
        &config,
        format!(
            "[repository]\npath = \"{}\"\nremote = \"missing\"\nmain_branch = \"main\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"main-01\"\n",
            repository.display(),
            repository.display(),
        ),
    )
    .expect("config file");
    let fake_bin = fake_gh(&temporary, "exit 99");
    let before = git(&repository, &["status", "--porcelain=v1", "--branch"]);
    let before_refs = git(&repository, &["show-ref", "--head"]);

    let output = recycle(&config, &fake_bin);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("▎ failed   repository unknown upstream fetch failed")
    );
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(git(&repository, &["show-ref", "--head"]), before_refs);
}

#[test]
fn recycle_continues_after_one_bench_fails() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = temporary.path().join("failure-isolation.toml");
    let missing = temporary.path().join("missing-bench");
    fs::write(
        &config,
        format!(
            "[repository]\npath = \"{}\"\nremote = \"origin\"\nmain_branch = \"main\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"main-01\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"main-01\"\n",
            repository.display(),
            missing.display(),
            repository.display(),
        ),
    )
    .expect("config file");
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);

    let output = recycle(&config, &fake_bin);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ failed   missing-bench unknown repository check failed"));
    assert!(stdout.contains("▎ recycled repository main-01 feature/merged preserved"));
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "main-01"
    );
}

#[test]
fn status_skips_a_standin_checked_out_in_another_worktree() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let other_worktree = temporary.path().join("standin-worktree");
    git(
        &repository,
        &[
            "worktree",
            "add",
            other_worktree.to_str().expect("worktree path is UTF-8"),
            "main-01",
        ],
    );
    let fake_bin = fake_gh(&temporary, "exit 99");
    let before = git(&repository, &["status", "--porcelain=v1", "--branch"]);
    let before_refs = git(&repository, &["show-ref", "--head"]);

    let output = status(&config, &fake_bin);

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("▎ blocked  repository feature/merged stand-in branch is checked out")
    );
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(git(&repository, &["show-ref", "--head"]), before_refs);
}

#[test]
fn recycle_skips_a_bench_already_on_its_standin_branch() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    git(&repository, &["switch", "main-01"]);
    let pull_request =
        merged_pull_request_body("main-01", git(&repository, &["rev-parse", "HEAD"]).trim());
    let fake_bin = fake_gh(&temporary, &pull_request);
    let before_refs = git(&repository, &["show-ref", "--head"]);

    let output = recycle(&config, &fake_bin);

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("▎ skipped  repository main-01 already on the stand-in branch")
    );
    assert_eq!(git(&repository, &["show-ref", "--head"]), before_refs);
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "main-01"
    );
    assert!(git(&repository, &["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn status_skips_a_local_tip_that_does_not_match_the_merged_pull_request() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let merged_head = git(&repository, &["rev-parse", "HEAD"]);
    fs::write(repository.join("LOCAL.md"), "local follow-up\n").expect("local follow-up");
    git(&repository, &["add", "LOCAL.md"]);
    git(&repository, &["commit", "-m", "local follow-up"]);
    let pull_request = merged_pull_request_body("feature/merged", merged_head.trim());
    let fake_bin = fake_gh(&temporary, &pull_request);
    let before = git(&repository, &["status", "--porcelain=v1", "--branch"]);
    let before_refs = git(&repository, &["show-ref", "--head"]);

    let output = status(&config, &fake_bin);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "▎ idle     repository feature/merged current branch does not have exactly one merged pull request"
    ));
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(git(&repository, &["show-ref", "--head"]), before_refs);
}

#[test]
fn status_reports_every_structured_dirty_file() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary, "exit 99");
    fs::write(repository.join("SOURCE.md"), "source\n").expect("rename source");
    git(&repository, &["add", "SOURCE.md"]);
    git(&repository, &["commit", "-m", "add rename source"]);
    fs::write(repository.join("STAGED.md"), "staged\n").expect("staged file");
    git(&repository, &["add", "STAGED.md"]);
    fs::write(repository.join("README.md"), "unstaged\n").expect("unstaged file");
    fs::write(repository.join("UNTRACKED.md"), "untracked\n").expect("untracked file");
    git(&repository, &["mv", "SOURCE.md", "RENAMED.md"]);

    let output = status_with(&config, &fake_bin, &["--verbose"], &[]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("    path: {}\n", repository.display())));
    assert!(stdout.contains("    A  STAGED.md\n"));
    assert!(stdout.contains("     M README.md\n"));
    assert!(stdout.contains("    ?? UNTRACKED.md\n"));
    assert!(stdout.contains("    R  SOURCE.md -> RENAMED.md\n"));
}

#[test]
fn status_escapes_control_characters_in_dirty_file_paths() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary, "exit 99");
    let source = "source\nname\u{1b}[31m";
    fs::write(repository.join(source), "source\n").expect("rename source");
    git(&repository, &["add", source]);
    git(
        &repository,
        &["commit", "-m", "add control-character rename source"],
    );
    git(&repository, &["mv", source, "RENAMED.md"]);
    fs::write(repository.join("line\nname\u{1b}[31m"), "dirty\n").expect("dirty file");

    let output = status_with(&config, &fake_bin, &["-v"], &[]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("    ?? line\\nname\\u{1b}[31m\n"));
    assert!(stdout.contains("    R  source\\nname\\u{1b}[31m -> RENAMED.md\n"));
    assert!(!stdout.contains("line\nname\u{1b}[31m"));
    assert!(!stdout.contains(source));
}

#[test]
fn status_reports_a_detached_head() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary, "exit 99");
    git(&repository, &["checkout", "--detach"]);

    let output = status(&config, &fake_bin);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ blocked  repository detached HEAD is detached\n"));
}

#[test]
fn status_retains_a_discovered_branch_when_later_inspection_fails() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let fake_bin = fake_gh(&temporary, "exit 99");

    let output = status_with(&config, &fake_bin, &["--verbose"], &[]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ failed   repository feature/merged pull-request lookup failed\n"));
    assert!(stdout.contains(&format!("    path: {}\n", repository.display())));
    assert!(stdout.contains("    error: gh pr list"));
    assert!(stdout.contains("gh pr list"));
}

#[test]
fn status_reports_unknown_when_branch_discovery_does_not_complete() {
    let (temporary, repository) = fixture_repository();
    let missing = temporary.path().join("missing-bench");
    let config = temporary.path().join("missing-bench.toml");
    fs::write(
        &config,
        format!(
            "[repository]\npath = \"{}\"\nremote = \"origin\"\nmain_branch = \"main\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"main-01\"\n",
            repository.display(),
            missing.display(),
        ),
    )
    .expect("config file");
    let fake_bin = fake_gh(&temporary, "exit 99");

    let output = status(&config, &fake_bin);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("▎ failed   missing-bench unknown repository check failed\n")
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("git rev-parse"));
}
