use nib_core::git::{git_pull_sync, parse_git_status, run_git_with_timeout};
use std::time::{Duration, Instant};

fn run(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(name: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!("lite_editor_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    run(&tmp, &["init"]);
    run(&tmp, &["config", "user.email", "test@example.com"]);
    run(&tmp, &["config", "user.name", "Lite Editor Test"]);
    tmp
}

#[test]
fn test_git_branch_fast() {
    let tmp = std::env::temp_dir().join("git_test_fast");
    let _ = std::fs::remove_dir_all(&tmp);
    std::process::Command::new("git")
        .args(["init", tmp.to_str().unwrap()])
        .output()
        .unwrap();

    let result = run_git_with_timeout(tmp.to_str().unwrap(), &["branch"], Duration::from_secs(5));
    assert!(
        result.is_ok()
            || !result
                .as_ref()
                .unwrap_err()
                .contains("not a git repository")
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_git_pull_auth_fails_fast() {
    let tmp = std::env::temp_dir().join("git_test_auth");
    let _ = std::fs::remove_dir_all(&tmp);
    std::process::Command::new("git")
        .args(["init", tmp.to_str().unwrap()])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-C",
            tmp.to_str().unwrap(),
            "remote",
            "add",
            "origin",
            "https://github.com/nonexistent-user-abc123/nonexistent-repo-xyz789.git",
        ])
        .output()
        .unwrap();

    let start = Instant::now();
    let result = run_git_with_timeout(tmp.to_str().unwrap(), &["pull"], Duration::from_secs(30));
    let elapsed = start.elapsed();

    assert!(result.is_err());
    assert!(
        elapsed < Duration::from_secs(15),
        "Auth failure took too long: {:?}",
        elapsed
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_git_pull_timeout_on_network_hang() {
    let tmp = std::env::temp_dir().join("git_test_timeout");
    let _ = std::fs::remove_dir_all(&tmp);
    std::process::Command::new("git")
        .args(["init", tmp.to_str().unwrap()])
        .output()
        .unwrap();
    // 192.0.2.1 is TEST-NET — packets are silently dropped, simulating network hang
    std::process::Command::new("git")
        .args([
            "-C",
            tmp.to_str().unwrap(),
            "remote",
            "add",
            "origin",
            "https://192.0.2.1/fake.git",
        ])
        .output()
        .unwrap();

    let timeout = Duration::from_secs(3);
    let start = Instant::now();
    let result = run_git_with_timeout(tmp.to_str().unwrap(), &["pull"], timeout);
    let elapsed = start.elapsed();

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("timed out"),
        "Expected timeout error, got: {}",
        err
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "Timeout took too long: {:?}",
        elapsed
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_git_pull_missing_upstream_is_friendly() {
    let tmp = init_repo("pull_missing_upstream");
    let remote = tmp.with_extension("remote.git");
    let _ = std::fs::remove_dir_all(&remote);
    std::fs::create_dir_all(&remote).unwrap();
    run(&remote, &["init", "--bare"]);
    run(&tmp, &["remote", "add", "origin", remote.to_str().unwrap()]);
    std::fs::write(tmp.join("a.txt"), "a\n").unwrap();
    run(&tmp, &["add", "a.txt"]);
    run(&tmp, &["commit", "-m", "initial"]);
    run(&tmp, &["checkout", "-b", "feature/stale"]);
    run(&tmp, &["config", "branch.feature/stale.remote", "origin"]);
    run(
        &tmp,
        &[
            "config",
            "branch.feature/stale.merge",
            "refs/heads/feature/stale",
        ],
    );

    let result = git_pull_sync(
        &tmp.to_string_lossy(),
        Some("feature/stale".to_string()),
        Some("origin/feature/stale".to_string()),
    );

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("no longer exists"),
        "unexpected error: {}",
        err
    );
    assert!(!err.contains("fatal:"), "raw fatal leaked: {}", err);

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&remote);
}

#[test]
fn test_git_status_keeps_staged_nested_path_intact() {
    let tmp = init_repo("status_nested");
    let path = tmp.join("rate/src/main/java/com/alibaba/rate/rate/application/survey");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("SurveyInfo.java"), "class SurveyInfo {}\n").unwrap();
    run(
        &tmp,
        &[
            "add",
            "rate/src/main/java/com/alibaba/rate/rate/application/survey/SurveyInfo.java",
        ],
    );

    let changes = parse_git_status(&tmp.to_string_lossy()).unwrap();

    assert_eq!(changes.len(), 1);
    assert_eq!(
        serde_json::to_value(&changes[0]).unwrap()["path"],
        "rate/src/main/java/com/alibaba/rate/rate/application/survey/SurveyInfo.java"
    );
    assert_eq!(serde_json::to_value(&changes[0]).unwrap()["staged"], true);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_git_status_keeps_unstaged_nested_path_intact() {
    let tmp = init_repo("status_unstaged_nested");
    let path = tmp.join("rate/src/main/java/com/alibaba/rate/rate/application/survey");
    std::fs::create_dir_all(&path).unwrap();
    let file = path.join("SurveyServiceImpl.java");
    std::fs::write(&file, "class SurveyServiceImpl {}\n").unwrap();
    run(
        &tmp,
        &[
            "add",
            "rate/src/main/java/com/alibaba/rate/rate/application/survey/SurveyServiceImpl.java",
        ],
    );
    run(&tmp, &["commit", "-m", "initial"]);
    std::fs::write(&file, "class SurveyServiceImpl { int changed; }\n").unwrap();

    let changes = parse_git_status(&tmp.to_string_lossy()).unwrap();

    assert_eq!(changes.len(), 1);
    assert_eq!(
        serde_json::to_value(&changes[0]).unwrap()["path"],
        "rate/src/main/java/com/alibaba/rate/rate/application/survey/SurveyServiceImpl.java"
    );
    assert_eq!(serde_json::to_value(&changes[0]).unwrap()["staged"], false);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_git_status_records_rename_old_path() {
    let tmp = init_repo("status_rename");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/Old Name.java"), "class OldName {}\n").unwrap();
    run(&tmp, &["add", "src/Old Name.java"]);
    run(&tmp, &["commit", "-m", "initial"]);
    run(&tmp, &["mv", "src/Old Name.java", "src/New Name.java"]);

    let changes = parse_git_status(&tmp.to_string_lossy()).unwrap();

    assert_eq!(changes.len(), 1);
    let value = serde_json::to_value(&changes[0]).unwrap();
    assert_eq!(value["path"], "src/New Name.java");
    assert_eq!(value["old_path"], "src/Old Name.java");
    assert_eq!(value["status"], "Renamed");
    assert_eq!(value["staged"], true);

    let _ = std::fs::remove_dir_all(&tmp);
}
