// 端到端验证 merge 视图写回链:真实 git 仓库制造冲突 →
// git_merge_conflicts 能列出 → git_resolve_by_choices 逐块选边落盘 →
// 文件内容正确、unmerged 清零(git add 生效)。

use nib_core::diff::MergeSide;

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

/// 允许失败的 git(merge 制造冲突时退出码非 0 是预期)
fn run_allow_fail(cwd: &std::path::Path, args: &[&str]) {
    let _ = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
}

#[test]
fn resolve_by_choices_end_to_end() {
    let tmp = std::env::temp_dir().join(format!("nib_merge_resolve_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    run(&tmp, &["init", "-b", "main"]);
    run(&tmp, &["config", "user.email", "test@example.com"]);
    run(&tmp, &["config", "user.name", "Nib Test"]);

    // base → 两个分支各改同一行,merge 必然冲突
    std::fs::write(tmp.join("a.txt"), "head\nbase\ntail\n").unwrap();
    run(&tmp, &["add", "."]);
    run(&tmp, &["commit", "-m", "base"]);
    run(&tmp, &["checkout", "-b", "feature"]);
    std::fs::write(tmp.join("a.txt"), "head\nfeature-change\ntail\n").unwrap();
    run(&tmp, &["commit", "-am", "feature"]);
    run(&tmp, &["checkout", "main"]);
    std::fs::write(tmp.join("a.txt"), "head\nmain-change\ntail\n").unwrap();
    run(&tmp, &["commit", "-am", "main"]);
    run_allow_fail(&tmp, &["merge", "feature"]);

    let cwd = tmp.to_string_lossy().to_string();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let conflicts = nib_core::git::git_merge_conflicts(cwd.clone()).await.unwrap();
        assert_eq!(conflicts, vec!["a.txt".to_string()], "merge 后应有一个冲突文件");

        let parsed = nib_core::git::parse_conflict_file(
            tmp.join("a.txt").to_string_lossy().to_string(),
        )
        .await
        .unwrap();
        assert_eq!(parsed.chunks.len(), 1);
        assert_eq!(parsed.chunks[0].ours_text, "main-change");
        assert_eq!(parsed.chunks[0].theirs_text, "feature-change");

        // 选边数不足 → 拒绝落盘
        let err = nib_core::git::git_resolve_by_choices(cwd.clone(), "a.txt".into(), vec![])
            .await
            .unwrap_err();
        assert!(err.contains("未选边"), "不完整选边必须报错: {}", err);

        nib_core::git::git_resolve_by_choices(cwd.clone(), "a.txt".into(), vec![MergeSide::Theirs])
            .await
            .unwrap();

        let resolved = std::fs::read_to_string(tmp.join("a.txt")).unwrap();
        assert_eq!(resolved, "head\nfeature-change\ntail\n");
        let conflicts = nib_core::git::git_merge_conflicts(cwd.clone()).await.unwrap();
        assert!(conflicts.is_empty(), "git add 后 unmerged 应清零");
    });

    let _ = std::fs::remove_dir_all(&tmp);
}
