// Nib 原生工作台(M1):Warm Earth 主题 + 标签页 + quick-open + 会话恢复 + 脏标记。
// 性能纪律(RFC v2 §5):主线程零阻塞 IO——目录遍历/读写文件全部经 nib-core
// 自持 runtime,结果回主线程更新实体;异步回灌一律带陈旧守卫。

mod quick_open;
mod session;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState, TabSize},
    list::ListItem,
    tab::{Tab, TabBar},
    tree::{tree, TreeItem, TreeState},
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable as _, Theme, ThemeMode, ThemeRegistry,
    TitleBar,
};

use quick_open::{QuickOpen, QuickOpenEvent};

actions!(
    nib,
    [SaveFile, CloseTab, ToggleQuickOpen, PaletteUp, PaletteDown, PaletteDismiss]
);

/// 双击 Shift 的判定窗口(对齐旧版 quick-open 习惯)
const DOUBLE_SHIFT_WINDOW: Duration = Duration::from_millis(400);

/// 文件扩展名 → tree-sitter 语言名(gpui-component LanguageRegistry 的命名)
fn language_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "rs" => "rust",
        "java" => "java",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" | "jsx" => "typescript",
        "json" => "json",
        "md" => "markdown",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "html" => "html",
        "css" => "css",
        "xml" => "xml",
        "sh" | "zsh" | "bash" => "bash",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        _ => "text",
    }
}

fn file_node_to_tree_item(node: &nib_core::fs::FileNode) -> TreeItem {
    let item = TreeItem::new(node.path.clone(), node.name.clone());
    match &node.children {
        Some(children) => item.children(children.iter().map(file_node_to_tree_item)),
        None => item,
    }
}

struct OpenTab {
    path: PathBuf,
    title: SharedString,
    lang: &'static str,
    dirty: bool,
    editor: Entity<InputState>,
    _change_sub: Subscription,
}

struct Workbench {
    focus_handle: FocusHandle,
    window_handle: AnyWindowHandle,
    project_root: PathBuf,
    project_name: SharedString,
    tree_state: Entity<TreeState>,
    tabs: Vec<OpenTab>,
    active_tab: Option<usize>,
    /// 全项目文件清单缓存(quick-open 用;core runtime 预载)
    all_files: Arc<Vec<String>>,
    palette: Option<Entity<QuickOpen>>,
    _palette_sub: Option<Subscription>,
    status: SharedString,
    /// 主线程停顿哨兵计数(>32ms 漂移即记,可举证不凭感觉)
    stall_count: usize,
    last_shift: Option<Instant>,
    prev_modifiers: Modifiers,
}

impl Workbench {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let arg_root = std::env::args().nth(1).map(PathBuf::from);
        let root = arg_root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let tree_state = cx.new(|cx| TreeState::new(cx));

        // 树的选中变化(点击/键盘)→ 叶子节点即打开文件
        cx.observe(&tree_state, |this: &mut Workbench, state, cx| {
            let target = {
                let state = state.read(cx);
                state.selected_entry().and_then(|entry| {
                    if entry.is_folder() {
                        None
                    } else {
                        Some(PathBuf::from(entry.item().id.to_string()))
                    }
                })
            };
            if let Some(path) = target {
                this.open_file(path, cx);
            }
        })
        .detach();

        Self::start_stall_sentinel(cx);

        // 空窗口也要有焦点锚点,否则 Cmd+P/双击Shift 的按键分发没有落点
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let mut this = Self {
            focus_handle,
            window_handle: window.window_handle(),
            project_root: root.clone(),
            project_name: "".into(),
            tree_state,
            tabs: Vec::new(),
            active_tab: None,
            all_files: Arc::new(Vec::new()),
            palette: None,
            _palette_sub: None,
            status: "".into(),
            stall_count: 0,
            last_shift: None,
            prev_modifiers: Modifiers::default(),
        };
        this.load_project(root, cx);

        // 没传路径参数 → 恢复上次会话(项目 + 打开的标签)
        if arg_root.is_none() {
            cx.spawn(async move |weak, cx| {
                let Some(sess) = session::load().await else {
                    return;
                };
                let Some(project) = sess.projects.get(sess.active_project_index).cloned() else {
                    return;
                };
                let _ = weak.update(cx, |this, cx| {
                    let root = PathBuf::from(&project.path);
                    if root.exists() {
                        this.load_project(root, cx);
                        this.restore_tabs(
                            project.open_files.clone(),
                            project.active_file.clone(),
                            cx,
                        );
                    }
                });
            })
            .detach();
        }

