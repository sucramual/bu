use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
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
    fs::write(repository.join(".gitignore"), "ignored.log\n").expect("ignore fixture");
    git(&repository, &["add", "README.md", ".gitignore"]);
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

fn fake_git(bin: &Path, body: &str) {
    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("find real git")
            .stdout,
    )
    .expect("Git path is UTF-8");
    let executable = bin.join("git");
    fs::write(
        &executable,
        format!("#!/bin/sh\n{body}\nexec \"{}\" \"$@\"\n", real_git.trim()),
    )
    .expect("fake git executable");
    let mut permissions = fs::metadata(&executable)
        .expect("fake git metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).expect("make fake git executable");
}

fn merged_pull_request_body(branch: &str, head_commit: &str) -> String {
    format!(
        "printf '%s\\n' '[{{\"number\":42,\"state\":\"MERGED\",\"mergedAt\":\"2026-07-28T00:00:00Z\",\"headRefName\":\"{branch}\",\"headRefOid\":\"{head_commit}\"}}]'"
    )
}

fn matching_merged_pull_request_body() -> &'static str {
    r#"head=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--head" ]; then
    shift
    head="$1"
  fi
  shift
done
oid=$(git rev-parse "$head")
printf '[{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"%s","headRefOid":"%s"}]\n' "$head" "$oid""#
}

fn add_numbered_bench(repository: &Path, slot: &str, feature_branch: &str) -> std::path::PathBuf {
    let repository_name = repository
        .file_name()
        .expect("repository name")
        .to_string_lossy();
    let bench = repository
        .parent()
        .expect("repository parent")
        .join(format!("{repository_name}-{slot}"));
    let standin_branch = format!("main-{slot}");
    if git(repository, &["branch", "--list", &standin_branch])
        .trim()
        .is_empty()
    {
        git(repository, &["branch", &standin_branch]);
    }
    git(
        repository,
        &[
            "worktree",
            "add",
            "-b",
            feature_branch,
            bench.to_str().expect("bench path is UTF-8"),
            "main",
        ],
    );
    bench
}

fn status(config: &Path, fake_bin: &Path) -> Output {
    status_with(config, fake_bin, &[], &[])
}

fn bu_command(fake_bin: &Path) -> Command {
    let original_path = env::var_os("PATH").expect("PATH is set");
    let path = env::join_paths(
        std::iter::once(fake_bin.to_path_buf()).chain(env::split_paths(&original_path)),
    )
    .expect("valid PATH");
    let mut command = Command::new(env!("CARGO_BIN_EXE_bu"));
    command.env("PATH", path);
    command
}

fn status_command(config: Option<&Path>, fake_bin: &Path) -> Command {
    let mut command = bu_command(fake_bin);
    if let Some(config) = config {
        command.args(["--config"]).arg(config);
    }
    command.arg("status");
    command
}

