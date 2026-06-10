// 终端会话内核(M4,RFC 拉模型):alacritty_terminal 解析 PTY 输出,
// Term 状态机锁在 FairMutex 后面。**高频源不走 EventSink**——PTY 每个
// chunk 只做「置脏 + 唤醒回调」,UI 在自己的帧节奏里 take_dirty() 后
// snapshot() 拉一份已合并 run 的网格,天然合帧,主线程零阻塞 IO。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, Notify as _, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, Notifier};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{test::TermSize, Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

/// UI 侧无关 alacritty 类型的颜色表示:Default 用主题前景/背景,
/// Palette 是 ANSI 0-15,Indexed 是 256 色表,Rgb 是真彩
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TermColor {
    Default,
    Palette(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// 一段同样式的连续文本(snapshot 时已按样式合并,UI 直接画 run 不画单格)
#[derive(Clone, Debug)]
pub struct TermRun {
    pub text: String,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub inverse: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TermSnapshot {
    pub rows: Vec<Vec<TermRun>>,
    /// 光标 (可视行, 列);回看历史(display_offset>0)时光标不在视口内为 None
    pub cursor: Option<(usize, usize)>,
    pub display_offset: usize,
}

fn map_color(color: AnsiColor) -> TermColor {
    match color {
        AnsiColor::Named(nc) => match nc {
            NamedColor::Foreground
            | NamedColor::Background
            | NamedColor::Cursor
            | NamedColor::BrightForeground
            | NamedColor::DimForeground => TermColor::Default,
            // Dim* 折回普通色;to_bright 不动已是 0-15 的值
            other => {
                let base = if (other as usize) > NamedColor::BrightWhite as usize {
                    other.to_bright()
                } else {
                    other
                };
                TermColor::Palette((base as usize).min(15) as u8)
            }
        },
        AnsiColor::Indexed(ix) if ix < 16 => TermColor::Palette(ix),
        AnsiColor::Indexed(ix) => TermColor::Indexed(ix),
        AnsiColor::Spec(rgb) => TermColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// alacritty EventListener → 置脏 + 唤醒。PtyWrite(终端回报序列,如光标
/// 位置应答)必须立刻回灌 PTY,经 EventLoopSender 直发,不绕 UI。
#[derive(Clone)]
struct EventProxy {
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    waker: Arc<dyn Fn() + Send + Sync>,
    loop_tx: Arc<FairMutex<Option<EventLoopSender>>>,
}

impl EventProxy {
    fn mark_dirty(&self) {
        // false→true 的边沿才唤醒:UI 没消费前的后续输出不再打扰主线程
        if !self.dirty.swap(true, Ordering::AcqRel) {
            (self.waker)();
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                if let Some(tx) = self.loop_tx.lock().as_ref() {
                    let _ = tx.send(Msg::Input(text.into_bytes().into()));
                }
            }
            Event::Exit | Event::ChildExit(_) => {
                self.exited.store(true, Ordering::Release);
                self.mark_dirty();
            }
            // ColorRequest/TextAreaSizeRequest/Clipboard*:v1 不应答(罕见查询,
            // 不应答仅查询方收不到回报,不影响正常输出)
            _ => self.mark_dirty(),
        }
    }
}

pub struct TerminalSession {
    term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    dirty: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    size: FairMutex<(u16, u16)>, // (cols, rows)
}

impl TerminalSession {
    /// 起一个交互 shell(用户默认 shell,-l 行为由 PTY 自带)。waker 在
    /// PTY 线程被调,必须只做非阻塞投递(如 unbounded channel send)。
    pub fn spawn(
        cwd: String,
        cols: u16,
        rows: u16,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let cols = cols.max(2);
        let rows = rows.max(2);
        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        let options = tty::Options {
            shell: None,
            working_directory: Some(cwd.into()),
            drain_on_exit: false,
            env,
        };
        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 8,
            cell_height: 16,
        };
        let pty = tty::new(&options, window_size, 0).map_err(|e| format!("PTY 启动失败: {}", e))?;

        let dirty = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let loop_tx = Arc::new(FairMutex::new(None));
        let proxy = EventProxy {
            dirty: dirty.clone(),
            exited: exited.clone(),
            waker,
            loop_tx: loop_tx.clone(),
        };

        let term_size = TermSize::new(cols as usize, rows as usize);
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &term_size,
            proxy.clone(),
        )));

        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .map_err(|e| format!("终端事件循环启动失败: {}", e))?;
        let sender = event_loop.channel();
        *loop_tx.lock() = Some(sender.clone());
        let _io_thread = event_loop.spawn();

        Ok(Self {
            term,
            notifier: Notifier(sender),
            dirty,
            exited,
            size: FairMutex::new((cols, rows)),
        })
    }

    /// 用户输入(已转义的字节序列)写入 PTY
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        self.notifier.notify(bytes.into());
    }

    /// 粘贴:应用开了 bracketed-paste(vim/现代 shell)时按协议包裹,
    /// 否则裸写——对齐旧版 xterm.paste() 的语义,防止粘贴内容被当按键流执行
    pub fn paste(&self, text: &str) {
        let bracketed = self
            .term
            .lock()
            .mode()
            .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
        if bracketed {
            let mut bytes = Vec::with_capacity(text.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.write(bytes);
        } else {
            self.write(text.as_bytes().to_vec());
        }
    }

    /// UI 布局变化时调:PTY 与 Term 双侧同步改尺寸
    pub fn resize(&self, cols: u16, rows: u16) {
        let cols = cols.max(2);
        let rows = rows.max(2);
        {
            let mut size = self.size.lock();
            if *size == (cols, rows) {
                return;
            }
            *size = (cols, rows);
        }
        let _ = self.notifier.0.send(Msg::Resize(WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: 8,
            cell_height: 16,
        }));
        self.term
            .lock()
            .resize(TermSize::new(cols as usize, rows as usize));
        self.dirty.store(true, Ordering::Release);
    }

    /// 滚动回看(行数,正=向上回看)
    pub fn scroll(&self, delta: i32) {
        self.term.lock().scroll_display(Scroll::Delta(delta));
        self.dirty.store(true, Ordering::Release);
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
        self.dirty.store(true, Ordering::Release);
    }

    /// UI 帧首调:有脏才值得 snapshot
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// 拉当前视口:逐行把同样式格子合并成 run。锁内只做拷贝,不做布局。
    pub fn snapshot(&self) -> TermSnapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let cursor_point = content.cursor.point;

        let mut rows: Vec<Vec<TermRun>> = Vec::new();
        let mut current_line: i32 = i32::MIN;
        let mut row: Vec<TermRun> = Vec::new();
        for indexed in content.display_iter {
            let line = indexed.point.line.0;
            if line != current_line {
                if current_line != i32::MIN {
                    rows.push(std::mem::take(&mut row));
                }
                current_line = line;
            }
            let cell = &indexed.cell;
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let fg = map_color(cell.fg);
            let bg = map_color(cell.bg);
            let bold = cell.flags.contains(Flags::BOLD);
            let inverse = cell.flags.contains(Flags::INVERSE);
            let ch = if cell.flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            match row.last_mut() {
                Some(run)
                    if run.fg == fg && run.bg == bg && run.bold == bold && run.inverse == inverse =>
                {
                    run.text.push(ch);
                }
                _ => row.push(TermRun {
                    text: ch.to_string(),
                    fg,
                    bg,
                    bold,
                    inverse,
                }),
            }
        }
        if current_line != i32::MIN {
            rows.push(row);
        }

        // 视口首行的 grid 行号 = -display_offset;光标 grid 行号转视口行号
        let viewport_row = cursor_point.line.0 + display_offset as i32;
        let cursor = if (0..rows.len() as i32).contains(&viewport_row) {
            Some((viewport_row as usize, cursor_point.column.0))
        } else {
            None
        };

        TermSnapshot {
            rows,
            cursor,
            display_offset,
        }
    }

    /// 主动关停(窗口关闭/标签关闭):通知事件循环退出,shell 收 SIGHUP
    pub fn shutdown(&self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
        self.exited.store(true, Ordering::Release);
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn snapshot_text(snap: &TermSnapshot) -> String {
        snap.rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn spawn_echo_and_snapshot() {
        let woken = Arc::new(AtomicBool::new(false));
        let woken2 = woken.clone();
        let session = TerminalSession::spawn(
            std::env::temp_dir().to_string_lossy().to_string(),
            80,
            24,
            Arc::new(move || {
                woken2.store(true, Ordering::Release);
            }),
        )
        .expect("spawn shell");

        session.write(b"printf 'nib_term_ok\\n'\r".to_vec());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if session.take_dirty() {
                let snap = session.snapshot();
                let text = snapshot_text(&snap);
                if text.contains("nib_term_ok") {
                    assert!(woken.load(Ordering::Acquire), "waker 应被唤起过");
                    assert!(snap.cursor.is_some(), "底部视口应有光标");
                    break;
                }
            }
            assert!(Instant::now() < deadline, "10s 内没等到 shell 输出");
            std::thread::sleep(Duration::from_millis(30));
        }
        session.shutdown();
    }

    #[test]
    fn resize_changes_viewport_rows() {
        let session = TerminalSession::spawn(
            std::env::temp_dir().to_string_lossy().to_string(),
            80,
            24,
            Arc::new(|| {}),
        )
        .expect("spawn shell");
        assert_eq!(session.snapshot().rows.len(), 24);
        session.resize(60, 10);
        assert_eq!(session.snapshot().rows.len(), 10);
        session.shutdown();
    }
}
