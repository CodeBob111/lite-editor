// 终端面板(M4,对齐旧版 terminal-panel 主链):底部 240px,真 PTY shell。
// 拉模型:PTY 线程只「置脏+唤醒」(unbounded channel),本面板收到唤醒后
// take_dirty()→snapshot() 拉一帧已合并 run 的网格,8ms 节流,主线程零阻塞。
// 配色对齐旧版 xterm theme(bg #0d1017 / cursor #3b82f6 / ANSI 8 色)。

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::terminal::{TermColor, TermSnapshot, TerminalSession};

const FONT_SIZE: f32 = 13.;
const LINE_H: f32 = 17.;
/// 底部面板总高(main.rs 的插槽与本面板 rows 推算共用此单源)
pub const PANEL_HEIGHT: f32 = 220.;
const HEADER_H: f32 = 24.;
const PAD_V: f32 = 8.;
/// 旧版 xterm 主题的 ANSI 0-7;8-15 v1 复用同色(亮黑除外)
const PALETTE: [u32; 16] = [
    0x1b2230, 0xff7b72, 0x3fb950, 0xd29922, 0x79c0ff, 0xc699ff, 0x39c5cf, 0xd4dde8,
    0x4d5870, 0xff7b72, 0x3fb950, 0xd29922, 0x79c0ff, 0xc699ff, 0x39c5cf, 0xffffff,
];
const TERM_BG: u32 = 0x0d1017;
const TERM_FG: u32 = 0xc9d3e0;
const TERM_CURSOR: u32 = 0x3b82f6;

fn hex(v: u32) -> Hsla {
    rgb(v).into()
}

/// xterm 256 色表 16-255(16-231 6×6×6 立方,232-255 灰阶)
fn indexed_color(ix: u8) -> Hsla {
    if ix < 16 {
        return hex(PALETTE[ix as usize]);
    }
    if ix >= 232 {
        let v = 8 + (ix as u32 - 232) * 10;
        return hex((v << 16) | (v << 8) | v);
    }
    let ix = ix as u32 - 16;
    let step = |n: u32| if n == 0 { 0 } else { 55 + n * 40 };
    let r = step(ix / 36);
    let g = step((ix / 6) % 6);
    let b = step(ix % 6);
    hex((r << 16) | (g << 8) | b)
}

fn resolve_fg(c: TermColor) -> Hsla {
    match c {
        TermColor::Default => hex(TERM_FG),
        TermColor::Palette(ix) => hex(PALETTE[(ix as usize).min(15)]),
        TermColor::Indexed(ix) => indexed_color(ix),
        TermColor::Rgb(r, g, b) => hex(((r as u32) << 16) | ((g as u32) << 8) | b as u32),
    }
}

/// 背景:Default 透出面板底色(None 不画),非默认才画色块
fn resolve_bg(c: TermColor) -> Option<Hsla> {
    match c {
        TermColor::Default => None,
        other => Some(resolve_fg(other)),
    }
}

/// 按键 → PTY 字节序列。cmd 组合不吃(留给应用快捷键),返回 None 时事件继续冒泡
fn keystroke_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = ks.modifiers;
    if m.platform {
        return None;
    }
    let seq: Vec<u8> = match ks.key.as_str() {
        "enter" => b"\r".to_vec(),
        "backspace" => vec![0x7f],
        "delete" => b"\x1b[3~".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        key => {
            if m.control {
                // ctrl-a..z → 0x01..0x1a;ctrl-space → NUL
                let c = key.chars().next()?;
                if key == "space" {
                    vec![0x00]
                } else if key.len() == 1 && c.is_ascii_alphabetic() {
                    vec![c.to_ascii_lowercase() as u8 - b'a' + 1]
                } else {
                    return None;
                }
            } else {
                let kc = ks.key_char.as_ref()?;
                let mut v = Vec::new();
                if m.alt {
                    v.push(0x1b);
                }
                v.extend(kc.as_bytes());
                v
            }
        }
    };
    Some(seq)
}

use nib_core::text::{byte_range_to_utf16, utf16_len, utf16_to_byte};

/// 单个终端会话(对齐旧版 terminal-tabs:每会话一个标签,可多开)
struct TermTab {
    id: u64,
    name: SharedString,
    session: Arc<TerminalSession>,
    snap: TermSnapshot,
    exited: bool,
    grid: (u16, u16),
}