fn status_with(
    config: &Path,
    fake_bin: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Output {
    status_command(Some(config), fake_bin)
        .args(arguments)
        .envs(environment.iter().copied())
        .output()
        .expect("bu should start")
}

fn status_with_default_config(repository: &Path, home: &Path, fake_bin: &Path) -> Output {
    status_command(None, fake_bin)
        .current_dir(repository)
        .env("HOME", home)
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
    bu_command(fake_bin)
        .args(["--config"])
        .arg(config)
        .arg("recycle")
        .args(arguments)
        .envs(environment.iter().copied())
        .output()
        .expect("bu should start")
}

#[test]
fn default_config_uses_primary_checkout_when_main_is_not_checked_out() {
    let (temporary, repository) = fixture_repository();
    let bench = add_numbered_bench(&repository, "01", "feature/one");
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("home directory");
    let fake_bin = fake_gh(&temporary, matching_merged_pull_request_body());

    let output = status_with_default_config(&bench, &home, &fake_bin);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = fs::read_to_string(home.join(".config/bu/config.toml")).expect("generated config");
    let parsed: bu::Config = toml::from_str(&config).expect("generated config parses");
    assert_eq!(
        parsed.repository.path,
        fs::canonicalize(&repository).expect("fixture repository path exists")
    );
    assert_eq!(parsed.repository.remote, "origin");
    assert_eq!(parsed.repository.main_branch, "main");
    assert_eq!(parsed.benches.len(), 1);
    assert_eq!(
        parsed.benches[0].path,
        fs::canonicalize(&bench).expect("fixture bench path exists")
    );
    assert_eq!(parsed.benches[0].standin_branch, "main-01");
}

#[test]
fn default_config_adds_a_bench_after_an_empty_first_run() {
    let (temporary, repository) = fixture_repository();
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("home directory");
    let fake_bin = fake_gh(&temporary, matching_merged_pull_request_body());

    let first = status_with_default_config(&repository, &home, &fake_bin);

    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let config_path = home.join(".config/bu/config.toml");
    let first_config = fs::read_to_string(&config_path).expect("generated config");
    assert!(!first_config.contains("benches = []"));
    let first_parsed: bu::Config = toml::from_str(&first_config).expect("generated config parses");
    assert!(first_parsed.benches.is_empty());

    let bench = add_numbered_bench(&repository, "01", "feature/one");
    let second = status_with_default_config(&bench, &home, &fake_bin);

    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let updated_config = fs::read_to_string(&config_path).expect("updated config");
    let updated: bu::Config = toml::from_str(&updated_config).expect("updated config parses");
    assert_eq!(updated.benches.len(), 1);

    let third = status_with_default_config(&bench, &home, &fake_bin);
    assert!(
        third.status.success(),
        "{}",
        String::from_utf8_lossy(&third.stderr)
    );
}

#[test]
fn default_config_creates_the_target_of_a_dangling_symlink() {
    let (temporary, repository) = fixture_repository();
    let bench = add_numbered_bench(&repository, "01", "feature/one");
    let home = temporary.path().join("home");
    let config_path = home.join(".config/bu/config.toml");
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("config parent");
    let target = home.join("dotfiles/bu/config.toml");
    symlink(&target, &config_path).expect("dangling default config link");
    let fake_bin = fake_gh(&temporary, matching_merged_pull_request_body());

    let output = status_with_default_config(&bench, &home, &fake_bin);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::symlink_metadata(&config_path)
            .expect("default config metadata")
            .file_type()
            .is_symlink()
    );
    let config = fs::read_to_string(&target).expect("symlink target config");
    let parsed: bu::Config = toml::from_str(&config).expect("symlink target config parses");
    assert_eq!(parsed.benches.len(), 1);
}

#[test]
fn default_config_preserves_chained_symlinks_to_a_dangling_target() {
    let (temporary, repository) = fixture_repository();
    let bench = add_numbered_bench(&repository, "01", "feature/one");
    let home = temporary.path().join("home");
    let config_path = home.join(".config/bu/config.toml");
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("config parent");
    let managed_link = home.join("dotfiles/bu/config.toml");
    fs::create_dir_all(managed_link.parent().expect("managed link parent"))
        .expect("managed link parent");
    let target = home.join("generated/bu/config.toml");
    symlink(&managed_link, &config_path).expect("default config link");
    symlink(&target, &managed_link).expect("managed config link");
    let fake_bin = fake_gh(&temporary, matching_merged_pull_request_body());

    let output = status_with_default_config(&bench, &home, &fake_bin);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::symlink_metadata(&config_path)
            .expect("default config metadata")
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(&managed_link)
            .expect("managed config metadata")
            .file_type()
            .is_symlink()
    );
    let config = fs::read_to_string(&target).expect("chained symlink target config");
    let parsed: bu::Config = toml::from_str(&config).expect("chained symlink target config parses");
    assert_eq!(parsed.benches.len(), 1);
}

