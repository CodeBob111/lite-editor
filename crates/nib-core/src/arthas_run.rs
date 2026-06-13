// Arthas 执行子系统:把 arthas.rs 拼好的命令真正打到目标 JVM 并流式回显。
// 分工:arthas.rs 纯生成命令字符串(watch/trace/...);本模块负责
//   ① jps 列 Java 进程  ② 定位 arthas-boot.jar  ③ spawn
//      `java -jar arthas-boot.jar <pid> -c "<command>" --batch-mode`
//      后台线程读 stdout/stderr,逐行入 buffer,waker 唤醒 UI 拉取。
// 端到端已对 throwaway JVM 验证:attach 成功、watch 捕获真实 params/returnObj/cost、
// -n 次数到达后 batch 自动退出。找不到 arthas-boot.jar 时由 UI 显式提示,不伪造输出。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// jps 列出的一个 Java 进程(已滤掉 Jps 自身与非数字 pid)。
#[derive(Clone, Debug)]
pub struct JavaProc {
    pub pid: String,
    pub name: String,
}

/// 列当前机器上的 Java 进程(`jps -l`)。同步瞬时,UI 可直接调;jps 缺失则返回空。
pub fn list_java_processes() -> Vec<JavaProc> {
    let Ok(out) = Command::new("jps")
        .arg("-l")
        .env("PATH", crate::lsp::augmented_path())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut it = line.splitn(2, ' ');
            let pid = it.next()?.trim().to_string();
            let fqn = it.next().unwrap_or("").trim().to_string();
            if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // 滤掉 jps 工具自身
            if fqn.ends_with("Jps") {
                return None;
            }
            let short = fqn.rsplit(['.', '/']).next().unwrap_or(&fqn);
            let name = if short.is_empty() {
                format!("pid {pid}")
            } else {
                short.to_string()
            };
            Some(JavaProc { pid, name })
        })
        .collect()
}