pub struct TerminalPanel {
    focus_handle: FocusHandle,
    project_root: PathBuf,
    tabs: Vec<TermTab>,
    active: usize,
    next_id: u64,
    /// 等宽字宽缓存(字体与字号固定,首帧实测一次即可)
    cell_w: Option<Pixels>,
    /// 右侧占位宽(Astore 右侧栏开启时为其宽度),列数推算要扣掉
    right_inset: f32,
    /// 终端内容区实际可用高(由 Workbench 按拖动后的面板高推入);grid 行数按它算,
    /// 不再用固定 PANEL_HEIGHT——否则拖高面板时网格仍只 ~12 行,TUI(如 Claude Code)展示不全。
    panel_height: f32,
    status: SharedString,
    /// 最近一次终端工作(收到 PTY 输出重建网格)的标签+时刻;卡顿哨兵据此归因
    last_op: Option<(SharedString, Instant)>,
    /// 输入法预编辑串(合成中的未提交文字,如拼音/候选)。提交后清空、写入 PTY。
    /// 全局单一合成(IME 一次只在聚焦控件上合成),故放面板而非每标签。
    ime_marked: String,
    /// CC 在本项目跑完一个回合、用户还没回看终端 → 终端 tab 显红点。聚焦终端即清。
    cc_done: bool,
    /// CC 回合监控句柄(drop 即停);项目变更时重建。
    _cc_watch: Option<nib_core::cc_watch::CcTurnWatcher>,
}