        this
    }

    /// 切换/加载项目:重置树与文件清单(均在 core runtime 上跑,带陈旧守卫)
    fn load_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.project_root = root.clone();
        self.project_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string())
            .into();
        self.status = root.display().to_string().into();

        let tree_for_load = self.tree_state.clone();
        let root_str = root.to_string_lossy().to_string();
        let guard_root = root.clone();
        cx.spawn(async move |weak, cx| {
            let result = nib_core::fs::read_dir_tree(root_str, Some(12)).await;
            if let Ok(node) = result {
                // 陈旧守卫:期间切了项目就丢弃
                let still_current = weak
                    .read_with(cx, |this, _| this.project_root == guard_root)
                    .unwrap_or(false);
                if !still_current {
                    return;
                }
                let items = match &node.children {
                    Some(children) => children.iter().map(file_node_to_tree_item).collect(),
                    None => vec![file_node_to_tree_item(&node)],
                };
                tree_for_load.update(cx, |state, cx| state.set_items(items, cx));
            }
        })
        .detach();

        // quick-open 文件清单预载
        let files_root = root.to_string_lossy().to_string();
        let guard_root = root;
        cx.spawn(async move |weak, cx| {
            if let Ok(mut files) = nib_core::search::list_all_files(files_root).await {
                files.sort();
                let _ = weak.update(cx, |this, cx| {
                    if this.project_root == guard_root {
                        this.all_files = Arc::new(files);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// 会话恢复:按保存顺序逐个读盘建标签(单任务串行,保证标签顺序确定)
    fn restore_tabs(&mut self, paths: Vec<String>, active: Option<String>, cx: &mut Context<Self>) {
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            for p in paths {
                let content = nib_core::fs::read_file(p.clone()).await;
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if let Ok(text) = content {
                            this.insert_tab(PathBuf::from(&p), text, window, cx);
                        }
                    });
                });
            }
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if let Some(active) = active {
                        let active = PathBuf::from(active);
                        if let Some(ix) = this.tabs.iter().position(|t| t.path == active) {
                            this.activate_tab(ix, window, cx);
                        }
                    } else if !this.tabs.is_empty() {
                        this.activate_tab(this.tabs.len() - 1, window, cx);
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 帧时/主线程停顿哨兵(RFC v2 §5.6):每 100ms 一个心跳回主线程,
    /// 漂移 >32ms 视为一次可感知停顿,记证据到 stderr + 状态栏计数。
    fn start_stall_sentinel(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            const BEAT: Duration = Duration::from_millis(100);
            const BUDGET: Duration = Duration::from_millis(32);
            let mut last = Instant::now();
            loop {
                cx.background_executor().timer(BEAT).await;
                let alive = this.update(cx, |this, cx| {
                    let now = Instant::now();
                    let drift = now.duration_since(last).saturating_sub(BEAT);
                    if drift > BUDGET {
                        this.stall_count += 1;
                        eprintln!(
                            "[nib-sentinel] 主线程停顿 ~{}ms(第 {} 次)",
                            drift.as_millis(),
                            this.stall_count
                        );
                        cx.notify();
                    }
                    last = now;
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// 建标签(主线程,需 window 构造编辑器);已存在则返回既有下标
    fn insert_tab(
        &mut self,
        path: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        if let Some(ix) = self.tabs.iter().position(|t| t.path == path) {
            return ix;
        }
        let lang = language_for(&path.to_string_lossy());
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(lang)
                .multi_line(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    ..Default::default()
                })
                .default_value(text)
        });
        // 编辑即脏:订阅 Change 给标签点脏标记
        let editor_for_sub = editor.clone();
        let change_sub = cx.subscribe(&editor, move |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                if let Some(tab) = this.tabs.iter_mut().find(|t| t.editor == editor_for_sub) {
                    if !tab.dirty {
                        tab.dirty = true;
                        cx.notify();
                    }
                }
            }
        });
        let title: SharedString = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
            .into();
        self.tabs.push(OpenTab {
            path,
            title,
            lang,
            dirty: false,
            editor,
            _change_sub: change_sub,
        });
        self.tabs.len() - 1
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = Some(ix);
        let tab = &self.tabs[ix];
        self.status = tab.path.display().to_string().into();
        let handle = tab.editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.persist_session(cx);
        cx.notify();
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // 已开 → 直接切标签(无 window 的调用路径,只改激活态)
        if let Some(ix) = self.tabs.iter().position(|t| t.path == path) {
            if self.active_tab != Some(ix) {
                self.active_tab = Some(ix);
                self.status = path.display().to_string().into();
                self.persist_session(cx);
                cx.notify();
            }
            return;
        }

        self.status = format!("打开 {} …", path.display()).into();
        cx.notify();

        let window_handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            // 读盘在 core runtime
            let content = nib_core::fs::read_file(path.to_string_lossy().to_string()).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    match content {
                        Ok(text) => {
                            let ix = this.insert_tab(path.clone(), text, window, cx);
                            this.activate_tab(ix, window, cx);
                        }
                        Err(err) => {
                            this.status = format!("打开失败: {}", err).into();
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn active(&self) -> Option<&OpenTab> {
        self.active_tab.and_then(|ix| self.tabs.get(ix))
    }

    fn persist_session(&self, _cx: &mut Context<Self>) {
        let sess = session::PersistedSession {
            version: 1,
            projects: vec![session::ProjectSession {
                path: self.project_root.to_string_lossy().to_string(),
                open_files: self
                    .tabs
                    .iter()
                    .map(|t| t.path.to_string_lossy().to_string())
                    .collect(),
                active_file: self.active().map(|t| t.path.to_string_lossy().to_string()),
            }],
            active_project_index: 0,
        };
        session::save(&sess);
    }

    fn on_save(&mut self, _: &SaveFile, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active() else { return };
        let path = tab.path.clone();
        let text = tab.editor.read(cx).value().to_string();
        self.status = format!("保存 {} …", path.display()).into();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = nib_core::fs::write_file(path.to_string_lossy().to_string(), text).await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(()) => {
                        if let Some(tab) = this.tabs.iter_mut().find(|t| t.path == path) {
                            tab.dirty = false;
                        }
                        format!("已保存 {}", path.display()).into()
                    }
                    Err(err) => format!("保存失败: {}", err).into(),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_tab else { return };
        self.tabs.remove(ix);
        self.active_tab = if self.tabs.is_empty() {
            window.focus(&self.focus_handle, cx);
            None
        } else {
            Some(ix.min(self.tabs.len() - 1))
        };
        self.persist_session(cx);
        cx.notify();
    }

    // ---- quick-open ----

    fn on_toggle_quick_open(
        &mut self,
        _: &ToggleQuickOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_palette(window, cx);
    }

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        let root = self.project_root.clone();
        let files = self.all_files.clone();
        let palette = cx.new(|cx| QuickOpen::new(root, files, window, cx));
        palette.update(cx, |p, cx| p.focus(window, cx));
        let sub = cx.subscribe_in(
            &palette,
            window,
            |this: &mut Self, _, event: &QuickOpenEvent, window, cx| match event {
                QuickOpenEvent::Open(path) => {
                    let path = path.clone();
                    this.close_palette(window, cx);
                    this.open_file(path, cx);
                }
            },
        );
        self.palette = Some(palette);
        self._palette_sub = Some(sub);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self._palette_sub = None;
        let handle = match self.active() {
            Some(tab) => tab.editor.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        };
        window.focus(&handle, cx);
        cx.notify();
    }

    fn on_palette_up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &self.palette {
            p.update(cx, |p, cx| p.move_selection(-1, cx));
        }
    }

    fn on_palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &self.palette {
            p.update(cx, |p, cx| p.move_selection(1, cx));
        }
    }

    fn on_palette_dismiss(
        &mut self,
        _: &PaletteDismiss,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_palette(window, cx);
    }

    /// 双击 Shift 开 quick-open(旧版习惯)。检测「shift 单独按下」的上升沿。
    fn on_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let m = event.modifiers;
        let shift_only = m.shift && !m.control && !m.alt && !m.platform && !m.function;
        let was_shift = self.prev_modifiers.shift;
        self.prev_modifiers = m;

        if shift_only && !was_shift {
            let now = Instant::now();
            if let Some(last) = self.last_shift {
                if now.duration_since(last) < DOUBLE_SHIFT_WINDOW {
                    self.last_shift = None;
                    self.toggle_palette(window, cx);
                    return;
                }
            }
            self.last_shift = Some(now);
        }
    }

    fn render_tree_item(
        ix: usize,
        entry: &gpui_component::tree::TreeEntry,
        _: bool,
        _: &mut Window,
        _: &mut App,
    ) -> ListItem {
        let item = entry.item();
        let icon = if !entry.is_folder() {
            IconName::File
        } else if entry.is_expanded() {
            IconName::FolderOpen
        } else {
            IconName::Folder
        };
        ListItem::new(ix)
            .pl(px(8.) + px(14.) * entry.depth() as f32)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(icon).small())
                    .child(item.label.clone()),
            )
    }
}

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_lang = self.active().map(|t| t.lang).unwrap_or("");
        let title: SharedString = match self.active() {
            Some(tab) => format!("{} — {}", self.project_name, tab.title).into(),
            None => self.project_name.clone(),
        };

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().background)
            .key_context("Workbench")
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_toggle_quick_open))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .child(TitleBar::new().child(div().text_sm().child(title)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w(px(260.))
                            .h_full()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .child(tree(&self.tree_state, Self::render_tree_item)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .when(!self.tabs.is_empty(), |this| {
                                this.child(
                                    TabBar::new("tabs")
                                        .w_full()
                                        .underline()
                                        .selected_index(self.active_tab.unwrap_or(0))
                                        .on_click(cx.listener(|this, ix: &usize, window, cx| {
                                            this.activate_tab(*ix, window, cx);
                                        }))
                                        .children(self.tabs.iter().map(|t| {
                                            let label: SharedString = if t.dirty {
                                                format!("● {}", t.title).into()
                                            } else {
                                                t.title.clone()
                                            };
                                            Tab::new().label(label)
                                        })),
                                )
                            })
                            .child(div().flex_1().min_h_0().map(|this| {
                                match self.active() {
                                    Some(tab) => this.child(
                                        Input::new(&tab.editor)
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(cx.theme().mono_font_size)
                                            .size_full(),
                                    ),
                                    None => this.child(
                                        v_flex()
                                            .size_full()
                                            .items_center()
                                            .justify_center()
                                            .gap_2()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{} — Nib (native)", self.project_name))
                                            .child(
                                                "双击 Shift / Cmd+P 快速打开;Cmd+S 保存;Cmd+W 关标签",
                                            ),
                                    ),
                                }
                            })),
                    ),
            )
            .child(
                h_flex()
                    .h(px(26.))
                    .px_2()
                    .items_center()
                    .gap_3()
                    .bg(cx.theme().status_bar)
                    .border_t_1()
                    .border_color(cx.theme().status_bar_border)
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(self.status.clone()),
                    )
                    .when(!active_lang.is_empty(), |this| this.child(active_lang))
                    .when(self.stall_count > 0, |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("卡顿 ×{}", self.stall_count)),
                        )
                    }),
            )
            .when_some(self.palette.clone(), |this, palette| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .key_context("QuickOpen")
                        .on_action(cx.listener(Self::on_palette_up))
                        .on_action(cx.listener(Self::on_palette_down))
                        .on_action(cx.listener(Self::on_palette_dismiss))
                        .child(div().mt(px(110.)).child(palette)),
                )
            })
    }
}

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        // Warm Earth:旧 Nib(webview 版)的 cobalt 色板逐项移植(src/styles/main.css
        // :root 单一真源 + editor-theme.ts 语法配色),保证重构前后外观一致
        if let Err(err) = ThemeRegistry::global_mut(cx)
            .load_themes_from_str(include_str!("../themes/warm-earth.json"))
        {
            eprintln!("[nib] 加载 Warm Earth 主题失败,退回默认暗色: {}", err);
        }
        let warm_earth = ThemeRegistry::global(cx)
            .themes()
            .get(&SharedString::from("Warm Earth"))
            .cloned();
        if let Some(config) = warm_earth {
            Theme::global_mut(cx).apply_config(&config);
        }

        cx.bind_keys([
            KeyBinding::new("cmd-s", SaveFile, Some("Workbench")),
            KeyBinding::new("cmd-w", CloseTab, Some("Workbench")),
            KeyBinding::new("cmd-p", ToggleQuickOpen, Some("Workbench")),
            KeyBinding::new("up", PaletteUp, Some("QuickOpen")),
            KeyBinding::new("down", PaletteDown, Some("QuickOpen")),
            KeyBinding::new("escape", PaletteDismiss, Some("QuickOpen")),
        ]);

        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| Workbench::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .expect("Failed to open window");
        })
        .detach();
    });
}
