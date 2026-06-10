// 负载下无阻塞——把「git 网络操作 / 大项目扫描期间界面不冻」从主观体验固化为机器断言。
//
// 原理:UI 冻结的唯一后端机制是「命令占住主线程/运行时」。这里在一个真实的重负载
// (网络挂死的 git fetch、数百文件的全量扫描)进行中,并发调用真实的 async 命令入口,
// 断言它们仍在毫秒级返回。若有人把命令改回同步阻塞或在 async 体里串行等待,测试立刻红。

use nib_core::fs::read_file;
use nib_core::search::search_in_files;
use nib_core::git::{git_status, run_git_with_timeout};
use std::time::{Duration, Instant};

fn run(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {:?} failed", args);
}

fn init_repo(name: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!("lite_editor_load_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    run(&tmp, &["init"]);
    run(&tmp, &["config", "user.email", "t@example.com"]);
    run(&tmp, &["config", "user.name", "T"]);
    std::fs::write(tmp.join("a.txt"), "hello\n").unwrap();
    run(&tmp, &["add", "a.txt"]);
    run(&tmp, &["commit", "-m", "init"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hanging_git_network_op_does_not_delay_other_commands() {
    let repo = init_repo("hang");
    let repo_str = repo.to_string_lossy().to_string();

    // 模拟 clone/push 挂死:向 TEST-NET(192.0.2.1,丢包不响应)fetch。
    // 走与命令完全相同的执行路径(spawn_blocking + run_git_with_timeout 轮询杀进程);
    // 用 5s 超时代替命令里的 60-600s,只为让测试本身收束,验证的并发性质与时长无关。
    let hang_repo = repo_str.clone();
    let hang = tokio::task::spawn_blocking(move || {
        let _ = run_git_with_timeout(
            &hang_repo,
            &["fetch", "https://192.0.2.1/fake.git"],
            Duration::from_secs(5),
        );
    });
    tokio::time::sleep(Duration::from_millis(300)).await; // 确保 fetch 已在挂

    // 挂死期间,真实命令入口必须保持毫秒级响应(阈值放宽到 2s 抗 CI 抖动)。
    let t = Instant::now();
    std::fs::write(repo.join("b.txt"), "dirty\n").unwrap();
    let changes = git_status(repo_str.clone()).await.expect("git_status");
    assert!(!changes.is_empty(), "应看到 b.txt 未跟踪");
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "git_status 在挂死的 fetch 旁被拖慢: {:?}",
        t.elapsed()
    );

    let t = Instant::now();
    let content = read_file(repo.join("a.txt").to_string_lossy().to_string())
        .await
        .expect("read_file");
    assert_eq!(content, "hello\n");
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "read_file 在挂死的 fetch 旁被拖慢: {:?}",
        t.elapsed()
    );

    hang.await.unwrap();
    let _ = std::fs::remove_dir_all(&repo);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_project_scan_does_not_delay_other_commands() {
    // 数百文件的项目全量扫描(search_in_files 会 walk + 读完所有文件,
    // 与索引构建同一类负载)进行中,单文件读取仍须即时返回。
    let dir = std::env::temp_dir().join(format!("lite_editor_scan_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let filler = "lorem ipsum dolor sit amet\n".repeat(80);
    for i in 0..500 {
        std::fs::write(dir.join(format!("F{}.java", i)), &filler).unwrap();
    }
    std::fs::write(dir.join("needle.java"), "class Needle { magicNeedleToken(); }\n").unwrap();

    let scan = tokio::spawn(search_in_files(
        dir.to_string_lossy().to_string(),
        "magicNeedleToken".to_string(),
        Some(true),
        None,
    ));

    let t = Instant::now();
    let content = read_file(dir.join("F0.java").to_string_lossy().to_string())
        .await
        .expect("read_file");
    assert!(!content.is_empty());
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "read_file 在全量扫描旁被拖慢: {:?}",
        t.elapsed()
    );

    let results = scan.await.unwrap().expect("search_in_files");
    assert_eq!(results.len(), 1, "扫描结果应命中 needle.java");

    let _ = std::fs::remove_dir_all(&dir);
}