/// 定位 arthas-boot.jar:环境变量 → ~/.arthas-boot.jar → ~/.arthas/arthas-boot.jar。
/// 命中即返回;都没有 → None(UI 据此提示安装,而非静默失败)。
pub fn locate_arthas_boot() -> Option<PathBuf> {
    for key in ["NIB_ARTHAS_BOOT", "ARTHAS_BOOT_JAR"] {
        if let Ok(p) = std::env::var(key) {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return Some(pb);
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    for cand in [
        format!("{home}/.arthas-boot.jar"),
        format!("{home}/.arthas/arthas-boot.jar"),
    ] {
        let pb = PathBuf::from(cand);
        if pb.is_file() {
            return Some(pb);
        }
    }
    None
}

/// 去除 ANSI 转义(arthas 输出带颜色码与控制序列;面板按纯文本渲染)。
fn strip_ansi(s: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").expect("static regex"));
    re.replace_all(s, "").trim_end().to_string()
}

/// 一次 Arthas 命令执行的句柄:三条后台线程(stdout/stderr 读取 + 进程 wait),
/// 行入共享 buffer。UI 用 take_dirty() 判新、lines() 取快照、is_finished() 判结束。
pub struct ArthasRun {
    child_pid: u32,
    lines: Arc<Mutex<Vec<String>>>,
    dirty: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl ArthasRun {
    /// spawn `java -jar <boot> <pid> -c "<command>" --batch-mode`,后台流式读取。
    /// `waker` 在每批新行 / 进程退出时触发,供 gpui 唤醒 pull 循环。
    pub fn spawn(
        boot_jar: &Path,
        pid: &str,
        command: &str,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<ArthasRun> {
        let mut child = Command::new("java")
            .arg("-jar")
            .arg(boot_jar)
            .arg(pid)
            .arg("-c")
            .arg(command)
            .arg("--batch-mode")
            .env("PATH", crate::lsp::augmented_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let child_pid = child.id();
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let dirty = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let push = move |lines: &Arc<Mutex<Vec<String>>>,
                         dirty: &Arc<AtomicBool>,
                         waker: &Arc<dyn Fn() + Send + Sync>,
                         raw: String| {
            let clean = strip_ansi(&raw);
            if let Ok(mut g) = lines.lock() {
                g.push(clean);
            }
            dirty.store(true, Ordering::Release);
            waker();
        };

        if let Some(out) = stdout {
            let (lines, dirty, waker, push) = (lines.clone(), dirty.clone(), waker.clone(), push);
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    push(&lines, &dirty, &waker, line);
                }
            });
        }
        if let Some(err) = stderr {
            let (lines, dirty, waker) = (lines.clone(), dirty.clone(), waker.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let clean = strip_ansi(&line);
                    if let Ok(mut g) = lines.lock() {
                        g.push(clean);
                    }
                    dirty.store(true, Ordering::Release);
                    waker();
                }
            });
        }
        // 进程退出守望线程:owns child,阻塞 wait 后置 finished 并唤醒(最后一次拉取)。
        {
            let (finished, waker) = (finished.clone(), waker.clone());
            std::thread::spawn(move || {
                let _ = child.wait();
                finished.store(true, Ordering::Release);
                waker();
            });
        }

        Ok(ArthasRun {
            child_pid,
            lines,
            dirty,
            finished,
        })
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// 「停止」:给 arthas-boot 子进程发 SIGTERM(batch 命令通常 -n 到达即自退,
    /// 此为长 watch 提前中止的安全出口)。
    pub fn kill(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child_pid as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        {
            let _ = self.child_pid;
        }
    }
}

impl Drop for ArthasRun {
    fn drop(&mut self) {
        if !self.is_finished() {
            self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_codes() {
        let raw = "\x1b[1;31mhello\x1b[0m world";
        assert_eq!(strip_ansi(raw), "hello world");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn list_java_processes_excludes_jps_itself() {
        // 不强求环境里有 Java 进程,只验证返回的每项 pid 是纯数字、name 非 Jps
        for p in list_java_processes() {
            assert!(p.pid.bytes().all(|b| b.is_ascii_digit()), "pid 必须纯数字: {}", p.pid);
            assert!(!p.name.ends_with("Jps"), "不应包含 jps 自身");
        }
    }

    // 端到端:对一个真实运行的目标 JVM 跑 watch,验证 spawn→流式→finish 这条
    // 面板实际走的路径。需外部环境(默认 ignore):
    //   NIB_TEST_PID=<pid> NIB_TEST_METHOD=Class#method cargo test -p nib-core \
    //     arthas_run::tests::attach_streams_real_output -- --ignored --nocapture
    #[test]
    #[ignore = "需 NIB_TEST_PID 指向一个活的目标 JVM + 可定位 arthas-boot.jar"]
    fn attach_streams_real_output() {
        use std::sync::atomic::AtomicBool;
        let pid = std::env::var("NIB_TEST_PID").expect("set NIB_TEST_PID");
        let cmd = std::env::var("NIB_TEST_CMD")
            .unwrap_or_else(|_| "watch ArthasTarget work '{params,returnObj}' -n 1 -x 1".into());
        let boot = locate_arthas_boot().expect("arthas-boot.jar 未定位到");

        let woke = Arc::new(AtomicBool::new(false));
        let w = woke.clone();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || w.store(true, Ordering::Release));

        let run = ArthasRun::spawn(&boot, &pid, &cmd, waker).expect("spawn 失败");
        for _ in 0..900 {
            if run.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let out = run.lines().join("\n");
        eprintln!("---- arthas 回显 ----\n{out}\n--------------------");
        assert!(run.is_finished(), "命令应在 -n 次数到达后自退");
        assert!(woke.load(Ordering::Acquire), "waker 应被触发过(有新行)");
        assert!(
            out.contains("method=") || out.contains("ts=") || out.contains("returnObj"),
            "应捕获到真实 watch 回显,实际:{out}"
        );
    }
}