impl TerminalPanel {
    pub fn new(project_root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            project_root,
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            cell_w: None,
            right_inset: 0.,
            panel_height: PANEL_HEIGHT,
            status: "".into(),
            last_op: None,
            ime_marked: String::new(),
            cc_done: false,
            _cc_watch: None,
        };
        this.spawn_session(cx);
        this.start_cc_watch(cx);
        this
    }

    /// 起 CC 回合监控(零配置兜底):watch 项目的 CC 会话目录,回合结束 → cc_done 红点 + dock 角标。
    /// 回调在 notify 线程,经 unbounded channel 投递到 gpui spawn 循环(主线程)再更新 UI。
    fn start_cc_watch(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.unbounded_send(());
        });
        self._cc_watch = nib_core::cc_watch::watch_project_turns(&self.project_root, waker);
        cx.spawn(async move |weak, cx| {
            while rx.next().await.is_some() {
                let alive = weak
                    .update(cx, |this: &mut TerminalPanel, cx| {
                        this.cc_done = true;
                        // 后台时亮 dock 角标(在 Nib 里时靠 cc_done 红点;bump 内部判前台会 no-op)
                        nib_core::dock::bump_badge();
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break; // 面板已销毁
                }
            }
        })
        .detach();
    }

    /// CC 回合完成、未回看 → 终端 tab 显红点(Workbench 读取)。
    pub fn cc_done(&self) -> bool {
        self.cc_done
    }

    fn active_tab(&self) -> Option<&TermTab> {
        self.tabs.get(self.active)
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn set_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        // 当前 shell 不动(它有自己的 cwd);只影响之后的重启
        self.project_root = root;
        // 换项目 → CC 会话目录也换:重建监控、清掉旧项目的红点
        self.cc_done = false;
        self.start_cc_watch(cx);
    }

    pub fn set_right_inset(&mut self, inset: f32) {
        self.right_inset = inset;
    }

    /// Workbench 按拖动后的面板高推入内容区实际高;不 notify(本面板作为子元素随父重渲染,
    /// 下一帧 render 即按新高算 grid 行数)。
    pub fn set_height(&mut self, height: f32) {
        self.panel_height = height;
    }

    /// 最近一次终端工作的标签+时刻(卡顿哨兵跨组件读取归因用)
    pub fn last_op(&self) -> Option<(SharedString, Instant)> {
        self.last_op.clone()
    }

    /// 新开一个会话标签(旧版 + 按钮/首次打开)
    fn spawn_session(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.unbounded_send(());
        });
        match TerminalSession::spawn(
            self.project_root.to_string_lossy().to_string(),
            80,
            12,
            waker,
        ) {
            Ok(session) => {
                self.next_id += 1;
                let id = self.next_id;
                let project = self
                    .project_root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "shell".into());
                self.tabs.push(TermTab {
                    id,
                    name: format!("{} ({})", project, id).into(),
                    session: Arc::new(session),
                    snap: TermSnapshot::default(),
                    exited: false,
                    grid: (80, 12),
                });
                self.active = self.tabs.len() - 1;
                self.status = "".into();
                cx.spawn(async move |weak, cx| {
                    while rx.next().await.is_some() {
                        let Ok(()) = weak.update(cx, |this: &mut TerminalPanel, cx| {
                            this.pull(id, cx);
                        }) else {
                            break; // 面板已销毁,任务退出
                        };
                        // 16ms 节流(~60FPS):大量输出时合帧,不按 PTY chunk 频率刷。
                        // 原 8ms=125FPS 过高,每帧都在主线程锁终端+遍历网格+重建字符串,
                        // 是「终端输出」类卡顿来源;60FPS 对终端足够顺滑且开销减半。
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(16))
                            .await;
                    }
                })
                .detach();
            }
            Err(err) => {
                self.status = format!("终端启动失败: {}", err).into();
            }
        }
        cx.notify();
    }

    fn pull(&mut self, id: u64, cx: &mut Context<Self>) {
        let mut dirtied = None;
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            // 响铃(BEL):CLI 任务跑完/等输入(如 Claude Code)。独立于 take_dirty 取——
            // Bell 虽也置脏,但即便该帧脏已被消费,铃标志仍要单独兑现。Nib 不在前台时
            // bump_badge 才落 Dock 角标(前台判定在内部)。
            if tab.session.take_bell() {
                nib_core::dock::bump_badge();
            }
            if tab.session.take_dirty() {
                tab.snap = tab.session.snapshot();
                tab.exited = tab.session.is_exited();
                dirtied = Some(tab.name.clone());
                cx.notify();
            }
        }
        // 收到 PTY 输出、重建网格 = 终端在主线程的工作量入口;留面包屑供哨兵归因
        if let Some(name) = dirtied {
            self.last_op = Some((format!("终端输出 {name}").into(), Instant::now()));
        }
    }

    fn close_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        tab.session.shutdown();
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.spawn_session(cx); // 面板常驻至少一个会话
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 在终端里敲键 = 已回看 → 清 CC 完成红点
        if self.cc_done {
            self.cc_done = false;
            cx.notify();
        }
        let Some(tab) = self.active_tab() else {
            return;
        };
        if tab.exited {
            return;
        }
        let session = tab.session.clone();
        let session = &session;
        let ks = &event.keystroke;
        // cmd-v 粘贴进终端(bracketed-paste 语义在 core 处理;其余 cmd 组合冒泡)
        if ks.modifiers.platform && ks.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                session.paste(&text);
                session.scroll_to_bottom();
            }
            cx.stop_propagation();
            return;
        }
        if let Some(bytes) = keystroke_bytes(ks) {
            session.write(bytes);
            session.scroll_to_bottom();
            cx.stop_propagation();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.active_tab().map(|t| t.session.clone()) else {
            return;
        };
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / LINE_H,
        };
        let lines = lines.round() as i32;
        if lines != 0 {
            session.scroll(lines);
            session.take_dirty();
            let snap = session.snapshot();
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.snap = snap;
            }
            cx.notify();
        }
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        let ix = self.active;
        if ix < self.tabs.len() {
            let old = self.tabs.remove(ix);
            old.session.shutdown();
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
        }
        self.spawn_session(cx);
    }

    /// 布局变化时由 render 调:active 会话网格尺寸变了才真正 resize
    fn sync_grid(&mut self, cols: u16, rows: u16) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if tab.grid != (cols, rows) {
                tab.grid = (cols, rows);
                tab.session.resize(cols, rows);
            }
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 等宽字宽实测一次后缓存(决定 cols 与光标 x;估错会导致换行/光标错位)
        let cell_w = *self.cell_w.get_or_insert_with(|| {
            let font = font(cx.theme().mono_font_family.clone());
            let font_id = window.text_system().resolve_font(&font);
            window
                .text_system()
                .advance(font_id, px(FONT_SIZE), 'm')
                .map(|s| s.width)
                .unwrap_or(px(7.8))
        });

        // 面板宽 = 视口宽 - 活动栏 - 侧栏 - 右侧栏占位 - 边框;高 = 总高 - 把手 - 留白
        let viewport = window.viewport_size();
        let avail_w = f32::from(viewport.width)
            - crate::ACTIVITY_WIDTH
            - crate::SIDEBAR_WIDTH
            - self.right_inset
            - 10.;
        let cols = ((avail_w / f32::from(cell_w)).floor() as u16).clamp(2, 500);
        let rows =
            (((self.panel_height - HEADER_H - PAD_V) / LINE_H).floor() as u16).clamp(2, 100);
        self.sync_grid(cols, rows);

        let active_ix = self.active;
        let session_tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let selected = ix == active_ix;
                h_flex()
                    .id(("term-tab", ix))
                    .h(px(20.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .flex_none()
                    .rounded(cx.theme().radius)
                    .text_size(px(11.))
                    .when(selected, |s| s.bg(cx.theme().accent))
                    .when(!selected, |s| {
                        s.text_color(cx.theme().muted_foreground)
                            .hover(|s| s.bg(cx.theme().accent))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.active = ix;
                            window.focus(&this.focus_handle, cx);
                            cx.notify();
                        }),
                    )
                    .child(tab.name.clone())
                    .when(tab.exited, |s| {
                        s.child(div().text_color(cx.theme().warning).child("!"))
                    })
                    .child(
                        div()
                            .id(("term-tab-close", ix))
                            .text_color(cx.theme().muted_foreground)
                            .hover(|s| s.text_color(cx.theme().foreground))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.close_session(ix, cx);
                                }),
                            )
                            .child("×"),
                    )
            })
            .collect();

        let (rows_el, cursor, display_offset, exited) = match self.active_tab() {
            Some(tab) => {
                let rows_el: Vec<_> = tab
                    .snap
                    .rows
                    .iter()
                    .map(|row| {
                        h_flex()
                            .h(px(LINE_H))
                            .overflow_hidden()
                            .children(row.iter().map(|run| {
                                let fg = resolve_fg(run.fg);
                                let bg = resolve_bg(run.bg);
                                let (fg, bg) = if run.inverse {
                                    (bg.unwrap_or(hex(TERM_BG)), Some(fg))
                                } else {
                                    (fg, bg)
                                };
                                div()
                                    .whitespace_nowrap()
                                    .text_color(fg)
                                    .when_some(bg, |s, b| s.bg(b))
                                    .when(run.bold, |s| s.font_weight(FontWeight::BOLD))
                                    .child(SharedString::from(run.text.clone()))
                            }))
                    })
                    .collect();
                (
                    rows_el,
                    tab.snap.cursor,
                    tab.snap.display_offset,
                    tab.exited,
                )
            }
            None => (Vec::new(), None, 0, false),
        };

        v_flex()
            .size_full()
            .bg(hex(TERM_BG))
            .child(
                h_flex()
                    .h(px(HEADER_H))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(div().mr_1().child("终端"))
                    .children(session_tabs)
                    .child(
                        div()
                            .id("term-new")
                            .w(px(18.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(cx.theme().radius)
                            .hover(|s| s.bg(cx.theme().accent))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.spawn_session(cx);
                                    window.focus(&this.focus_handle, cx);
                                }),
                            )
                            .child("+"),
                    )
                    .when(display_offset > 0, |s| {
                        s.child(format!("回看 -{} 行", display_offset))
                    })
                    .when(!self.status.is_empty(), |s| {
                        s.child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(self.status.clone()),
                        )
                    })
                    .child(div().flex_1())
                    .when(exited, |s| {
                        s.child(
                            div()
                                .id("term-restart")
                                .px_1()
                                .rounded(cx.theme().radius)
                                .text_color(cx.theme().warning)
                                .hover(|s| s.bg(cx.theme().accent))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.restart(cx)),
                                )
                                .child("进程已退出 — 点击重启"),
                        )
                    }),
            )
            .child(
                div()
                    .id("term-grid")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .px(px(5.))
                    .py(px(4.))
                    .overflow_hidden()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(FONT_SIZE))
                    .line_height(px(LINE_H))
                    .track_focus(&self.focus_handle)
                    .on_key_down(cx.listener(Self::on_key))
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            window.focus(&this.focus_handle, cx);
                            this.cc_done = false; // 点开终端 = 已回看,清红点
                        }),
                    )
                    .child(v_flex().children(rows_el))
                    .when_some(cursor, |s, (row, col)| {
                        // 块状光标覆盖层:等宽网格坐标 → 像素
                        s.child(
                            div()
                                .absolute()
                                .top(px(4. + row as f32 * LINE_H))
                                .left(px(5.) + cell_w * col as f32)
                                .w(cell_w)
                                .h(px(LINE_H))
                                .bg(hex(TERM_CURSOR))
                                .opacity(0.55),
                        )
                    })
                    .child({
                        // paint 阶段注册输入法处理器(仅聚焦时生效),让 IME 能在终端里合成。
                        // canvas 不画东西,只借 paint 钩子调 window.handle_input;absolute+size_full
                        // 覆盖网格,其 bounds 即候选窗定位用的 element_bounds。无鼠标监听=不挡点击。
                        let entity = cx.entity();
                        let focus = self.focus_handle.clone();
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| {
                                window.handle_input(
                                    &focus,
                                    ElementInputHandler::new(bounds, entity),
                                    cx,
                                );
                            },
                        )
                        .absolute()
                        .size_full()
                    }),
            )
    }
}

