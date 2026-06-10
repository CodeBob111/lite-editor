// 并发压力——目标条款「使用过程中(含并发操作)不卡死」的机器断言。
// 60 个混合命令(文件读 / git status / 项目搜索)同时打满,断言全部正确完成
// 且总耗时有界:任何一处引入锁顺序死锁、锁中毒或串行化瓶颈,这里会超时/失败。

use lite_editor_lib::commands::{read_file, search_in_files};
use lite_editor_lib::git::git_status;
use std::time::{Duration, Instant};

fn run(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {:?} failed", args);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn sixty_concurrent_mixed_commands_all_complete() {
    let dir = std::env::temp_dir().join(format!("lite_editor_stress_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run(&dir, &["init"]);
    run(&dir, &["config", "user.email", "t@example.com"]);
    run(&dir, &["config", "user.name", "T"]);
    for i in 0..80 {
        std::fs::write(
            dir.join(format!("S{}.java", i)),
            format!("class S{} {{ stressToken(); }}\n", i),
        )
        .unwrap();
    }
    run(&dir, &["add", "."]);
    run(&dir, &["commit", "-m", "init"]);
    std::fs::write(dir.join("dirty.txt"), "x\n").unwrap();

    let dir_str = dir.to_string_lossy().to_string();
    let t0 = Instant::now();
    let mut handles = Vec::new();
    for i in 0..60 {
        let d = dir_str.clone();
        let p = dir.join(format!("S{}.java", i % 80)).to_string_lossy().to_string();
        handles.push(tokio::spawn(async move {
            match i % 3 {
                0 => read_file(p).await.map(|c| usize::from(!c.is_empty())),
                1 => git_status(d).await.map(|c| c.len()),
                _ => search_in_files(d, "stressToken".to_string(), Some(true), None)
                    .await
                    .map(|r| r.len()),
            }
        }));
    }

    for h in handles {
        let n = h.await.unwrap().expect("并发命令不应失败");
        assert!(n >= 1, "并发命令返回了空结果");
    }
    assert!(
        t0.elapsed() < Duration::from_secs(30),
        "60 个并发命令耗时 {:?},存在串行化瓶颈或锁竞争",
        t0.elapsed()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