#[test]
fn default_config_is_created_and_updated_from_numbered_sibling_benches() {
    let (temporary, repository) = fixture_repository();
    let linked_main = repository
        .parent()
        .expect("repository parent")
        .join("linked-main-worktree");
    git(
        &repository,
        &[
            "worktree",
            "add",
            linked_main.to_str().expect("linked main path is UTF-8"),
            "main",
        ],
    );
    let first_bench = add_numbered_bench(&repository, "01", "feature/one");
    let wrong_width_bench =
        add_numbered_bench(&repository, "20260810", "feature/date-stamped-scratch");
    let repository_name = repository
        .file_name()
        .expect("repository name")
        .to_string_lossy();
    let newline_scratch = repository
        .parent()
        .expect("repository parent")
        .join(format!("{repository_name}-03\nscratch"));
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "feature/newline-scratch",
            newline_scratch
                .to_str()
                .expect("newline scratch path is UTF-8"),
            "main",
        ],
    );
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("home directory");
    let fake_bin = fake_gh(&temporary, matching_merged_pull_request_body());

    let first = status_with_default_config(&first_bench, &home, &fake_bin);

    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("Checked 1 bench"));
    let config_path = home.join(".config/bu/config.toml");
    let first_config = fs::read_to_string(&config_path).expect("generated config");
    let parsed: bu::Config = toml::from_str(&first_config).expect("generated config parses");
    assert_eq!(
        parsed.repository.path,
        fs::canonicalize(&repository).expect("fixture repository path exists")
    );
    assert_eq!(parsed.repository.remote, "origin");
    assert_eq!(parsed.repository.main_branch, "main");
    assert_eq!(parsed.benches.len(), 1);
    assert_eq!(
        parsed.benches[0].path,
        fs::canonicalize(&first_bench).expect("fixture bench path exists")
    );
    assert_eq!(parsed.benches[0].standin_branch, "main-01");
    assert!(
        parsed
            .benches
            .iter()
            .all(|bench| bench.path != wrong_width_bench)
    );
    assert!(parsed.benches.iter().all(|bench| {
        !bench
            .path
            .to_string_lossy()
            .contains(&format!("{repository_name}-03"))
    }));

    let second_bench = add_numbered_bench(&repository, "02", "feature/two");
    let dotfiles_config = home.join("dotfiles-config.toml");
    fs::rename(&config_path, &dotfiles_config).expect("move generated config into dotfiles");
    symlink(&dotfiles_config, &config_path).expect("link default config to dotfiles");
    fs::write(
        &config_path,
        format!("# Preserve this comment.\nunknown_top_level = \"keep\"\n\n{first_config}"),
    )
    .expect("add user-authored config content");
    let scratch = repository
        .parent()
        .expect("repository parent")
        .join("scratch-worktree");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "feature/scratch",
            scratch.to_str().expect("scratch path is UTF-8"),
            "main",
        ],
    );

    let second = status_with_default_config(&repository, &home, &fake_bin);

    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("Checked 2 benches"));
    let updated_config = fs::read_to_string(&config_path).expect("updated config");
    assert!(
        fs::symlink_metadata(&config_path)
            .expect("default config metadata")
            .file_type()
            .is_symlink()
    );
    assert!(updated_config.starts_with("# Preserve this comment.\nunknown_top_level = \"keep\"\n"));
    assert!(updated_config.contains(&first_bench.display().to_string()));
    assert!(updated_config.contains(&second_bench.display().to_string()));
    assert!(!updated_config.contains(&scratch.display().to_string()));
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
fn status_reports_a_dirty_exact_match_as_forceable_without_changing_git_state() {
    let (temporary, repository) = fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
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
    assert!(stdout.contains(
        "▎ forceable repository feature/merged merged pull request #42; dirty worktree (1 file)\n"
    ));
    assert!(stdout.contains("1 bench forceable with `bu recycle --force`\n"));
    assert!(!stdout.contains(" M README.md\n"));
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(git(&repository, &["show-ref", "--head"]), before_refs);
}

#[test]
fn ordinary_recycle_leaves_a_forceable_bench_unchanged_and_explains_force() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    fs::write(repository.join("README.md"), "dirty fixture\n").expect("dirty fixture");
    let before = git(&repository, &["status", "--porcelain=v1", "--branch"]);
    let feature_before = git(&repository, &["rev-parse", "feature/merged"]);
    let standin_before = git(&repository, &["rev-parse", "main-01"]);

    let output = recycle(&config, &fake_bin);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ forceable repository feature/merged"));
    assert!(stdout.contains("run `bu recycle --force` to discard 1 dirty file"));
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(
        git(&repository, &["rev-parse", "feature/merged"]),
        feature_before
    );
    assert_eq!(git(&repository, &["rev-parse", "main-01"]), standin_before);
}