// 终端的输入法接入(EntityInputHandler):让 CJK 输入法在终端里也能合成中文。
// 由 render 里的 canvas 在 paint 时 window.handle_input(ElementInputHandler) 注册。
// ElementInputHandler 的 prefers_ime_for_printable_keys = accepts_text_input(true):
// 中文输入源激活时,可打印键先进 IME 合成(本 impl);纯英文键盘布局(ASCII-capable)
// 由 gpui 的 is_ime_input_source_active() 判定为非 IME,键照旧走 on_key_down/keystroke_bytes
// 原始直达 PTY——故 ASCII 路径不变,只新增 CJK。文档=当前预编辑串(无可编辑缓冲)。
impl EntityInputHandler for TerminalPanel {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let len = self.ime_marked.len();
        let s = utf16_to_byte(&self.ime_marked, range_utf16.start).min(len);
        let e = utf16_to_byte(&self.ime_marked, range_utf16.end).min(len);
        if s > e {
            return None;
        }
        adjusted_range.replace(byte_range_to_utf16(&self.ime_marked, s..e));
        Some(self.ime_marked[s..e].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // 无可编辑文档,插入点落在预编辑串末尾(零宽)
        let end = utf16_len(&self.ime_marked);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if self.ime_marked.is_empty() {
            None
        } else {
            Some(0..utf16_len(&self.ime_marked))
        }
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.ime_marked.is_empty() {
            self.ime_marked.clear();
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // IME 提交(或直接插入):原始字节写入 PTY,不能用 paste(避免 bracketed-paste 包裹);
        // 不本地累积——shell 会回显,留着会双重渲染。
        let sess = self
            .active_tab()
            .filter(|t| !t.exited)
            .map(|t| t.session.clone());
        if let Some(sess) = sess {
            sess.write(text.as_bytes().to_vec());
            sess.scroll_to_bottom();
        }
        self.ime_marked.clear();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 合成中:在预编辑串内按 UTF-16 范围替换;无范围则整体替换(首次合成 / 整串改写)
        let len = self.ime_marked.len();
        let byte_range = match range_utf16 {
            Some(r) => {
                utf16_to_byte(&self.ime_marked, r.start).min(len)
                    ..utf16_to_byte(&self.ime_marked, r.end).min(len)
            }
            None => 0..len,
        };
        self.ime_marked.replace_range(byte_range, new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // 候选窗定位到终端光标所在单元格(element_bounds = 网格 canvas 的屏幕矩形)
        let (row, col) = self.active_tab().and_then(|t| t.snap.cursor)?;
        let cell_w = self.cell_w?;
        let x = element_bounds.origin.x + px(5.) + cell_w * col as f32;
        let y = element_bounds.origin.y + px(4.) + px(LINE_H) * row as f32;
        Some(Bounds {
            origin: point(x, y),
            size: size(cell_w, px(LINE_H)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        // 有活动会话且未退出才收文本——也驱动 prefers_ime,退出的 shell 自然停掉 IME
        self.active_tab().is_some_and(|t| !t.exited)
    }
}
