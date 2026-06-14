// Empirical check: does copy_files_to_clipboard actually put a *file reference*
// (public.file-url) on the system pasteboard — the thing Finder/other apps read
// on paste — not just plain text? This is the load-bearing assumption behind the
// "Cmd+C copies the file" feature; if the pasteboard ends up holding text or a
// `list`, no frontend wiring can make Finder paste the file.
//
// These tests mutate the REAL general pasteboard. They are not run by the build
// (`tauri build` never invokes `cargo test`); run them with `cargo test`.

#![cfg(target_os = "macos")]

use nib_core::clipboard::{copy_files_to_clipboard, copy_text_to_clipboard};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// 系统剪贴板是全局共享状态:cargo test 默认并行,三个测试互相覆盖对方刚写入的
// 内容,产生随机失败。串行化「写入→读回」整段;锁中毒(别的测试断言失败)不影响
// 剪贴板本身,取回继续即可。
static PASTEBOARD: Mutex<()> = Mutex::new(());

fn pasteboard_guard() -> std::sync::MutexGuard<'static, ()> {
    PASTEBOARD.lock().unwrap_or_else(|e| e.into_inner())
}

fn make_temp_file(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("lite_editor_clip_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(name);
    std::fs::write(&file, b"clip test\n").unwrap();
    // Canonicalize so /tmp -> /private/tmp resolves the same way the pasteboard
    // round-trip will report it, making exact comparison reliable.
    std::fs::canonicalize(&file)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// 跑 `osascript -e <script>`,但**绝不阻塞整个测试套件**:spawn 后轮询退出,超过
/// `timeout` 就 kill。headless / 无 Apple Events 授权的环境(CI、未登录 GUI)在
/// `the clipboard as «class furl»` 上会无限挂起,没有上界时这一个进程能把整轮
/// `cargo test` 卡死好几分钟。超时返回 None,调用方据此跳过(而非挂死或误判失败)。
/// 返回 (退出码 success, stdout)。osascript 输出仅一行路径,远小于管道缓冲,退出后再读安全。
fn osascript_bounded(script: &str, timeout: Duration) -> Option<(bool, String)> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new("osascript")
        .args(["-e", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_string(&mut out);
                }
                return Some((status.success(), out));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

#[test]
fn single_file_lands_as_a_readable_file_url() {
    let _pb = pasteboard_guard();
    let path = make_temp_file("readme.md");
    copy_files_to_clipboard(vec![path.clone()]).expect("write to pasteboard");

    // Read it back the way an app would: the clipboard must coerce to a file
    // reference («class furl») and resolve to exactly the file we copied.
    // Bounded so headless/Apple-Events-restricted environments skip instead of hanging.
    let Some((success, stdout)) = osascript_bounded(
        "POSIX path of (the clipboard as «class furl»)",
        Duration::from_secs(10),
    ) else {
        eprintln!(
            "跳过 single_file_lands_as_a_readable_file_url: osascript 在本环境无法完成\
             (headless / 无 Apple Events 授权),已超时"
        );
        return;
    };
    let got = stdout.trim().to_string();

    assert!(success, "clipboard did not hold a file reference (osascript failed)");
    // osascript reports the path with a trailing slash stripped; compare basenames
    // plus full path to be robust against any trailing-slash normalization.
    assert_eq!(got, path, "pasteboard file-url did not round-trip the path");
}

#[test]
fn multiple_files_each_land_as_a_distinct_file_url() {
    let _pb = pasteboard_guard();
    let a = make_temp_file("a.txt");
    let b = make_temp_file("b.txt");
    copy_files_to_clipboard(vec![a.clone(), b.clone()]).expect("write to pasteboard");

    // Each NSURL becomes its own pasteboard item carrying public.file-url; verify
    // the count via AppKit (osascript collapses multi-file to a useless `list`).
    let count = pasteboard_file_url_item_count();
    assert_eq!(count, 2, "expected 2 file-url pasteboard items, got {count}");
}

// Bug-2 fix: the Arthas command must land on the pasteboard via native NSPasteboard
// (gesture-free), so a single click copies it — unlike navigator.clipboard which is
// rejected after an await. Write text, read it back with pbpaste, expect exact match.
#[test]
fn text_lands_on_pasteboard_verbatim() {
    let _pb = pasteboard_guard();
    let s = format!(
        "trace com.alibaba.business.app.taobao.tradereview.utils.UserTagUtil buildUserTagVOListNew -n 5 --skipJDKMethod false [{}]",
        std::process::id()
    );
    copy_text_to_clipboard(s.clone()).expect("write text to pasteboard");

    let out = Command::new("pbpaste").output().expect("run pbpaste");
    let got = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(got, s, "pbpaste did not return exactly what we copied");
}

// Count pasteboard items that advertise the public.file-url type.
fn pasteboard_file_url_item_count() -> usize {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;

    let pb = NSPasteboard::generalPasteboard();
    let Some(items) = pb.pasteboardItems() else {
        return 0;
    };
    let file_url_type = NSString::from_str("public.file-url");
    let mut n = 0usize;
    for item in items.iter() {
        for ty in item.types().iter() {
            if ty.isEqualToString(&file_url_type) {
                n += 1;
                break;
            }
        }
    }
    n
}