#[test]
fn force_recycle_discards_tracked_and_untracked_changes_but_preserves_ignored_and_stash() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    let feature_before = git(&repository, &["rev-parse", "feature/merged"]);
    fs::write(repository.join("STASHED.md"), "stash fixture\n").expect("stash fixture");
    git(
        &repository,
        &["stash", "push", "--include-untracked", "-m", "keep me"],
    );
    let stash_before = git(&repository, &["stash", "list"]);
    fs::write(repository.join("README.md"), "unstaged change\n").expect("unstaged change");
    fs::write(repository.join("STAGED.md"), "staged change\n").expect("staged change");
    git(&repository, &["add", "STAGED.md"]);
    fs::write(repository.join("UNTRACKED.md"), "untracked change\n").expect("untracked change");
    fs::write(repository.join("ignored.log"), "preserve me\n").expect("ignored change");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ recycled repository main-01 feature/merged preserved"));
    assert!(stdout.contains("    discarded: README.md\n"));
    assert!(stdout.contains("    discarded: STAGED.md\n"));
    assert!(stdout.contains("    discarded: UNTRACKED.md\n"));
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "main-01"
    );
    assert_eq!(
        git(&repository, &["rev-parse", "feature/merged"]),
        feature_before
    );
    assert_eq!(git(&repository, &["stash", "list"]), stash_before);
    assert_eq!(
        fs::read_to_string(repository.join("ignored.log")).expect("ignored file remains"),
        "preserve me\n"
    );
    assert!(!repository.join("STAGED.md").exists());
    assert!(!repository.join("UNTRACKED.md").exists());
    assert!(git(&repository, &["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn force_recycle_does_not_overwrite_an_ignored_file_tracked_by_upstream() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    git(&repository, &["switch", "main"]);
    fs::write(repository.join("ignored.log"), "upstream contents\n")
        .expect("upstream ignored fixture");
    git(&repository, &["add", "--force", "ignored.log"]);
    git(
        &repository,
        &["commit", "-m", "track formerly ignored path"],
    );
    git(&repository, &["push", "origin", "main"]);
    git(&repository, &["switch", "feature/merged"]);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    let feature_before = git(&repository, &["rev-parse", "feature/merged"]);
    fs::write(repository.join("README.md"), "discard me\n").expect("tracked change");
    fs::write(repository.join("ignored.log"), "preserve me\n").expect("ignored local file");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("worktree switch failed"));
    assert_eq!(
        fs::read_to_string(repository.join("ignored.log")).expect("ignored file remains"),
        "preserve me\n"
    );
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "feature/merged"
    );
    assert_eq!(
        git(&repository, &["rev-parse", "feature/merged"]),
        feature_before
    );
}

#[test]
fn force_recycle_preserves_ignored_files_obstructing_a_tracked_path() {
    let (temporary, repository) = recycle_fixture_repository();
    fs::write(repository.join("node"), "tracked fixture\n").expect("tracked file");
    fs::write(repository.join(".gitignore"), "ignored.log\nnode/\n").expect("ignore fixture");
    git(&repository, &["add", "node", ".gitignore"]);
    git(
        &repository,
        &["commit", "-m", "add tracked obstruction fixture"],
    );
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    fs::remove_file(repository.join("node")).expect("remove tracked file");
    fs::create_dir(repository.join("node")).expect("obstructing directory");
    fs::write(repository.join("node/keep.log"), "preserve me\n").expect("ignored file");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cleanup failed; worktree may be partially cleaned"));
    assert!(stdout.contains("failed phase: tracked reset"));
    assert!(stdout.contains("hard reset would delete ignored path node/keep.log"));
    assert_eq!(
        fs::read_to_string(repository.join("node/keep.log")).expect("ignored file remains"),
        "preserve me\n"
    );
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "feature/merged"
    );
}

#[test]
fn force_recycle_blocks_a_dirty_branch_without_an_exact_merged_head_match() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "origin/main"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    fs::write(repository.join("README.md"), "must remain\n").expect("dirty fixture");
    let before = git(&repository, &["status", "--porcelain=v1", "--branch"]);
    let feature_before = git(&repository, &["rev-parse", "feature/merged"]);

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "▎ blocked  repository feature/merged current branch does not have exactly one merged pull request"
    ));
    assert_eq!(
        git(&repository, &["status", "--porcelain=v1", "--branch"]),
        before
    );
    assert_eq!(
        git(&repository, &["rev-parse", "feature/merged"]),
        feature_before
    );
}

#[test]
fn force_recycle_reports_the_cleanup_phase_and_remaining_paths() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    fake_git(
        &fake_bin,
        r#"if [ "$1" = "reset" ]; then
  echo "injected reset failure" >&2
  exit 41
fi"#,
    );
    fs::write(repository.join("README.md"), "must remain\n").expect("tracked fixture");
    fs::write(repository.join("UNTRACKED.md"), "must remain\n").expect("untracked fixture");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(
        "▎ failed   repository feature/merged cleanup failed; worktree may be partially cleaned"
    ));
    assert!(stdout.contains("    failed phase: tracked reset\n"));
    assert!(stdout.contains("injected reset failure"));
    assert!(stdout.contains("    remaining: README.md\n"));
    assert!(stdout.contains("    remaining: UNTRACKED.md\n"));
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "feature/merged"
    );
    assert!(repository.join("UNTRACKED.md").exists());
}

