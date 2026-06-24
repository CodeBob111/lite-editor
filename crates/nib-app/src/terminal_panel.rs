// 终端面板(M4,对齐旧版 terminal-panel 主链):底部 240px,真 PTY shell。
// 拉模型:PTY 线程只「置脏+唤醒」(unbounded channel),本面板收到唤醒后
// take_dirty()→snapshot() 拉一帧已合并 run 的网格,8ms 节流,主线程零阻塞。
// 配色对齐旧版 xterm theme(bg #0d1017 / cursor #3b82f6 / ANSI 8 色)。

use std::cell::Cell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
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
    project_root: PathBuf,
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
    /// 最近一次 CC 完成时刻:文件监控与 hook 两路会为同一回合各报一次,据此 ~5s 内去重防角标 +2。
    cc_done_at: Option<Instant>,
    /// CC 回合监控句柄(jsonl 文件监控,per-project,drop 即停;项目变更时重建)。
    _cc_watch: Option<nib_core::cc_watch::CcTurnWatcher>,
    /// CC Stop hook 事件目录监控句柄(全局,new 时起一次,不随项目重建)。
    _cc_event_watch: Option<nib_core::cc_watch::CcTurnWatcher>,
    /// 终端网格在窗口中的 bounds(canvas prepaint 时写入);鼠标像素 → 单元格用。
    /// 用 Rc<Cell> 而非字段直写:paint/prepaint 闭包里 entity.update 存不进来,Cell::set 必生效。
    grid_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 正在鼠标拖选(mouse down 起、move 中更新、松开止)。
    selecting: bool,
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
            panel_height: PANEL_HEIGHT,
            status: "".into(),
            last_op: None,
            ime_marked: String::new(),
            cc_done: false,
            cc_done_at: None,
            _cc_watch: None,
            _cc_event_watch: None,
            grid_bounds: Rc::new(Cell::new(None)),
            selecting: false,
        };
        this.spawn_session(cx);
        this.start_cc_watch(cx);
        this.start_cc_event_watch(cx);
        this
    }

    /// CC 一个回合结束(文件监控 或 hook 任一路触发)→ 红点 + dock 角标。两路会为同一回合各报
    /// 一次,~5s 内去重(只第一次 bump 角标),但红点幂等可重复设。
    fn on_cc_done(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let dup = self
            .cc_done_at
            .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(5));
        self.cc_done_at = Some(now);
        self.cc_done = true;
        if !dup {
            // 后台时亮 dock 角标(在 Nib 里时靠红点;bump 内部判前台会 no-op)
            nib_core::dock::bump_badge();
        }
        cx.notify();
    }

    /// 起 CC Stop hook 事件目录监控(精确路,全局):hook 写事件 → on_cc_done。
    fn start_cc_event_watch(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = nib_core::cc_hook::cc_events_dir() else {
            return;
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.unbounded_send(());
        });
        self._cc_event_watch = nib_core::cc_watch::watch_event_dir(&dir, waker);
        cx.spawn(async move |weak, cx| {
            while rx.next().await.is_some() {
                if weak
                    .update(cx, |this: &mut TerminalPanel, cx| this.on_cc_done(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
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
                if weak
                    .update(cx, |this: &mut TerminalPanel, cx| this.on_cc_done(cx))
                    .is_err()
                {
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

    fn active_project_tab_index(&self) -> Option<usize> {
        if self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.project_root == self.project_root)
        {
            Some(self.active)
        } else {
            self.tabs
                .iter()
                .position(|tab| tab.project_root == self.project_root)
        }
    }

    fn active_project_tab(&self) -> Option<&TermTab> {
        self.active_project_tab_index()
            .and_then(|ix| self.tabs.get(ix))
    }

    fn project_name(path: &PathBuf) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    }

    /// 窗口坐标 → 可视(行, 列)。grid_bounds 是网格 canvas 的 bounds(= term-grid padding box
    /// 原点),内容从 padding(left 5 / top 4)起,与 cursor 叠层同款偏移。
    fn pos_to_cell(&self, pos: Point<Pixels>) -> Option<(usize, usize)> {
        let bounds = self.grid_bounds.get()?;
        let cell_w = f32::from(self.cell_w?);
        let x = f32::from(pos.x - bounds.origin.x) - 5.;
        let y = f32::from(pos.y - bounds.origin.y) - 4.;
        if x < 0. || y < 0. || cell_w <= 0. {
            return None;
        }
        Some(((y / LINE_H) as usize, (x / cell_w) as usize))
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn set_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.project_root = root.clone();
        if let Some(ix) = self.tabs.iter().position(|tab| tab.project_root == root) {
            self.active = ix;
            self.status = "".into();
        } else {
            self.spawn_session(cx);
        }
        // 换项目 → CC 会话目录也换:重建监控、清掉旧项目的红点
        self.cc_done = false;
        self.start_cc_watch(cx);
        cx.notify();
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
                let project = Self::project_name(&self.project_root);
                self.tabs.push(TermTab {
                    id,
                    name: format!("{} ({})", project, id).into(),
                    project_root: self.project_root.clone(),
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
        // cmd-c:有选区则复制选中文本(无选区不拦,让其冒泡走应用快捷键)
        if ks.modifiers.platform && ks.key == "c" {
            if let Some(text) = session.selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
                return;
            }
        }
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
            // 有按键输入 → 清掉旧选区(与常见终端一致:打字即取消高亮)
            session.clear_selection();
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
        let ix = self.active_project_tab_index().unwrap_or(self.active);
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
        // §G 浮动卡片:终端卡片外有 work 容器左右内边距(8+8)+ 侧栏间隙(8)+ 卡片边框/内距(~10),
        // 共约 34px。列数按真实可用宽算,否则 grid 过宽、文字溢出终端卡片右边框。
        // (侧栏宽仍用 const 默认值;用户拖宽侧栏后列数会略偏,属既有限制。)
        let avail_w = f32::from(viewport.width)
            - crate::ACTIVITY_WIDTH
            - crate::SIDEBAR_WIDTH
            - 34.;
        let cols = ((avail_w / f32::from(cell_w)).floor() as u16).clamp(2, 500);
        let rows =
            (((self.panel_height - HEADER_H - PAD_V) / LINE_H).floor() as u16).clamp(2, 100);
        // 把 self.active 对齐到「当前项目实际显示的 tab」。render/restart 用 active_project_tab_index,
        // 但 on_key/on_scroll/sync_grid 用 self.active——两者错位时,打字/滚动/resize 会作用到隐藏的
        // session,显示的终端像冻住、没反应、提示符不刷新。在这里(用 self.active 之前)对齐即根治。
        if let Some(ix) = self.active_project_tab_index() {
            self.active = ix;
        }
        self.sync_grid(cols, rows);

        let active_ix = self.active_project_tab_index();
        let session_tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.project_root == self.project_root)
            .map(|(ix, tab)| {
                let selected = Some(ix) == active_ix;
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

        let (rows_el, cursor, display_offset, exited, selection) = match self.active_project_tab() {
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
                    tab.snap.selection,
                )
            }
            None => (Vec::new(), None, 0, false, None),
        };

        // 选区高亮叠层(可视坐标 → 像素;多行时首行 sc..行尾、中间整行、末行 0..ec)
        let selection_els: Vec<_> = selection
            .map(|(sr, sc, er, ec)| {
                (sr..=er)
                    .map(|r| {
                        let start_c = if r == sr { sc } else { 0 };
                        let end_c = if r == er { ec } else { cols as usize };
                        let w = (end_c.saturating_sub(start_c)) as f32 * f32::from(cell_w);
                        div()
                            .absolute()
                            .top(px(4. + r as f32 * LINE_H))
                            .left(px(5.) + cell_w * start_c as f32)
                            .w(px(w))
                            .h(px(LINE_H))
                            .bg(hex(TERM_CURSOR))
                            .opacity(0.3)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

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
                        cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus_handle, cx);
                            this.cc_done = false; // 点开终端 = 已回看,清红点
                            // 起一个新选区(点击空白即清旧选区);拖动时 on_mouse_move 更新终点
                            if let Some((row, col)) = this.pos_to_cell(ev.position) {
                                if let Some(session) =
                                    this.active_project_tab().map(|t| t.session.clone())
                                {
                                    session.selection_start(row, col);
                                    this.selecting = true;
                                }
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                        if !this.selecting {
                            return;
                        }
                        // 松开左键(可能漏了 mouse_up)→ 结束拖选
                        if ev.pressed_button != Some(MouseButton::Left) {
                            this.selecting = false;
                            return;
                        }
                        if let Some((row, col)) = this.pos_to_cell(ev.position) {
                            if let Some(session) =
                                this.active_project_tab().map(|t| t.session.clone())
                            {
                                session.selection_update(row, col);
                            }
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.selecting = false),
                    )
                    .child(v_flex().children(rows_el))
                    .children(selection_els)
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
                        let grid_bounds = self.grid_bounds.clone();
                        let focus = self.focus_handle.clone();
                        canvas(
                            // prepaint:把网格 bounds 存进 Cell,供鼠标拖选像素→单元格换算
                            move |bounds, _, _| grid_bounds.set(Some(bounds)),
                            move |bounds, _, window, cx| {
                                window.handle_input(
                                    &focus,
                                    ElementInputHandler::new(bounds, entity),
                                    cx,
                                );
                            },
                        )
                        // top/left=0 锚到网格左上角:不给 inset 时 absolute 元素落在「静态位置」
                        // (排在 v_flex 行列表之后),bounds.origin 会被推到网格下方,鼠标像素→单元格全错。
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
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
