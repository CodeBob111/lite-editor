use crate::commands::on_worker;
use rayon::prelude::*;
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Serialize)]
pub struct GitRepo {
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct GitBranch {
    name: String,
    current: bool,
    remote: bool,
    ahead: i32,
    behind: i32,
    tracking: Option<String>,
    upstream_gone: bool,
}

#[derive(Serialize)]
pub struct GitCommit {
    hash: String,
    short_hash: String,
    parents: Vec<String>,
    author: String,
    date: String,
    refs: String,
    subject: String,
}

fn run_git(cwd: &str, args: &[&str]) -> Result<String, String> {
    run_git_with_timeout(cwd, args, Duration::from_secs(10))
}

fn run_git_raw(cwd: &str, args: &[&str]) -> Result<String, String> {
    run_git_with_timeout_raw(cwd, args, Duration::from_secs(10))
}

pub fn run_git_with_timeout(cwd: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    run_git_with_timeout_raw(cwd, args, timeout).map(|s| s.trim().to_string())
}

pub fn run_git_with_timeout_raw(
    cwd: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o ConnectTimeout=10 -o BatchMode=yes",
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    let start = Instant::now();
    let pid = child.id();
    let output = std::thread::scope(|s| {
        let handle = s.spawn(|| child.wait_with_output());
        loop {
            if handle.is_finished() {
                return handle
                    .join()
                    .map_err(|_| "git thread panicked".to_string())?
                    .map_err(|e| format!("Failed to read git output: {}", e));
            }
            if start.elapsed() > timeout {
                #[cfg(unix)]
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
                #[cfg(not(unix))]
                {
                    let _ = handle;
                }
                return Err(format!(
                    "Git operation timed out after {}s. Check network connection and credentials.",
                    timeout.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });

    let output = output?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "git command failed".into()
        } else {
            stderr
        })
    }
}

// 注意:本文件所有命令都会 spawn git 子进程并等待;同步 Tauri 命令在主线程执行,
// 等待期间整个 UI 冻住(clone 超时 600s)。因此全部包成 async + on_worker。
#[tauri::command]
pub async fn git_current_branch(cwd: String) -> Result<String, String> {
    on_worker(move || run_git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"])).await
}

#[derive(Serialize)]
pub struct BatchResult<T: Serialize> {
    pub path: String,
    pub result: Option<T>,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn git_current_branch_batch(paths: Vec<String>) -> Vec<BatchResult<String>> {
    tokio::task::spawn_blocking(move || {
        paths
            .par_iter()
            .map(|cwd| match run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
                Ok(branch) => BatchResult { path: cwd.clone(), result: Some(branch), error: None },
                Err(e) => BatchResult { path: cwd.clone(), result: None, error: Some(e) },
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub async fn git_status_batch(paths: Vec<String>) -> Vec<BatchResult<Vec<GitChange>>> {
    tokio::task::spawn_blocking(move || {
        paths
            .par_iter()
            .map(|cwd| match parse_git_status(cwd) {
                Ok(changes) => BatchResult { path: cwd.clone(), result: Some(changes), error: None },
                Err(e) => BatchResult { path: cwd.clone(), result: None, error: Some(e) },
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

pub fn parse_git_status(cwd: &str) -> Result<Vec<GitChange>, String> {
    let output = run_git_raw(cwd, &["status", "--porcelain=v1", "-z", "-uall"])?;
    let mut changes = Vec::new();
    let mut entries = output.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 { continue; }
        let index_status = entry.as_bytes()[0] as char;
        let worktree_status = entry.as_bytes()[1] as char;
        let file_path = entry[3..].to_string();
        let old_path = if matches!(index_status, 'R' | 'C') || matches!(worktree_status, 'R' | 'C') {
            entries.next().map(|path| path.to_string())
        } else {
            None
        };
        if index_status != ' ' && index_status != '?' {
            changes.push(GitChange {
                path: file_path.clone(),
                old_path: old_path.clone(),
                status: classify_status(index_status),
                staged: true,
            });
        }
        if worktree_status != ' ' {
            let status = if worktree_status == '?' { "Untracked".to_string() } else { classify_status(worktree_status) };
            changes.push(GitChange {
                path: file_path,
                old_path,
                status,
                staged: false,
            });
        }
    }
    Ok(changes)
}

#[tauri::command]
pub async fn git_list_branches(cwd: String) -> Result<Vec<GitBranch>, String> {
    on_worker(move || git_list_branches_sync(&cwd)).await
}

fn git_list_branches_sync(cwd: &str) -> Result<Vec<GitBranch>, String> {
    let local_output = run_git(
        cwd,
        &[
            "branch",
            "--format=%(HEAD)\t%(refname:short)\t%(upstream:short)\t%(upstream:track,nobracket)",
        ],
    )?;
    let remote_output =
        run_git(cwd, &["branch", "-r", "--format=%(refname:short)"]).unwrap_or_default();

    let mut branches = Vec::new();

    for line in local_output.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        let current = parts.first().is_some_and(|h| *h == "*");
        let name = parts.get(1).unwrap_or(&"").to_string();
        let tracking_raw = parts.get(2).unwrap_or(&"").to_string();
        let track_info = parts.get(3).unwrap_or(&"").to_string();

        let tracking = if tracking_raw.is_empty() {
            None
        } else {
            Some(tracking_raw)
        };

        let mut ahead = 0i32;
        let mut behind = 0i32;
        let mut upstream_gone = false;
        for part in track_info.split(", ") {
            let part = part.trim();
            if let Some(n) = part.strip_prefix("ahead ") {
                ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                behind = n.parse().unwrap_or(0);
            } else if part == "gone" {
                upstream_gone = true;
            }
        }

        if !name.is_empty() {
            branches.push(GitBranch {
                name,
                current,
                remote: false,
                ahead,
                behind,
                tracking,
                upstream_gone,
            });
        }
    }

    for line in remote_output.lines() {
        let name = line.trim();
        if name.is_empty() || name.contains("HEAD") {
            continue;
        }
        branches.push(GitBranch {
            name: name.to_string(),
            current: false,
            remote: true,
            ahead: 0,
            behind: 0,
            tracking: None,
            upstream_gone: false,
        });
    }

    sort_branches(&mut branches);

    Ok(branches)
}

// 返回序即面板展示序(排序自前端 git-panel.ts 迁入):local 按 current → master → main → 其余,
// 组内大小写不敏感字典序(近似 localeCompare,原串 tie-break);remote 排最后、
// 仅按名(rank 对 "origin/x" 恒为 10,等效名字序)。
fn sort_branches(branches: &mut [GitBranch]) {
    branches.sort_by_cached_key(|b| {
        (
            b.remote,
            branch_rank(b),
            b.name.to_lowercase(),
            b.name.clone(),
        )
    });
}

fn branch_rank(branch: &GitBranch) -> u8 {
    if branch.current {
        0
    } else if branch.name == "master" {
        1
    } else if branch.name == "main" {
        2
    } else {
        10
    }
}

#[tauri::command]
pub async fn git_checkout(cwd: String, branch: String, force: Option<bool>) -> Result<String, String> {
    on_worker(move || {
        if force.unwrap_or(false) {
            run_git(&cwd, &["checkout", "--force", &branch])
        } else {
            run_git(&cwd, &["checkout", &branch])
        }
    })
    .await
}

#[tauri::command]
pub async fn git_new_branch(cwd: String, branch: String, start_point: String) -> Result<String, String> {
    on_worker(move || run_git(&cwd, &["checkout", "-b", &branch, &start_point])).await
}

#[tauri::command]
pub async fn git_delete_branch(cwd: String, branch: String, force: bool) -> Result<String, String> {
    on_worker(move || {
        let flag = if force { "-D" } else { "-d" };
        run_git(&cwd, &["branch", flag, &branch])
    })
    .await
}

fn prune_sync(cwd: &str, remote: &str) -> Result<String, String> {
    run_git_with_timeout(cwd, &["fetch", "--prune", remote], Duration::from_secs(60))
}

#[tauri::command]
pub async fn git_prune(cwd: String, remote: Option<String>) -> Result<String, String> {
    on_worker(move || {
        let remote_name = remote.unwrap_or_else(|| "origin".to_string());
        prune_sync(&cwd, &remote_name)
    })
    .await
}

#[tauri::command]
pub async fn git_pull(
    cwd: String,
    branch: Option<String>,
    tracking: Option<String>,
) -> Result<String, String> {
    on_worker(move || git_pull_sync(&cwd, branch, tracking)).await
}

pub fn git_pull_sync(
    cwd: &str,
    branch: Option<String>,
    tracking: Option<String>,
) -> Result<String, String> {
    match (branch, tracking) {
        (Some(local_branch), Some(upstream)) => {
            let (remote, remote_branch) = parse_upstream(&upstream)?;
            ensure_remote_branch_exists(cwd, remote, remote_branch)?;
            run_git_with_timeout(
                cwd,
                &[
                    "fetch",
                    remote,
                    &format!("refs/heads/{}:refs/heads/{}", remote_branch, local_branch),
                ],
                Duration::from_secs(60),
            )
        }
        (Some(local_branch), None) => Err(format!(
            "Branch '{}' has no upstream. Check it out and set an upstream before updating it.",
            local_branch
        )),
        (None, _) => {
            match run_git(
                cwd,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            ) {
                Ok(upstream) => {
                    let (remote, remote_branch) = parse_upstream(&upstream)?;
                    ensure_remote_branch_exists(cwd, remote, remote_branch)?;
                    run_git_with_timeout(cwd, &["pull", "--no-rebase"], Duration::from_secs(60))
                }
                // No upstream configured. If origin has a same-named branch, adopt
                // it as the upstream and pull; otherwise there's nothing to pull.
                Err(_) => {
                    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
                    if branch.is_empty() || branch == "HEAD" {
                        return Err("Detached HEAD has no upstream to pull from.".to_string());
                    }
                    let remote = "origin";
                    // Fetching the matching branch both verifies it exists on the
                    // remote (clear error if not) and creates the remote-tracking
                    // ref that --set-upstream-to requires.
                    run_git_with_timeout(
                        cwd,
                        &["fetch", remote, branch.as_str()],
                        Duration::from_secs(60),
                    )?;
                    run_git(
                        cwd,
                        &["branch", &format!("--set-upstream-to={}/{}", remote, branch)],
                    )?;
                    run_git_with_timeout(cwd, &["pull", "--no-rebase"], Duration::from_secs(60))
                }
            }
        }
    }
}

fn parse_upstream(upstream: &str) -> Result<(&str, &str), String> {
    let (remote, branch) = upstream
        .split_once('/')
        .ok_or_else(|| format!("Invalid upstream branch: {}", upstream))?;
    if remote.is_empty() || branch.is_empty() {
        return Err(format!("Invalid upstream branch: {}", upstream));
    }
    Ok((remote, branch))
}

fn ensure_remote_branch_exists(cwd: &str, remote: &str, branch: &str) -> Result<(), String> {
    match run_git_with_timeout_raw(
        cwd,
        &["ls-remote", "--exit-code", "--heads", remote, branch],
        Duration::from_secs(30),
    ) {
        Ok(_) => Ok(()),
        Err(err) if err == "git command failed" => {
            let _ = prune_sync(cwd, remote);
            Err(format!(
                "Upstream branch '{}/{}' no longer exists on remote. Stale remote refs were pruned; set a new upstream or delete the local branch.",
                remote, branch
            ))
        }
        Err(err) => Err(format!(
            "Unable to verify upstream branch '{}/{}': {}",
            remote, branch, err
        )),
    }
}

#[tauri::command]
pub async fn git_fetch_branch(cwd: String, remote: String, branch: String) -> Result<String, String> {
    on_worker(move || run_git_with_timeout(&cwd, &["fetch", &remote, &branch], Duration::from_secs(60))).await
}

#[tauri::command]
pub async fn git_push(cwd: String, branch: String) -> Result<String, String> {
    on_worker(move || run_git_with_timeout(&cwd, &["push", "origin", &branch], Duration::from_secs(60))).await
}

#[derive(Serialize)]
pub struct MergeResult {
    pub success: bool,
    pub message: String,
    pub conflicts: Vec<String>,
}

#[tauri::command]
pub async fn git_merge(cwd: String, branch: String) -> Result<MergeResult, String> {
    on_worker(move || {
        match run_git_with_timeout(&cwd, &["merge", &branch], Duration::from_secs(30)) {
            Ok(msg) => Ok(MergeResult { success: true, message: msg, conflicts: vec![] }),
            Err(e) => {
                let conflicts = list_unmerged_files(&cwd);
                if !conflicts.is_empty() {
                    Ok(MergeResult { success: false, message: e, conflicts })
                } else {
                    Err(e)
                }
            }
        }
    })
    .await
}

fn list_unmerged_files(cwd: &str) -> Vec<String> {
    run_git(cwd, &["diff", "--name-only", "--diff-filter=U"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

#[tauri::command]
pub async fn git_merge_conflicts(cwd: String) -> Result<Vec<String>, String> {
    on_worker(move || Ok(list_unmerged_files(&cwd))).await
}

#[tauri::command]
pub async fn git_show_conflict_version(cwd: String, rel_path: String, stage: u32) -> Result<String, String> {
    on_worker(move || {
        if !(1..=3).contains(&stage) {
            return Err("stage must be 1 (base), 2 (ours), or 3 (theirs)".into());
        }
        run_git_raw(&cwd, &["show", &format!(":{}:{}", stage, rel_path)])
    })
    .await
}

#[tauri::command]
pub async fn git_merge_abort(cwd: String) -> Result<String, String> {
    on_worker(move || run_git(&cwd, &["merge", "--abort"])).await
}

#[tauri::command]
pub async fn git_resolve_conflict_file(cwd: String, rel_path: String, content: String) -> Result<String, String> {
    on_worker(move || {
        let abs_path = Path::new(&cwd).join(&rel_path);
        std::fs::write(&abs_path, &content)
            .map_err(|e| format!("Failed to write file: {}", e))?;
        run_git(&cwd, &["add", "--", &rel_path])
    })
    .await
}

#[tauri::command]
pub async fn git_checkout_conflict_side(cwd: String, rel_path: String, side: String) -> Result<String, String> {
    on_worker(move || {
        let flag = match side.as_str() {
            "ours" => "--ours",
            "theirs" => "--theirs",
            _ => return Err("side must be 'ours' or 'theirs'".into()),
        };
        run_git(&cwd, &["checkout", flag, "--", &rel_path])?;
        run_git(&cwd, &["add", "--", &rel_path])
    })
    .await
}

/// Discard local changes to a single path (IDEA's "Rollback").
/// - Untracked: no committed version exists, so rolling back removes the file.
/// - Added (staged-new): unstage, then remove from the working tree.
/// - Tracked (Modified/Deleted/...): restore both index and working tree from HEAD.
#[tauri::command]
pub async fn git_discard_changes(cwd: String, rel_path: String, status: String) -> Result<String, String> {
    on_worker(move || {
        match status.as_str() {
            "Untracked" => {
                std::fs::remove_file(Path::new(&cwd).join(&rel_path))
                    .map_err(|e| format!("Failed to delete file: {}", e))?;
                Ok(String::new())
            }
            "Added" => {
                run_git(&cwd, &["reset", "--quiet", "HEAD", "--", &rel_path])?;
                std::fs::remove_file(Path::new(&cwd).join(&rel_path))
                    .map_err(|e| format!("Failed to delete file: {}", e))?;
                Ok(String::new())
            }
            _ => run_git(&cwd, &["checkout", "HEAD", "--", &rel_path]),
        }
    })
    .await
}

#[tauri::command]
pub async fn git_rebase(cwd: String, branch: String) -> Result<String, String> {
    on_worker(move || run_git_with_timeout(&cwd, &["rebase", &branch], Duration::from_secs(30))).await
}

#[tauri::command]
pub async fn git_rename_branch(
    cwd: String,
    old_name: String,
    new_name: String,
) -> Result<String, String> {
    on_worker(move || run_git(&cwd, &["branch", "-m", &old_name, &new_name])).await
}

#[tauri::command]
pub async fn git_log(cwd: String, branch: String, limit: Option<u32>) -> Result<Vec<GitCommit>, String> {
    on_worker(move || git_log_sync(&cwd, &branch, limit)).await
}

fn git_log_sync(cwd: &str, branch: &str, limit: Option<u32>) -> Result<Vec<GitCommit>, String> {
    let max_count = limit.unwrap_or(200).clamp(1, 1000).to_string();
    let output = run_git_raw(
        cwd,
        &[
            "log",
            "--date=iso-strict",
            "--decorate=short",
            &format!("--max-count={}", max_count),
            "--pretty=format:%H%x1f%h%x1f%P%x1f%an%x1f%ad%x1f%D%x1f%s%x1e",
            branch,
        ],
    )?;

    let mut commits = Vec::new();
    for record in output.split('\x1e') {
        let record = record.trim_matches('\n');
        if record.is_empty() {
            continue;
        }
        let parts: Vec<&str> = record.splitn(7, '\x1f').collect();
        if parts.len() < 7 {
            continue;
        }
        commits.push(GitCommit {
            hash: parts[0].to_string(),
            short_hash: parts[1].to_string(),
            parents: parts[2]
                .split_whitespace()
                .map(|p| p.to_string())
                .collect(),
            author: parts[3].to_string(),
            date: parts[4].to_string(),
            refs: parts[5].to_string(),
            subject: parts[6].to_string(),
        });
    }
    Ok(commits)
}

#[tauri::command]
pub async fn git_commit_patch(cwd: String, hash: String) -> Result<String, String> {
    on_worker(move || run_git_raw(&cwd, &["format-patch", "-1", "--stdout", &hash])).await
}

#[tauri::command]
pub async fn git_cherry_pick(cwd: String, hash: String) -> Result<String, String> {
    on_worker(move || run_git_with_timeout(&cwd, &["cherry-pick", &hash], Duration::from_secs(60))).await
}

#[tauri::command]
pub async fn git_checkout_revision(cwd: String, hash: String) -> Result<String, String> {
    on_worker(move || run_git(&cwd, &["checkout", &hash])).await
}

#[tauri::command]
pub async fn git_revert_commit(cwd: String, hash: String) -> Result<String, String> {
    on_worker(move || run_git_with_timeout(&cwd, &["revert", "--no-edit", &hash], Duration::from_secs(60))).await
}

#[tauri::command]
pub async fn git_new_branch_at_commit(
    cwd: String,
    branch: String,
    hash: String,
) -> Result<String, String> {
    on_worker(move || run_git(&cwd, &["checkout", "-b", &branch, &hash])).await
}

#[derive(Serialize)]
pub struct GitChange {
    path: String,
    old_path: Option<String>,
    status: String,
    staged: bool,
}

#[tauri::command]
pub async fn git_commit(cwd: String, files: Vec<String>, message: String) -> Result<String, String> {
    on_worker(move || {
        if files.is_empty() {
            return Err("No files selected".to_string());
        }
        if message.trim().is_empty() {
            return Err("Commit message is empty".to_string());
        }
        let file_refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        let mut add_args = vec!["add", "--"];
        add_args.extend_from_slice(&file_refs);
        run_git(&cwd, &add_args)?;
        run_git(&cwd, &["commit", "-m", &message])
    })
    .await
}

#[tauri::command]
pub async fn git_status(cwd: String) -> Result<Vec<GitChange>, String> {
    on_worker(move || parse_git_status(&cwd)).await
}

#[tauri::command]
pub async fn git_show_file(cwd: String, rel_path: String) -> Result<String, String> {
    on_worker(move || run_git_raw(&cwd, &["show", &format!("HEAD:{}", rel_path)])).await
}

#[tauri::command]
pub async fn git_show_staged(cwd: String, rel_path: String) -> Result<String, String> {
    on_worker(move || run_git_raw(&cwd, &["show", &format!(":{}", rel_path)])).await
}

fn classify_status(c: char) -> String {
    match c {
        'M' => "Modified".to_string(),
        'A' => "Added".to_string(),
        'D' => "Deleted".to_string(),
        'R' => "Renamed".to_string(),
        'C' => "Copied".to_string(),
        'U' => "Unmerged".to_string(),
        _ => format!("{}", c),
    }
}

#[tauri::command]
pub async fn git_discover_repos(root: String) -> Vec<GitRepo> {
    tokio::task::spawn_blocking(move || git_discover_repos_sync(root))
        .await
        .unwrap_or_default()
}

fn git_discover_repos_sync(root: String) -> Vec<GitRepo> {
    let root_path = Path::new(&root);
    if root_path.join(".git").exists() {
        return vec![GitRepo {
            name: root_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| root.clone()),
            path: root,
        }];
    }

    let mut repos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root_path) {
        for entry in entries.flatten() {
            if entry.path().join(".git").exists() {
                let name = entry.file_name().to_string_lossy().to_string();
                repos.push(GitRepo {
                    name,
                    path: entry.path().to_string_lossy().to_string(),
                });
            }
        }
    }
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    repos
}

#[derive(Serialize, Clone)]
pub struct BlameLine {
    pub author: String,
    pub timestamp: i64,
}

#[tauri::command]
pub async fn git_blame(cwd: String, file_path: String) -> Result<Vec<BlameLine>, String> {
    on_worker(move || git_blame_sync(&cwd, &file_path)).await
}

fn git_blame_sync(cwd: &str, file_path: &str) -> Result<Vec<BlameLine>, String> {
    let file_dir = std::path::Path::new(file_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.to_string());
    let repo_root = run_git(&file_dir, &["rev-parse", "--show-toplevel"])
        .unwrap_or_else(|_| cwd.to_string());
    let rel = if file_path.starts_with(&repo_root) {
        file_path[repo_root.len()..].trim_start_matches('/').to_string()
    } else {
        file_path.to_string()
    };
    let output = run_git_with_timeout(
        &repo_root,
        &["blame", "--porcelain", "--", &rel],
        Duration::from_secs(30),
    )?;

    let mut commits: std::collections::HashMap<String, (String, i64)> = std::collections::HashMap::new();
    let mut lines: Vec<BlameLine> = Vec::new();
    let mut current_sha = String::new();

    for line in output.lines() {
        if line.starts_with('\t') {
            if let Some((author, ts)) = commits.get(&current_sha) {
                lines.push(BlameLine {
                    author: author.clone(),
                    timestamp: *ts,
                });
            } else {
                lines.push(BlameLine {
                    author: "?".into(),
                    timestamp: 0,
                });
            }
        } else if line.len() >= 40 && line.as_bytes().iter().take(40).all(|b| b.is_ascii_hexdigit()) {
            let sha = &line[..40];
            current_sha = sha.to_string();
        } else if let Some(rest) = line.strip_prefix("author ") {
            commits
                .entry(current_sha.clone())
                .or_insert_with(|| (String::new(), 0))
                .0 = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            if let Ok(ts) = rest.parse::<i64>() {
                commits
                    .entry(current_sha.clone())
                    .or_insert_with(|| (String::new(), 0))
                    .1 = ts;
            }
        }
    }

    Ok(lines)
}

#[tauri::command]
pub async fn git_clone(url: String, directory: String) -> Result<String, String> {
    on_worker(move || {
        let dir_path = std::path::Path::new(&directory);
        if dir_path.exists() {
            let is_empty = dir_path
                .read_dir()
                .map(|mut e| e.next().is_none())
                .unwrap_or(false);
            if !is_empty {
                return Err(format!("Directory '{}' already exists and is not empty", directory));
            }
        }
        if let Some(parent) = dir_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent directory: {}", e))?;
        }
        let parent_dir = dir_path
            .parent()
            .ok_or("Invalid directory path")?
            .to_string_lossy()
            .to_string();
        run_git_with_timeout(&parent_dir, &["clone", &url, &directory], Duration::from_secs(600))
    })
    .await
}

#[tauri::command]
pub async fn git_remote_url(cwd: String, remote: Option<String>) -> Result<String, String> {
    on_worker(move || {
        let remote = remote.as_deref().unwrap_or("origin");
        run_git(&cwd, &["remote", "get-url", remote])
    })
    .await
}

// ---- 冲突标记解析(3-way merge 编辑器用) ----
// 自前端 merge-conflict.ts 迁入。行分割 split('\n') + 重建 join("\n"):行内 \r 与
// 尾部空行原样保留,CRLF 文件经 merge 保存不被静默 LF 化(字节级保真)。
// 收 path:文件由 Rust 直接读盘解析,前端不再为解析上传全文;下行的是
// ours/theirs 两份重建文本与 chunk 区间。

#[derive(Serialize)]
pub struct ConflictChunk {
    ours_start: usize,
    ours_end: usize,
    theirs_start: usize,
    theirs_end: usize,
    ours_text: String,
    theirs_text: String,
}

#[derive(Serialize)]
pub struct ConflictParse {
    ours: String,
    theirs: String,
    chunks: Vec<ConflictChunk>,
}

pub(crate) fn parse_conflict_markers(text: &str) -> ConflictParse {
    let mut ours_lines: Vec<&str> = Vec::new();
    let mut theirs_lines: Vec<&str> = Vec::new();
    let mut chunks: Vec<ConflictChunk> = Vec::new();
    let mut in_ours = false;
    let mut in_theirs = false;
    let mut chunk_ours_start = 0usize;
    let mut chunk_theirs_start = 0usize;
    let mut chunk_ours_lines: Vec<&str> = Vec::new();
    let mut chunk_theirs_lines: Vec<&str> = Vec::new();
    // 自 "<<<<<<<" 起的原始行(含标记行):块未闭合时原文回灌双侧,不丢内容
    let mut raw_chunk: Vec<&str> = Vec::new();

    // 未闭合的标记块不是真冲突(正文里恰好有 "<<<<<<<" 开头的行、或文件被截断),
    // 原始行原样保留在两侧——否则 merge 保存会静默丢掉这段内容
    fn flush_unterminated<'a>(
        ours: &mut Vec<&'a str>,
        theirs: &mut Vec<&'a str>,
        raw: &mut Vec<&'a str>,
    ) {
        ours.extend(raw.iter().copied());
        theirs.extend(raw.iter().copied());
        raw.clear();
    }

    for line in text.split('\n') {
        if line.starts_with("<<<<<<<") {
            if in_ours || in_theirs {
                flush_unterminated(&mut ours_lines, &mut theirs_lines, &mut raw_chunk);
            }
            in_ours = true;
            in_theirs = false;
            chunk_ours_start = ours_lines.len();
            chunk_theirs_start = theirs_lines.len();
            chunk_ours_lines = Vec::new();
            chunk_theirs_lines = Vec::new();
            raw_chunk = vec![line];
            continue;
        }
        if line.starts_with("=======") && in_ours {
            in_ours = false;
            in_theirs = true;
            raw_chunk.push(line);
            continue;
        }
        if line.starts_with(">>>>>>>") && in_theirs {
            in_theirs = false;
            raw_chunk.clear();
            ours_lines.append(&mut chunk_ours_lines);
            theirs_lines.append(&mut chunk_theirs_lines);
            chunks.push(ConflictChunk {
                ours_start: chunk_ours_start,
                ours_end: ours_lines.len(),
                theirs_start: chunk_theirs_start,
                theirs_end: theirs_lines.len(),
                ours_text: ours_lines[chunk_ours_start..].join("\n"),
                theirs_text: theirs_lines[chunk_theirs_start..].join("\n"),
            });
            continue;
        }
        if in_ours {
            chunk_ours_lines.push(line);
            raw_chunk.push(line);
        } else if in_theirs {
            chunk_theirs_lines.push(line);
            raw_chunk.push(line);
        } else {
            ours_lines.push(line);
            theirs_lines.push(line);
        }
    }

    if in_ours || in_theirs {
        flush_unterminated(&mut ours_lines, &mut theirs_lines, &mut raw_chunk);
    }

    ConflictParse {
        ours: ours_lines.join("\n"),
        theirs: theirs_lines.join("\n"),
        chunks,
    }
}

#[tauri::command]
pub async fn parse_conflict_file(path: String) -> Result<ConflictParse, String> {
    on_worker(move || {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {}", path, e))?;
        Ok(parse_conflict_markers(&content))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_conflict_chunks() {
        let text = "head\n<<<<<<< HEAD\na1\n=======\nb1\n>>>>>>> branch\nmid\n<<<<<<< HEAD\na2\na3\n=======\nb2\n>>>>>>> branch\ntail\n";
        let parsed = parse_conflict_markers(text);
        assert_eq!(parsed.ours, "head\na1\nmid\na2\na3\ntail\n");
        assert_eq!(parsed.theirs, "head\nb1\nmid\nb2\ntail\n");
        assert_eq!(parsed.chunks.len(), 2);
        let c1 = &parsed.chunks[0];
        assert_eq!((c1.ours_start, c1.ours_end), (1, 2));
        assert_eq!(c1.ours_text, "a1");
        assert_eq!(c1.theirs_text, "b1");
        let c2 = &parsed.chunks[1];
        assert_eq!((c2.ours_start, c2.ours_end), (3, 5));
        assert_eq!(c2.ours_text, "a2\na3");
    }

    #[test]
    fn no_markers_passes_through() {
        let text = "line1\nline2\n";
        let parsed = parse_conflict_markers(text);
        assert_eq!(parsed.ours, text);
        assert_eq!(parsed.theirs, text);
        assert!(parsed.chunks.is_empty());
    }

    #[test]
    fn crlf_content_is_preserved_byte_for_byte() {
        let text = "a\r\n<<<<<<< HEAD\r\nx\r\n=======\r\ny\r\n>>>>>>> branch\r\nz\r\n";
        let parsed = parse_conflict_markers(text);
        // 行内 \r 与尾部空行原样保留,merge 保存不重写行尾
        assert_eq!(parsed.ours, "a\r\nx\r\nz\r\n");
        assert_eq!(parsed.theirs, "a\r\ny\r\nz\r\n");
        assert_eq!(parsed.chunks[0].ours_text, "x\r");
    }

    #[test]
    fn orphan_separator_is_kept_in_both_sides() {
        let text = "a\n=======\nb";
        let parsed = parse_conflict_markers(text);
        assert_eq!(parsed.ours, text);
        assert_eq!(parsed.theirs, text);
        assert!(parsed.chunks.is_empty());
    }

    #[test]
    fn unterminated_marker_block_keeps_content() {
        // 正文里出现 "<<<<<<<" 开头的行但没有闭合 → 不是冲突,内容原样保留
        let text = "a\n<<<<<<< 教学示例\nb\nc\n";
        let parsed = parse_conflict_markers(text);
        assert_eq!(parsed.ours, text);
        assert_eq!(parsed.theirs, text);
        assert!(parsed.chunks.is_empty());
        // 未闭合块后紧跟一个真冲突:前者回灌,后者正常解析
        let text2 = "<<<<<<< x\norphan\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> br\ntail";
        let parsed2 = parse_conflict_markers(text2);
        assert_eq!(parsed2.ours, "<<<<<<< x\norphan\na\ntail");
        assert_eq!(parsed2.theirs, "<<<<<<< x\norphan\nb\ntail");
        assert_eq!(parsed2.chunks.len(), 1);
    }

    #[test]
    fn local_branches_sort_by_rank_then_ci_name() {
        let mk = |name: &str, current: bool, remote: bool| GitBranch {
            name: name.to_string(),
            current,
            remote,
            ahead: 0,
            behind: 0,
            tracking: None,
            upstream_gone: false,
        };
        let mut branches = [
            mk("Zeta", false, false),
            mk("alpha", false, false),
            mk("main", false, false),
            mk("feature/x", true, false),
            mk("origin/beta", false, true),
            mk("origin/Alpha", false, true),
        ];
        sort_branches(&mut branches);
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["feature/x", "main", "alpha", "Zeta", "origin/Alpha", "origin/beta"]
        );
    }
}