#[test]
fn force_recycle_reports_partial_cleanup_when_untracked_removal_fails() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);
    fake_git(
        &fake_bin,
        r#"if [ "$1" = "clean" ]; then
  echo "injected clean failure" >&2
  exit 42
fi"#,
    );
    fs::write(repository.join("README.md"), "reset me\n").expect("tracked fixture");
    fs::write(repository.join("UNTRACKED.md"), "must remain\n").expect("untracked fixture");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("    failed phase: untracked cleanup\n"));
    assert!(stdout.contains("    remaining: UNTRACKED.md\n"));
    assert!(!stdout.contains("    remaining: README.md\n"));
    assert_eq!(
        fs::read_to_string(repository.join("README.md")).expect("tracked file reset"),
        "fixture\n"
    );
    assert!(repository.join("UNTRACKED.md").exists());
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "feature/merged"
    );
}

#[test]
fn force_recycle_rechecks_the_pull_request_before_deleting_files() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let head = git(&repository, &["rev-parse", "HEAD"]);
    let mismatched_head = git(&repository, &["rev-parse", "origin/main"]);
    let counter = temporary.path().join("gh-count");
    let fake_bin = fake_gh(
        &temporary,
        &format!(
            r#"count_file='{}'
count=$(cat "$count_file" 2>/dev/null || printf 0)
if [ "$count" -eq 0 ]; then
  oid='{}'
else
  oid='{}'
fi
printf '%s' "$((count + 1))" > "$count_file"
printf '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"feature/merged","headRefOid":"%s"}}]\n' "$oid""#,
            counter.display(),
            head.trim(),
            mismatched_head.trim(),
        ),
    );
    fs::write(repository.join("README.md"), "must remain\n").expect("dirty fixture");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "▎ blocked  repository feature/merged current branch does not have exactly one merged pull request"
    ));
    assert_eq!(
        fs::read_to_string(repository.join("README.md")).expect("dirty file remains"),
        "must remain\n"
    );
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "feature/merged"
    );
}

#[test]
fn force_recycle_aborts_when_head_moves_after_the_pull_request_recheck() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let verified_head = git(&repository, &["rev-parse", "HEAD"]);
    let moved_head = git(&repository, &["rev-parse", "origin/main"]);
    let counter = temporary.path().join("gh-count");
    let fake_bin = fake_gh(
        &temporary,
        &format!(
            r#"count_file='{}'
count=$(cat "$count_file" 2>/dev/null || printf 0)
if [ "$count" -eq 1 ]; then
  git update-ref refs/heads/feature/merged '{}'
fi
printf '%s' "$((count + 1))" > "$count_file"
printf '%s\n' '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"feature/merged","headRefOid":"{}"}}]'"#,
            counter.display(),
            moved_head.trim(),
            verified_head.trim(),
        ),
    );
    fs::write(repository.join("KEEP.md"), "must remain\n").expect("dirty fixture");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "▎ blocked  repository feature/merged current branch does not have exactly one merged pull request"
    ));
    assert_eq!(
        fs::read_to_string(repository.join("KEEP.md")).expect("dirty file remains"),
        "must remain\n"
    );
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "feature/merged"
    );
}

#[test]
fn force_recycle_aborts_when_a_git_operation_starts_after_the_recheck() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let verified_head = git(&repository, &["rev-parse", "HEAD"]);
    let counter = temporary.path().join("gh-count");
    let fake_bin = fake_gh(
        &temporary,
        &format!(
            r#"count_file='{}'
count=$(cat "$count_file" 2>/dev/null || printf 0)
if [ "$count" -eq 1 ]; then
  printf '%s\n' '{}' > "$(git rev-parse --git-path MERGE_HEAD)"
fi
printf '%s' "$((count + 1))" > "$count_file"
printf '%s\n' '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"feature/merged","headRefOid":"{}"}}]'"#,
            counter.display(),
            verified_head.trim(),
            verified_head.trim(),
        ),
    );
    fs::write(repository.join("KEEP.md"), "must remain\n").expect("dirty fixture");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("▎ blocked  repository feature/merged Git operation in progress: merge")
    );
    assert_eq!(
        fs::read_to_string(repository.join("KEEP.md")).expect("dirty file remains"),
        "must remain\n"
    );
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "feature/merged"
    );
}

#[test]
fn force_recycle_aborts_when_the_standin_changes_after_the_recheck() {
    let (temporary, repository) = recycle_fixture_repository();
    let config = write_config(&temporary, &repository);
    let verified_head = git(&repository, &["rev-parse", "HEAD"]);
    let counter = temporary.path().join("gh-count");
    let fake_bin = fake_gh(
        &temporary,
        &format!(
            r#"count_file='{}'
count=$(cat "$count_file" 2>/dev/null || printf 0)
if [ "$count" -eq 1 ]; then
  git update-ref -d refs/heads/main-01
fi
printf '%s' "$((count + 1))" > "$count_file"
printf '%s\n' '[{{"number":42,"state":"MERGED","mergedAt":"2026-07-28T00:00:00Z","headRefName":"feature/merged","headRefOid":"{}"}}]'"#,
            counter.display(),
            verified_head.trim(),
        ),
    );
    fs::write(repository.join("KEEP.md"), "must remain\n").expect("dirty fixture");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("▎ blocked  repository feature/merged stand-in branch is missing")
    );
    assert_eq!(
        fs::read_to_string(repository.join("KEEP.md")).expect("dirty file remains"),
        "must remain\n"
    );
    assert!(
        git(&repository, &["branch", "--list", "main-01"])
            .trim()
            .is_empty()
    );
}

#[test]
fn one_force_command_recycles_every_forceable_bench() {
    let (temporary, repository) = recycle_fixture_repository();
    let second_bench = add_numbered_bench(&repository, "02", "feature/two");
    let config = temporary.path().join("two-benches.toml");
    fs::write(
        &config,
        format!(
            "[repository]\npath = \"{}\"\nremote = \"origin\"\nmain_branch = \"main\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"main-01\"\n\n[[benches]]\npath = \"{}\"\nstandin_branch = \"main-02\"\n",
            repository.display(),
            repository.display(),
            second_bench.display(),
        ),
    )
    .expect("config file");
    let fake_bin = fake_gh(&temporary, matching_merged_pull_request_body());
    fs::write(repository.join("ONE.md"), "discard one\n").expect("first dirty file");
    fs::write(second_bench.join("TWO.md"), "discard two\n").expect("second dirty file");

    let output = recycle_with(&config, &fake_bin, &["--force"], &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("    discarded: ONE.md\n"));
    assert!(stdout.contains("    discarded: TWO.md\n"));
    assert!(stdout.contains("2 recycled, 0 blocked, 0 skipped, 0 failed\n"));
    assert_eq!(
        git(&repository, &["branch", "--show-current"]).trim(),
        "main-01"
    );
    assert_eq!(
        git(&second_bench, &["branch", "--show-current"]).trim(),
        "main-02"
    );
    assert!(!repository.join("ONE.md").exists());
    assert!(!second_bench.join("TWO.md").exists());
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("▎ failed   repository unknown upstream fetch failed"));
    assert!(stdout.contains("    error: could not fetch upstream main before recycling:"));
    assert!(
        stdout.contains("git fetch [\"fetch\", \"--no-tags\", \"missing\""),
        "{stdout}"
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
    fs::write(repository.join("SOURCE.md"), "source\n").expect("rename source");
    git(&repository, &["add", "SOURCE.md"]);
    git(&repository, &["commit", "-m", "add rename source"]);
    fs::write(repository.join("STAGED.md"), "staged\n").expect("staged file");
    git(&repository, &["add", "STAGED.md"]);
    fs::write(repository.join("README.md"), "unstaged\n").expect("unstaged file");
    fs::write(repository.join("UNTRACKED.md"), "untracked\n").expect("untracked file");
    git(&repository, &["mv", "SOURCE.md", "RENAMED.md"]);
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);

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
    let source = "source\nname\u{1b}[31m";
    fs::write(repository.join(source), "source\n").expect("rename source");
    git(&repository, &["add", source]);
    git(
        &repository,
        &["commit", "-m", "add control-character rename source"],
    );
    git(&repository, &["mv", source, "RENAMED.md"]);
    fs::write(repository.join("line\nname\u{1b}[31m"), "dirty\n").expect("dirty file");
    let pull_request = merged_pull_request_body(
        "feature/merged",
        git(&repository, &["rev-parse", "HEAD"]).trim(),
    );
    let fake_bin = fake_gh(&temporary, &pull_request);

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
