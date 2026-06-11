// Nib 原生工作台(M1):Warm Earth 主题 + 标签页 + quick-open + 会话恢复 + 脏标记。
// 性能纪律(RFC v2 §5):主线程零阻塞 IO——目录遍历/读写文件全部经 nib-core
// 自持 runtime,结果回主线程更新实体;异步回灌一律带陈旧守卫。

mod astore_panel;
mod diff_view;
mod file_icons;
mod git_panel;
mod maven_panel;
mod merge_view;
mod recents_view;
mod settings_view;
mod terminal_panel;
mod usages_view;
mod quick_open;
mod search_panel;
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
    tree::{tree, TreeItem, TreeState},
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable as _, Theme, ThemeMode, ThemeRegistry,
    TitleBar,
};

use futures::StreamExt as _;
use diff_view::{DiffView, DiffViewEvent};
use astore_panel::AstorePanel;
use git_panel::{GitPanel, GitPanelEvent, GitPanelMode};
use maven_panel::MavenPanel;
use merge_view::{MergeView, MergeViewEvent};
use quick_open::{QuickOpen, QuickOpenEvent};
use recents_view::{RecentsEvent, RecentsView};
use search_panel::{SearchEvent, SearchPanel};
use settings_view::{SettingsEvent, SettingsView};
use terminal_panel::TerminalPanel;
use usages_view::{UsagesEvent, UsagesView};

actions!(
    nib,
    [
        SaveFile,
        CloseTab,
        ToggleQuickOpen,
        ToggleSearch,
        PaletteUp,
        PaletteDown,
        PaletteDismiss,
        OpenFolder,
        GotoDefinition,
        FindUsages,
        ToggleMdPreview,
        ToggleTerminal,
        ToggleAstore,
        ShowRecentProjects,
        OpenSettings,
        ArthasWatch,
        ArthasTrace,
        ArthasStack,
        ArthasMonitor,
        ArthasTt,
        PaletteConfirm,
        Quit
    ]
);

/// 首帧计时锚点(RFC v2 §5 预算:冷启动首帧 ≤300ms)
static APP_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

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

fn file_node_to_tree_item(
    node: &nib_core::fs::FileNode,
    expanded: &std::collections::HashSet<String>,
) -> TreeItem {
    let item = TreeItem::new(node.path.clone(), node.name.clone())
        .expanded(expanded.contains(&node.path));
    match &node.children {
        Some(children) => {
            item.children(children.iter().map(|c| file_node_to_tree_item(c, expanded)))
        }
        None => item,
    }
}

/// notify 线程 → gpui 主线程的事件桥(EventSink 的 nib-app 实现)
struct ChannelSink(futures::channel::mpsc::UnboundedSender<nib_core::CoreEvent>);

impl nib_core::EventSink for ChannelSink {
    fn emit(&self, event: nib_core::CoreEvent) {
        let _ = self.0.unbounded_send(event);
    }
}

enum Overlay {
    QuickOpen(Entity<QuickOpen>),
    Search(Entity<SearchPanel>),
    Diff(Entity<DiffView>),
    Usages(Entity<UsagesView>),
    Merge(Entity<MergeView>),
    Recents(Entity<RecentsView>),
    Settings(Entity<SettingsView>),
}

/// 标签上限(LRU 淘汰,脏标签豁免)
const MAX_TABS: usize = 30;

/// 侧栏宽度单源(terminal_panel 推算可用列数也用它;旧版 --sidebar-width: 256px)
pub const SIDEBAR_WIDTH: f32 = 256.;
/// 活动栏宽(旧版 --activity-width: 48px)
pub const ACTIVITY_WIDTH: f32 = 48.;
/// Astore 右侧栏宽(旧版 --astore-width: 260px)
pub const ASTORE_WIDTH: f32 = 260.;

struct OpenTab {
    path: PathBuf,
    title: SharedString,
    lang: &'static str,
    dirty: bool,
    last_used: Instant,
    editor: Entity<InputState>,
    _change_sub: Subscription,
    /// 编辑器任何重绘(含光标移动)都触发本体重渲染,状态栏 Ln/Col 才跟手
    _observe_sub: Subscription,
}

#[derive(PartialEq, Clone, Copy)]
enum SidebarView {
    Files,
    Commit,
    Git,
    Maven,
}

impl SidebarView {
    fn title(&self) -> &'static str {
        match self {
            SidebarView::Files => "Explorer",
            SidebarView::Commit => "Commit",
            SidebarView::Git => "Git",
            SidebarView::Maven => "Maven",
        }
    }
}

struct Workbench {
    focus_handle: FocusHandle,
    sidebar_view: SidebarView,
    astore_visible: bool,
    git_panel: Entity<GitPanel>,
    maven_panel: Entity<MavenPanel>,
    astore_panel: Entity<AstorePanel>,
    _git_sub: Subscription,
    window_handle: AnyWindowHandle,
    project_root: PathBuf,
    project_name: SharedString,
    tree_state: Entity<TreeState>,
    tabs: Vec<OpenTab>,
    active_tab: Option<usize>,
    /// 全项目文件清单缓存(quick-open 用;core runtime 预载)
    all_files: Arc<Vec<String>>,
    overlay: Option<Overlay>,
    _overlay_sub: Option<Subscription>,
    /// 无输入框浮层(Usages/Merge/Recents)的焦点锚:不聚焦到浮层容器,
    /// "QuickOpen" 上下文的 ↑↓/Enter/Esc 绑定就不在按键分发路径上
    overlay_focus: FocusHandle,
    watcher: Arc<nib_core::watch::WatcherState>,
    lsp: Arc<nib_core::lsp::LspState>,
    events_sink: Arc<ChannelSink>,
    settings: session::EditorSettings,
    md_preview: bool,
    terminal: Option<Entity<TerminalPanel>>,
    terminal_visible: bool,
    expanded_paths: std::collections::HashSet<String>,
    /// git 状态标记(绝对路径→状态首字母),Explorer 树着色用(对齐旧版)
    git_marks: Arc<std::collections::HashMap<String, char>>,
    status: SharedString,
    /// 主线程停顿哨兵计数(>32ms 漂移即记,可举证不凭感觉)
    stall_count: usize,
    first_frame_logged: bool,
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

        // 树的选中变化(点击/键盘)→ 叶子打开文件;文件夹记录展开态
        // (reload_tree 重建 items 时回放,目录树刷新不再把展开全折叠)
        cx.observe(&tree_state, |this: &mut Workbench, state, cx| {
            enum Sel {
                Open(PathBuf),
                Folder(String, bool),
                None,
            }
            let sel = {
                let state = state.read(cx);
                match state.selected_entry() {
                    Some(entry) if entry.is_folder() => {
                        Sel::Folder(entry.item().id.to_string(), entry.is_expanded())
                    }
                    Some(entry) => Sel::Open(PathBuf::from(entry.item().id.to_string())),
                    None => Sel::None,
                }
            };
            match sel {
                Sel::Open(path) => this.open_file(path, cx),
                Sel::Folder(id, expanded) => {
                    if expanded {
                        this.expanded_paths.insert(id);
                    } else {
                        this.expanded_paths.remove(&id);
                    }
                }
                Sel::None => {}
            }
        })
        .detach();

        let git_panel = cx.new(|cx| GitPanel::new(root.clone(), window, cx));
        let maven_panel = cx.new(|cx| MavenPanel::new(root.clone(), cx));
        let astore_panel = cx.new(|cx| AstorePanel::new(root.clone(), window, cx));
        let git_sub = cx.subscribe(
            &git_panel,
            |this: &mut Workbench, _, event: &GitPanelEvent, cx| match event {
                GitPanelEvent::OpenDiff { rel_path, abs_path } => {
                    this.open_diff(rel_path.clone(), abs_path.clone(), cx)
                }
                GitPanelEvent::OpenMerge { rel_path } => {
                    this.open_merge(rel_path.clone(), cx)
                }
            },
        );

        Self::start_stall_sentinel(cx);

        // 空窗口也要有焦点锚点,否则 Cmd+P/双击Shift 的按键分发没有落点
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        // 文件监听:notify 线程 emit → channel → 主线程 on_core_event
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<nib_core::CoreEvent>();
        let window_handle = window.window_handle();
        cx.spawn(async move |weak, cx| {
            while let Some(event) = rx.next().await {
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    let _ = weak.update(cx, |this: &mut Workbench, cx| {
                        this.on_core_event(event, window, cx);
                    });
                });
            }
        })
        .detach();

        let mut this = Self {
            focus_handle,
            sidebar_view: SidebarView::Files,
            astore_visible: false,
            git_panel,
            maven_panel,
            astore_panel,
            _git_sub: git_sub,
            window_handle: window.window_handle(),
            project_root: root.clone(),
            project_name: "".into(),
            tree_state,
            tabs: Vec::new(),
            active_tab: None,
            all_files: Arc::new(Vec::new()),
            overlay: None,
            _overlay_sub: None,
            overlay_focus: cx.focus_handle(),
            watcher: Arc::new(nib_core::watch::WatcherState::default()),
            lsp: Arc::new(nib_core::lsp::LspState::default()),
            events_sink: Arc::new(ChannelSink(tx)),
            settings: session::EditorSettings::default(),
            md_preview: false,
            terminal: None,
            terminal_visible: false,
            expanded_paths: std::collections::HashSet::new(),
            git_marks: Arc::new(std::collections::HashMap::new()),
            status: "".into(),
            stall_count: 0,
            first_frame_logged: false,
            last_shift: None,
            prev_modifiers: Modifiers::default(),
        };
        let lsp_for_quit = this.lsp.clone();
        cx.on_app_quit(move |_, _| {
            let lsp = lsp_for_quit.clone();
            async move {
                lsp.kill_all();
            }
        })
        .detach();

        this.load_project(root, cx);

        // 加载编辑器偏好(含旧 settings.json 一次性导入),回来后应用到已开标签
        cx.spawn(async move |weak, cx| {
            let settings = session::load_settings().await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    this.settings = settings;
                    for tab in &this.tabs {
                        tab.editor.update(cx, |state, cx| {
                            state.set_soft_wrap(settings.word_wrap, window, cx);
                            state.set_folding(settings.folding, window, cx);
                        });
                    }
                    cx.notify();
                });
            });
        })
        .detach();

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
        let old_root = self.project_root.to_string_lossy().to_string();
        let _ = nib_core::watch::stop_file_watcher(&old_root, &self.watcher);
        if let Err(err) = nib_core::watch::start_file_watcher(
            root.to_string_lossy().to_string(),
            self.events_sink.clone(),
            &self.watcher,
        ) {
            eprintln!("[nib] 文件监听启动失败: {}", err);
        }
        self.project_root = root.clone();
        self.project_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string())
            .into();
        self.status = root.display().to_string().into();
        self.expanded_paths.clear();
        session::remember_recent(root.to_string_lossy().to_string());
        if let Some(panel) = &self.terminal {
            panel.update(cx, |panel, _| panel.set_project(root.clone()));
        }

        self.reload_tree(cx);
        self.refresh_git_marks(cx);
        let git_root = self.project_root.clone();
        self.git_panel
            .update(cx, |panel, cx| panel.set_project(git_root.clone(), cx));
        self.maven_panel
            .update(cx, |panel, cx| panel.set_project(git_root.clone(), cx));
        self.astore_panel
            .update(cx, |panel, cx| panel.set_project(git_root, cx));

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

    /// 重读目录树(项目加载/外部结构变化共用;core runtime 上跑,带陈旧守卫)。
    /// 重建后回放展开态 + 恢复当前标签的选中(set_items 会清掉两者)。
    fn reload_tree(&mut self, cx: &mut Context<Self>) {
        let tree_for_load = self.tree_state.clone();
        let root_str = self.project_root.to_string_lossy().to_string();
        let guard_root = self.project_root.clone();
        cx.spawn(async move |weak, cx| {
            let result = nib_core::fs::read_dir_tree(root_str, Some(12)).await;
            if let Ok(node) = result {
                // 陈旧守卫:期间切了项目就丢弃;同帧取展开态与当前标签
                let Ok((still_current, expanded, active_item)) = weak.read_with(cx, |this, _| {
                    (
                        this.project_root == guard_root,
                        this.expanded_paths.clone(),
                        this.active().map(|t| {
                            (t.path.to_string_lossy().to_string(), t.title.clone())
                        }),
                    )
                }) else {
                    return;
                };
                if !still_current {
                    return;
                }
                let items: Vec<TreeItem> = match &node.children {
                    Some(children) => children
                        .iter()
                        .map(|c| file_node_to_tree_item(c, &expanded))
                        .collect(),
                    None => vec![file_node_to_tree_item(&node, &expanded)],
                };
                tree_for_load.update(cx, |state, cx| {
                    state.set_items(items, cx);
                    if let Some((id, title)) = active_item {
                        let item = TreeItem::new(id, title);
                        state.set_selected_item(Some(&item), cx);
                    }
                });
            }
        })
        .detach();
    }

    /// 外部文件变更(watcher 已做 500ms 防抖 + 产物目录过滤):
    /// 结构变化重读树;打开中的非脏标签自动跟随磁盘内容(agent 改文件编辑器要跟上)
    fn on_core_event(
        &mut self,
        event: nib_core::CoreEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let nib_core::CoreEvent::LspDiagnostics(params) = &event {
            self.apply_diagnostics(params.clone(), window, cx);
            return;
        }
        let nib_core::CoreEvent::FileChanged {
            project,
            has_structural,
        } = event
        else {
            return;
        };
        if std::path::Path::new(&project) != self.project_root.as_path() {
            return;
        }
        if has_structural {
            self.reload_tree(cx);
        }
        self.git_panel.update(cx, |panel, cx| panel.refresh(cx));
        self.refresh_git_marks(cx);
        let _ = window;
        for tab in &self.tabs {
            if tab.dirty {
                continue; // 本地有未保存编辑,绝不覆盖
            }
            let path = tab.path.clone();
            let editor = tab.editor.clone();
            let window_handle = self.window_handle;
            cx.spawn(async move |weak, cx| {
                let Ok(disk) = nib_core::fs::read_file(path.to_string_lossy().to_string()).await
                else {
                    return;
                };
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    let _ = weak.update(cx, |this: &mut Workbench, cx| {
                        // 陈旧守卫:标签还在、仍非脏、内容确实变了才回灌
                        let still = this
                            .tabs
                            .iter()
                            .any(|t| t.path == path && !t.dirty && t.editor == editor);
                        if !still {
                            return;
                        }
                        let changed = editor.read(cx).value().as_ref() != disk.as_str();
                        if changed {
                            editor.update(cx, |state, cx| {
                                state.set_value(disk.clone(), window, cx);
                            });
                            // set_value 会触发 Change 订阅误标脏,这里立刻洗掉
                            if let Some(tab) = this.tabs.iter_mut().find(|t| t.path == path) {
                                tab.dirty = false;
                            }
                            cx.notify();
                        }
                    });
                });
            })
            .detach();
        }
    }

    /// LSP publishDiagnostics → 命中标签的 DiagnosticSet(行列直接用 LSP Position)
    fn apply_diagnostics(
        &mut self,
        params: serde_json::Value,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(params) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params)
        else {
            return;
        };
        let path = PathBuf::from(params.uri.path().as_str());
        let Some(tab) = self.tabs.iter().find(|t| t.path == path) else {
            return;
        };
        tab.editor.update(cx, |state, cx| {
            let text = state.text().clone();
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&text);
                for d in &params.diagnostics {
                    let mut diag = gpui_component::highlighter::Diagnostic::new(
                        d.range.start..d.range.end,
                        d.message.clone(),
                    );
                    if let Some(sev) = d.severity {
                        diag = diag.with_severity(sev);
                    }
                    set.push(diag);
                }
            }
            cx.notify();
        });
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
        let text_for_lsp = text.clone();
        let settings = self.settings;
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(lang)
                .multi_line(true)
                .tab_size(TabSize {
                    tab_size: settings.tab_size as usize,
                    ..Default::default()
                })
                .soft_wrap(settings.word_wrap)
                .folding(settings.folding)
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
                if this.md_preview {
                    cx.notify();
                }
            }
        });
        let title: SharedString = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
            .into();
        // Java:起 jdtls(幂等)并 didOpen——diagnostics 会经 EventSink 流回
        if lang == "java" {
            let lsp = self.lsp.clone();
            let sink = self.events_sink.clone();
            let root = self.project_root.to_string_lossy().to_string();
            let file = path.to_string_lossy().to_string();
            let content = text_for_lsp;
            cx.spawn(async move |_, _| {
                let jdtls_root = session::data_dirs().jdtls_workspaces();
                if let Err(err) = nib_core::lsp::start_lsp(
                    "java".into(),
                    root,
                    sink as Arc<dyn nib_core::EventSink>,
                    jdtls_root,
                    &lsp,
                )
                .await
                {
                    eprintln!("[nib-lsp] jdtls 启动失败: {}", err);
                    return;
                }
                if let Err(err) =
                    nib_core::lsp::lsp_did_open(file, "java".into(), content, &lsp).await
                {
                    eprintln!("[nib-lsp] didOpen 失败: {}", err);
                }
            })
            .detach();
        }
        // LRU 淘汰:超上限时关掉最久未用且不脏的标签(全脏则不淘汰,宁多勿丢)
        if self.tabs.len() >= MAX_TABS {
            let evict = self
                .tabs
                .iter()
                .enumerate()
                .filter(|(ix, t)| !t.dirty && Some(*ix) != self.active_tab)
                .min_by_key(|(_, t)| t.last_used)
                .map(|(ix, _)| ix);
            if let Some(evict_ix) = evict {
                self.tabs.remove(evict_ix);
                if let Some(active) = self.active_tab {
                    if evict_ix < active {
                        self.active_tab = Some(active - 1);
                    }
                }
            }
        }
        let observe_sub = cx.observe(&editor, |_, _, cx| cx.notify());
        self.tabs.push(OpenTab {
            path,
            title,
            lang,
            dirty: false,
            last_used: Instant::now(),
            editor,
            _change_sub: change_sub,
            _observe_sub: observe_sub,
        });
        self.tabs.len() - 1
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active_tab = Some(ix);
        self.tabs[ix].last_used = Instant::now();
        let tab = &self.tabs[ix];
        self.status = tab.path.display().to_string().into();
        let handle = tab.editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        // 树高亮跟随当前标签(按 id 匹配,自动展开祖先;观察者对已激活文件是 no-op)
        let tree_item = TreeItem::new(tab.path.to_string_lossy().to_string(), tab.title.clone());
        self.tree_state
            .update(cx, |state, cx| state.set_selected_item(Some(&tree_item), cx));
        self.persist_session(cx);
        cx.notify();
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // 已开 → 直接切标签(无 window 的调用路径,只改激活态)
        if let Some(ix) = self.tabs.iter().position(|t| t.path == path) {
            if self.active_tab != Some(ix) {
                self.active_tab = Some(ix);
                self.tabs[ix].last_used = Instant::now();
                self.status = path.display().to_string().into();
                self.persist_session(cx);
                // 本路径无 window(树点击的 observe 进来),经窗口句柄把焦点
                // 交还编辑器——否则切到已开标签后键入无落点
                let editor = self.tabs[ix].editor.clone();
                let window_handle = self.window_handle;
                cx.spawn(async move |_, cx| {
                    let _ = cx.update_window(window_handle, |_, window, cx| {
                        let handle = editor.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                    });
                })
                .detach();
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

        let lsp = self.lsp.clone();
        let is_java = path.extension().is_some_and(|e| e == "java");
        cx.spawn(async move |this, cx| {
            let result =
                nib_core::fs::write_file(path.to_string_lossy().to_string(), text.clone()).await;
            if result.is_ok() && is_java {
                if let Err(err) = nib_core::lsp::lsp_did_change(
                    path.to_string_lossy().to_string(),
                    text,
                    &lsp,
                )
                .await
                {
                    eprintln!("[nib-lsp] didChange 失败: {}", err);
                }
            }
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
        self.close_tab_at(ix, window, cx);
    }

    fn close_tab_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        self.active_tab = if self.tabs.is_empty() {
            window.focus(&self.focus_handle, cx);
            None
        } else {
            let new_active = match self.active_tab {
                Some(active) if active > ix => active - 1,
                Some(active) if active == ix => ix.min(self.tabs.len() - 1),
                other => return self.finish_close(other, cx),
            };
            let handle = self.tabs[new_active].editor.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            Some(new_active)
        };
        self.persist_session(cx);
        cx.notify();
    }

    fn finish_close(&mut self, active: Option<usize>, cx: &mut Context<Self>) {
        self.active_tab = active;
        self.persist_session(cx);
        cx.notify();
    }

    /// 打开文件并定位到行列(全局搜索跳转用)
    fn open_file_at(
        &mut self,
        path: PathBuf,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::Position;
        if let Some(ix) = self.tabs.iter().position(|t| t.path == path) {
            self.activate_tab(ix, window, cx);
            if let Some(tab) = self.tabs.get(ix) {
                tab.editor.update(cx, |state, cx| {
                    state.set_cursor_position(Position::new(line, column), window, cx);
                });
            }
            return;
        }
        let window_handle = self.window_handle;
        let status_path = path.display().to_string();
        self.status = format!("打开 {} …", status_path).into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let content = nib_core::fs::read_file(path.to_string_lossy().to_string()).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if let Ok(text) = content {
                        let ix = this.insert_tab(path.clone(), text, window, cx);
                        this.activate_tab(ix, window, cx);
                        if let Some(tab) = this.tabs.get(ix) {
                            tab.editor.update(cx, |state, cx| {
                                state.set_cursor_position(Position::new(line, column), window, cx);
                            });
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// F12 跳定义(旧版主路径):跨文件由宿主完成,绕开组件只支持同文件的限制
    fn on_goto_definition(
        &mut self,
        _: &GotoDefinition,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active() else { return };
        if tab.lang != "java" {
            return;
        }
        let pos = tab.editor.read(cx).cursor_position();
        let file = tab.path.to_string_lossy().to_string();
        let lsp = self.lsp.clone();
        let window_handle = self.window_handle;
        self.status = "跳转定义…".into();
        cx.notify();
        cx.spawn(async move |weak, cx| {
            let result =
                nib_core::lsp::lsp_goto_definition(file, pos.line, pos.character, &lsp).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    match result {
                        Ok(Some(usage)) => {
                            let path = PathBuf::from(
                                usage.uri.strip_prefix("file://").unwrap_or(&usage.uri),
                            );
                            this.status = path.display().to_string().into();
                            this.open_file_at(path, usage.line, usage.character, window, cx);
                        }
                        Ok(None) => this.status = "未找到定义".into(),
                        Err(err) => this.status = format!("跳转失败: {}", err).into(),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// Shift+F12 查引用:core lsp_find_references → 浮层列表
    fn on_find_usages(&mut self, _: &FindUsages, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active() else { return };
        if tab.lang != "java" {
            return;
        }
        let pos = tab.editor.read(cx).cursor_position();
        let file = tab.path.to_string_lossy().to_string();
        let title = tab.title.to_string();
        let lsp = self.lsp.clone();
        let window_handle = self.window_handle;
        self.status = "查找引用…".into();
        cx.notify();
        cx.spawn(async move |weak, cx| {
            let result =
                nib_core::lsp::lsp_find_references(file, pos.line, pos.character, &lsp).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    match result {
                        Ok(usages) if !usages.is_empty() => {
                            this.status = format!("{} 处引用", usages.len()).into();
                            let view = cx.new(|_| UsagesView::new(title.clone(), usages));
                            let sub = cx.subscribe_in(
                                &view,
                                window,
                                |this: &mut Workbench,
                                 _,
                                 event: &UsagesEvent,
                                 window,
                                 cx| match event {
                                    UsagesEvent::Open {
                                        path,
                                        line,
                                        character,
                                    } => {
                                        let (path, line, character) =
                                            (path.clone(), *line, *character);
                                        this.close_palette(window, cx);
                                        this.open_file_at(path, line, character, window, cx);
                                    }
                                },
                            );
                            this.overlay = Some(Overlay::Usages(view));
                            this._overlay_sub = Some(sub);
                            window.focus(&this.overlay_focus, cx);
                        }
                        Ok(_) => this.status = "未找到引用".into(),
                        Err(err) => this.status = format!("查找失败: {}", err).into(),
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn on_palette_confirm(
        &mut self,
        _: &PaletteConfirm,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &self.overlay {
            Some(Overlay::Usages(view)) => view.update(cx, |view, cx| view.confirm(cx)),
            Some(Overlay::Recents(view)) => view.update(cx, |view, cx| view.confirm(cx)),
            _ => {}
        }
    }

    /// Arthas:光标处方法 → 命令 → 剪贴板(对齐旧版右键菜单链路)
    fn arthas_command(&mut self, cmd: nib_core::arthas::ArthasCommand, cx: &mut Context<Self>) {
        let Some(tab) = self.active() else { return };
        if tab.lang != "java" {
            self.status = "Arthas 仅支持 Java 文件".into();
            cx.notify();
            return;
        }
        let pos = tab.editor.read(cx).cursor_position();
        let text = tab.editor.read(cx).value().to_string();
        let file = tab.path.to_string_lossy().to_string();
        let lsp = self.lsp.clone();
        cx.spawn(async move |weak, cx| {
            // FQCN:package 声明 + 文件名类(旧版同口径)
            let pkg = nib_core::arthas::parse_package(&text);
            let class = nib_core::arthas::class_name_from_file_path(&file);
            let self_fqn = if pkg.is_empty() { class } else { format!("{}.{}", pkg, class) };

            // 旧版语义第一优先:光标停在调用点(标识符后跟'(',非声明行)→ 命令打在被调方上。
            // cursor_position().character 是字符列(组件实证,非 UTF-16),换算字节列再解析。
            let call_target = text.lines().nth(pos.line as usize).and_then(|line| {
                let byte_col =
                    nib_core::arthas::char_col_to_byte(line, pos.character as usize);
                let (word, s, e) = nib_core::arthas::identifier_at(line, byte_col)?;
                if !nib_core::arthas::followed_by_paren(line, e) {
                    return None;
                }
                nib_core::arthas::resolve_call_fqn_by_text(&text, line, s, &word, &pkg, &self_fqn)
            });

            // 兜底:所在方法(LSP 符号树,再失败落声明行)
            let (fqn, method) = match call_target {
                Some((fqn, method)) => (fqn, Some(method)),
                None => {
                    let method =
                        match nib_core::lsp::lsp_document_symbols(file.clone(), &lsp).await {
                            Ok(symbols) => nib_core::arthas::find_method_at_position(
                                &symbols,
                                pos.line as u64,
                                pos.character as u64,
                            ),
                            Err(_) => None,
                        }
                        .or_else(|| {
                            text.lines()
                                .nth(pos.line as usize)
                                .and_then(nib_core::arthas::method_name_from_decl_line)
                        });
                    (self_fqn, method)
                }
            };
            let command =
                nib_core::arthas::generate_arthas_command(&fqn, method.as_deref(), cmd);
            let copied = nib_core::clipboard::copy_text_to_clipboard(command.clone());
            let _ = weak.update(cx, |this: &mut Workbench, cx| {
                this.status = match copied {
                    Ok(()) => format!("已复制: {}", command).into(),
                    Err(err) => format!("复制失败: {}", err).into(),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// 活动栏切视图(对齐旧版 activity-bar):Commit/Git 共用 GitPanel 按 mode 渲染
    fn set_sidebar_view(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        self.sidebar_view = view;
        match view {
            SidebarView::Commit => self.git_panel.update(cx, |p, cx| {
                p.set_mode(GitPanelMode::Commit, cx);
                p.refresh(cx);
            }),
            SidebarView::Git => self.git_panel.update(cx, |p, cx| {
                p.set_mode(GitPanelMode::Branches, cx);
                p.refresh(cx);
            }),
            _ => {}
        }
        cx.notify();
    }

    fn on_toggle_astore(&mut self, _: &ToggleAstore, _: &mut Window, cx: &mut Context<Self>) {
        self.astore_visible = !self.astore_visible;
        if let Some(panel) = &self.terminal {
            let inset = if self.astore_visible { ASTORE_WIDTH } else { 0. };
            panel.update(cx, |p, _| p.set_right_inset(inset));
        }
        cx.notify();
    }

    fn on_toggle_md_preview(
        &mut self,
        _: &ToggleMdPreview,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.md_preview = !self.md_preview;
        cx.notify();
    }

    /// 底部终端开/关。首次打开才起 shell;关闭只藏不杀(再开即回)。
    fn on_toggle_terminal(
        &mut self,
        _: &ToggleTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_visible = !self.terminal_visible;
        if self.terminal_visible {
            let panel = match &self.terminal {
                Some(panel) => panel.clone(),
                None => {
                    let panel = cx.new(|cx| TerminalPanel::new(self.project_root.clone(), cx));
                    self.terminal = Some(panel.clone());
                    panel
                }
            };
            let handle = panel.read(cx).focus_handle();
            window.focus(&handle, cx);
        } else {
            let handle = match self.active() {
                Some(tab) => tab.editor.read(cx).focus_handle(cx),
                None => self.focus_handle.clone(),
            };
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// 设置浮层:保存即持久化 + 热应用(换行/折叠经运行时 setter 下发已开标签)
    fn on_open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, Some(Overlay::Settings(_))) {
            self.close_palette(window, cx);
            return;
        }
        let view = cx.new(|cx| SettingsView::new(self.settings, window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this: &mut Workbench, _, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Apply(settings) => {
                    let settings = *settings;
                    this.settings = settings;
                    session::save_settings(settings);
                    for tab in &this.tabs {
                        tab.editor.update(cx, |state, cx| {
                            state.set_soft_wrap(settings.word_wrap, window, cx);
                            state.set_folding(settings.folding, window, cx);
                        });
                    }
                    this.status = "设置已保存 ✓".into();
                    this.close_palette(window, cx);
                }
            },
        );
        self.overlay = Some(Overlay::Settings(view));
        self._overlay_sub = Some(sub);
        window.focus(&self.overlay_focus, cx);
        cx.notify();
    }

    /// 最近项目浮层(File 菜单进入):读持久化列表 → 选中即切项目
    fn on_show_recents(
        &mut self,
        _: &ShowRecentProjects,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, Some(Overlay::Recents(_))) {
            self.close_palette(window, cx);
            return;
        }
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            let recents = session::load_recents().await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    let view = cx.new(|_| RecentsView::new(recents));
                    let sub = cx.subscribe_in(
                        &view,
                        window,
                        |this: &mut Workbench, _, event: &RecentsEvent, window, cx| match event {
                            RecentsEvent::Open(path) => {
                                let root = PathBuf::from(path.clone());
                                this.close_palette(window, cx);
                                if root != this.project_root {
                                    this.tabs.clear();
                                    this.active_tab = None;
                                    this.load_project(root, cx);
                                    this.persist_session(cx);
                                }
                                cx.notify();
                            }
                        },
                    );
                    this.overlay = Some(Overlay::Recents(view));
                    this._overlay_sub = Some(sub);
                    window.focus(&this.overlay_focus, cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn on_open_folder(&mut self, _: &OpenFolder, _: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("打开项目".into()),
        });
        cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(root) = paths.into_iter().next() {
                    let _ = weak.update(cx, |this: &mut Workbench, cx| {
                        this.tabs.clear();
                        this.active_tab = None;
                        this.load_project(root, cx);
                        this.persist_session(cx);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    /// 打开 diff 浮层(diff 在 core runtime 计算,回主线程建视图)
    fn open_diff(&mut self, rel_path: String, abs_path: PathBuf, cx: &mut Context<Self>) {
        let cwd = self.project_root.to_string_lossy().to_string();
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            let Ok(diff) = nib_core::diff::diff_file_against_head(cwd, rel_path.clone()).await
            else {
                return;
            };
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    let view = cx.new(|_| DiffView::new(rel_path.clone(), abs_path.clone(), diff));
                    let sub = cx.subscribe_in(
                        &view,
                        window,
                        |this: &mut Workbench, _, event: &DiffViewEvent, window, cx| match event {
                            DiffViewEvent::OpenFile(path) => {
                                let path = path.clone();
                                this.close_palette(window, cx);
                                this.open_file(path, cx);
                            }
                        },
                    );
                    this.overlay = Some(Overlay::Diff(view));
                    this._overlay_sub = Some(sub);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 打开 3-way merge 浮层(Git 面板点击冲突文件进来)。解决成功后
    /// 关浮层并刷新 Git 面板;文件写回会触发 watcher,打开的标签自动重载。
    fn open_merge(&mut self, rel_path: String, cx: &mut Context<Self>) {
        let cwd = self.project_root.to_string_lossy().to_string();
        let view = cx.new(|cx| MergeView::new(cwd, rel_path, cx));
        let sub = cx.subscribe(
            &view,
            |this: &mut Workbench, _, event: &MergeViewEvent, cx| match event {
                MergeViewEvent::Resolved => {
                    this.git_panel.update(cx, |panel, cx| panel.refresh(cx));
                    let window_handle = this.window_handle;
                    cx.spawn(async move |weak, cx| {
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            let _ = weak.update(cx, |this: &mut Workbench, cx| {
                                this.close_palette(window, cx);
                            });
                        });
                    })
                    .detach();
                }
            },
        );
        self.overlay = Some(Overlay::Merge(view));
        self._overlay_sub = Some(sub);
        // 聚焦浮层锚点让 Esc 生效(本方法无 window,经窗口句柄异步聚焦)
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    window.focus(&this.overlay_focus, cx);
                });
            });
        })
        .detach();
        cx.notify();
    }

    // ---- 浮层(quick-open / 全局搜索) ----

    fn on_toggle_quick_open(
        &mut self,
        _: &ToggleQuickOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_palette(window, cx);
    }

    fn on_toggle_search(&mut self, _: &ToggleSearch, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::Search(_))) {
            self.close_palette(window, cx);
            return;
        }
        let panel = cx.new(|cx| SearchPanel::new(self.project_root.clone(), window, cx));
        panel.update(cx, |p, cx| p.focus(window, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this: &mut Self, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Open { path, line, column } => {
                    let (path, line, column) = (path.clone(), *line, *column);
                    this.close_palette(window, cx);
                    this.open_file_at(path, line, column, window, cx);
                }
            },
        );
        self.overlay = Some(Overlay::Search(panel));
        self._overlay_sub = Some(sub);
        cx.notify();
    }

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::QuickOpen(_))) {
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
        self.overlay = Some(Overlay::QuickOpen(palette));
        self._overlay_sub = Some(sub);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self._overlay_sub = None;
        let handle = match self.active() {
            Some(tab) => tab.editor.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        };
        window.focus(&handle, cx);
        cx.notify();
    }

    fn on_palette_up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::QuickOpen(p)) => p.update(cx, |p, cx| p.move_selection(-1, cx)),
            Some(Overlay::Search(p)) => p.update(cx, |p, cx| p.move_selection(-1, cx)),
            Some(Overlay::Usages(p)) => p.update(cx, |p, cx| p.move_selection(-1, cx)),
            Some(Overlay::Recents(p)) => p.update(cx, |p, cx| p.move_selection(-1, cx)),
            Some(Overlay::Diff(_)) | Some(Overlay::Merge(_)) | Some(Overlay::Settings(_))
            | None => {}
        }
    }

    fn on_palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::QuickOpen(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Search(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Usages(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Recents(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Diff(_)) | Some(Overlay::Merge(_)) | Some(Overlay::Settings(_))
            | None => {}
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

}

/// Explorer 树行(对齐旧版):文件夹用折叠图标,文件用类型字形图标(file-icons 同源),
/// 文件名按 git 状态着色(M=橙 A/?=绿 D=红),lock/忽略类淡化
fn render_tree_item(
    ix: usize,
    entry: &gpui_component::tree::TreeEntry,
    _: bool,
    _: &mut Window,
    app: &mut App,
    marks: &std::collections::HashMap<String, char>,
) -> ListItem {
    let item = entry.item();
    let row = h_flex().gap_2().items_center();
    let row = if entry.is_folder() {
        let icon = if entry.is_expanded() {
            IconName::FolderOpen
        } else {
            IconName::Folder
        };
        row.child(Icon::new(icon).small())
            .child(div().child(item.label.clone()))
    } else {
        let meta = file_icons::file_icon_meta(&item.label);
        let name_color = match marks.get(item.id.as_ref()) {
            Some('M') | Some('R') => Some(app.theme().warning),
            Some('A') | Some('U') => Some(app.theme().success),
            Some('D') => Some(app.theme().danger),
            _ if meta.dim => Some(app.theme().muted_foreground),
            _ => None,
        };
        row.child(
            div()
                .w(px(14.))
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .text_color(meta.color)
                .child(meta.glyph),
        )
        .child(
            div()
                .when_some(name_color, |s, c| s.text_color(c))
                .child(item.label.clone()),
        )
    };
    ListItem::new(ix).pl(px(8.) + px(14.) * entry.depth() as f32).child(row)
}

fn lang_display(lang: &str) -> &'static str {
    match lang {
        "rust" => "Rust",
        "java" => "Java",
        "typescript" => "TypeScript",
        "javascript" => "JavaScript",
        "python" => "Python",
        "markdown" => "Markdown",
        "json" => "JSON",
        "yaml" => "YAML",
        "toml" => "TOML",
        "html" => "HTML",
        "css" => "CSS",
        "xml" => "XML",
        "bash" => "Shell",
        "go" => "Go",
        "c" => "C",
        "cpp" => "C++",
        _ => "Plain Text",
    }
}

impl Workbench {
    /// 活动栏按钮(旧版 .activity-btn 40×40,激活态高亮)
    fn activity_btn(
        &self,
        id: &'static str,
        glyph: &'static str,
        view: SidebarView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.sidebar_view == view;
        div()
            .id(id)
            .w(px(40.))
            .h(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(cx.theme().radius)
            .text_size(px(18.))
            .text_color(if active {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .when(active, |s| s.bg(cx.theme().list_active))
            .hover(|s| s.bg(cx.theme().accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.set_sidebar_view(view, cx)),
            )
            .child(glyph)
    }

    /// 单个编辑器标签(旧版 .tab:类型图标+文件名+关闭×;脏=●)
    fn render_editor_tab(&self, ix: usize, tab: &OpenTab, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_tab == Some(ix);
        let icon = file_icons::file_icon_meta(&tab.title);
        let dirty = tab.dirty;
        h_flex()
            .id(ix)
            .h_full()
            .px_3()
            .gap_2()
            .items_center()
            .flex_none()
            .border_r_1()
            .border_color(cx.theme().border)
            .when(active, |s| {
                s.bg(cx.theme().background)
                    .border_t_2()
                    .border_color(cx.theme().primary)
            })
            .when(!active, |s| {
                s.bg(cx.theme().sidebar)
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().accent))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.activate_tab(ix, window, cx)),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(icon.color)
                    .child(icon.glyph),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .whitespace_nowrap()
                    .when(icon.dim, |s| s.text_color(cx.theme().muted_foreground))
                    .child(tab.title.clone()),
            )
            .child(
                div()
                    .id(("tab-close", ix))
                    .w(px(16.))
                    .h(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .map(|s| {
                        if dirty {
                            s.text_color(cx.theme().foreground).child("●")
                        } else {
                            s.text_color(cx.theme().muted_foreground)
                                .hover(|s| s.bg(cx.theme().accent))
                                .child("×")
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_at(ix, window, cx);
                        }),
                    ),
            )
    }

    /// 面包屑(旧版 #breadcrumb):项目名 › 相对路径段 › 文件名
    fn render_breadcrumb(&self, path: &std::path::Path, cx: &mut Context<Self>) -> impl IntoElement {
        let rel = path
            .strip_prefix(&self.project_root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.to_path_buf());
        let mut segs: Vec<String> = vec![self.project_name.to_string()];
        segs.extend(rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()));
        let line = segs.join(" › ");
        h_flex()
            .h(px(24.))
            .px_3()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .text_size(px(11.))
            .text_color(cx.theme().muted_foreground)
            .overflow_hidden()
            .whitespace_nowrap()
            .child(line)
    }

    /// Explorer 头部的「定位当前文件」(旧版 btn-locate-file ⌖)
    fn locate_current_file(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.active() {
            let item = TreeItem::new(tab.path.to_string_lossy().to_string(), tab.title.clone());
            self.tree_state
                .update(cx, |state, cx| state.set_selected_item(Some(&item), cx));
        }
    }

    /// 拉一次 git status 喂给树着色(项目装载 + watcher 变更时;陈旧守卫同款)
    fn refresh_git_marks(&mut self, cx: &mut Context<Self>) {
        let cwd = self.project_root.to_string_lossy().to_string();
        let guard_root = self.project_root.clone();
        let root = self.project_root.clone();
        cx.spawn(async move |weak, cx| {
            let Ok(changes) = nib_core::git::git_status(cwd).await else {
                return;
            };
            let mut marks = std::collections::HashMap::new();
            for c in changes {
                let abs = root.join(&c.path).to_string_lossy().to_string();
                let ch = c.status.chars().next().unwrap_or(' ');
                // 同文件 staged+unstaged 两条:改动类标记优先于已暂存覆盖
                marks.entry(abs).or_insert(ch);
            }
            let _ = weak.update(cx, |this: &mut Workbench, cx| {
                if this.project_root == guard_root {
                    this.git_marks = Arc::new(marks);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            if let Some(t0) = APP_START.get() {
                eprintln!("[nib-perf] 首帧 {}ms", t0.elapsed().as_millis());
            }
        }
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
            .on_action(cx.listener(Self::on_toggle_search))
            .on_action(cx.listener(Self::on_open_folder))
            .on_action(cx.listener(Self::on_goto_definition))
            .on_action(cx.listener(Self::on_find_usages))
            .on_action(cx.listener(Self::on_toggle_md_preview))
            .on_action(cx.listener(Self::on_toggle_terminal))
            .on_action(cx.listener(Self::on_toggle_astore))
            .on_action(cx.listener(Self::on_show_recents))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(|this: &mut Self, _: &ArthasWatch, _, cx| {
                this.arthas_command(nib_core::arthas::ArthasCommand::Watch, cx)
            }))
            .on_action(cx.listener(|this: &mut Self, _: &ArthasTrace, _, cx| {
                this.arthas_command(nib_core::arthas::ArthasCommand::Trace, cx)
            }))
            .on_action(cx.listener(|this: &mut Self, _: &ArthasStack, _, cx| {
                this.arthas_command(nib_core::arthas::ArthasCommand::Stack, cx)
            }))
            .on_action(cx.listener(|this: &mut Self, _: &ArthasMonitor, _, cx| {
                this.arthas_command(nib_core::arthas::ArthasCommand::Monitor, cx)
            }))
            .on_action(cx.listener(|this: &mut Self, _: &ArthasTt, _, cx| {
                this.arthas_command(nib_core::arthas::ArthasCommand::Tt, cx)
            }))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .child(TitleBar::new().child(div().text_sm().child(title)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w(px(ACTIVITY_WIDTH))
                            .h_full()
                            .items_center()
                            .py_2()
                            .gap_1()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .child(self.activity_btn("act-files", "☰", SidebarView::Files, cx))
                            .child(self.activity_btn("act-commit", "✓", SidebarView::Commit, cx))
                            .child(self.activity_btn("act-git", "⎇", SidebarView::Git, cx))
                            .child(self.activity_btn("act-maven", "◪", SidebarView::Maven, cx))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id("act-settings")
                                    .w(px(40.))
                                    .h(px(40.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(cx.theme().radius)
                                    .text_size(px(18.))
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|s| s.bg(cx.theme().accent))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.on_open_settings(&OpenSettings, window, cx)
                                        }),
                                    )
                                    .child("⚙"),
                            ),
                    )
                    .child(
                        v_flex()
                            .w(px(SIDEBAR_WIDTH))
                            .h_full()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .child(
                                h_flex()
                                    .h(px(30.))
                                    .px_3()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(cx.theme().muted_foreground)
                                            .child(self.sidebar_view.title().to_uppercase()),
                                    )
                                    .when(self.sidebar_view == SidebarView::Files, |s| {
                                        s.child(
                                            div()
                                                .id("locate-file")
                                                .text_size(px(14.))
                                                .text_color(cx.theme().muted_foreground)
                                                .hover(|s| s.text_color(cx.theme().foreground))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(|this, _, _, cx| {
                                                        this.locate_current_file(cx)
                                                    }),
                                                )
                                                .child("⌖"),
                                        )
                                    }),
                            )
                            .child(div().flex_1().min_h_0().map(|this| {
                                match self.sidebar_view {
                                    SidebarView::Files => this
                                        .child(tree(&self.tree_state, {
                                            let marks = self.git_marks.clone();
                                            move |ix, entry, sel, window, app| {
                                                render_tree_item(ix, entry, sel, window, app, &marks)
                                            }
                                        })),
                                    SidebarView::Commit | SidebarView::Git => {
                                        this.child(self.git_panel.clone())
                                    }
                                    SidebarView::Maven => this.child(self.maven_panel.clone()),
                                }
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .when(!self.tabs.is_empty(), |this| {
                                this.child(
                                    h_flex()
                                        .id("editor-tabs")
                                        .h(px(36.))
                                        .w_full()
                                        .overflow_x_scroll()
                                        .border_b_1()
                                        .border_color(cx.theme().border)
                                        .children(self.tabs.iter().enumerate().map(
                                            |(ix, t)| self.render_editor_tab(ix, t, cx),
                                        )),
                                )
                                .when_some(self.active(), |this, tab| {
                                    this.child(self.render_breadcrumb(&tab.path, cx))
                                })
                            })
                            .child(div().flex_1().min_h_0().map(|this| {
                                match self.active() {
                                    Some(tab) => {
                                        let editor_el = Input::new(&tab.editor)
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(px(self.settings.font_size))
                                            .size_full();
                                        if self.md_preview && tab.lang == "markdown" {
                                            let text = tab.editor.read(cx).value();
                                            this.child(
                                                h_flex()
                                                    .size_full()
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .h_full()
                                                            .min_w_0()
                                                            .child(editor_el),
                                                    )
                                                    .child(
                                                        div()
                                                            .id("md-preview")
                                                            .flex_1()
                                                            .h_full()
                                                            .min_w_0()
                                                            .overflow_y_scroll()
                                                            .border_l_1()
                                                            .border_color(cx.theme().border)
                                                            .p_4()
                                                            .child(
                                                                gpui_component::text::TextView::markdown(
                                                                    "md-preview-view",
                                                                    text,
                                                                ),
                                                            ),
                                                    ),
                                            )
                                        } else {
                                            this.child(editor_el)
                                        }
                                    }
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
                            }))
                            .when(self.terminal_visible, |this| {
                                this.when_some(self.terminal.clone(), |this, panel| {
                                    this.child(
                                        div()
                                            .h(px(terminal_panel::PANEL_HEIGHT))
                                            .border_t_1()
                                            .border_color(cx.theme().border)
                                            .child(panel),
                                    )
                                })
                            }),
                    )
                    .when(self.astore_visible, |row| {
                        row.child(
                            v_flex()
                                .w(px(ASTORE_WIDTH))
                                .h_full()
                                .border_l_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().sidebar)
                                .child(
                                    h_flex()
                                        .h(px(30.))
                                        .px_3()
                                        .items_center()
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().muted_foreground)
                                        .child("ASTORE"),
                                )
                                .child(div().flex_1().min_h_0().child(self.astore_panel.clone())),
                        )
                    }),
            )
            .child(
                h_flex()
                    .h(px(24.))
                    .px_2()
                    .items_center()
                    .gap_3()
                    .bg(cx.theme().status_bar)
                    .border_t_1()
                    .border_color(cx.theme().status_bar_border)
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .map(|bar| {
                        let branch = self.git_panel.read(cx).branch();
                        bar.when(!branch.is_empty(), |s| {
                            s.child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .whitespace_nowrap()
                                    .child(div().text_color(cx.theme().info).child("⎇"))
                                    .child(branch),
                            )
                        })
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(self.status.clone()),
                    )
                    .when(self.stall_count > 0, |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("卡顿 ×{}", self.stall_count)),
                        )
                    })
                    .when_some(
                        self.active()
                            .map(|t| t.editor.read(cx).cursor_position()),
                        |this, pos| {
                            this.child(format!("Ln {}, Col {}", pos.line + 1, pos.character + 1))
                        },
                    )
                    .when(self.active().is_some(), |this| {
                        this.child(format!("Spaces: {}", self.settings.tab_size))
                            .child("UTF-8")
                            .child("LF")
                    })
                    .when(!active_lang.is_empty(), |this| {
                        this.child(lang_display(active_lang))
                    })
                    .when(active_lang == "java", |this| {
                        this.child(
                            div()
                                .w(px(7.))
                                .h(px(7.))
                                .rounded_full()
                                .bg(cx.theme().success),
                        )
                    }),
            )
            .when(self.overlay.is_some(), |this| {
                let content: AnyElement = match self.overlay.as_ref().unwrap() {
                    Overlay::QuickOpen(p) => p.clone().into_any_element(),
                    Overlay::Search(p) => p.clone().into_any_element(),
                    Overlay::Diff(p) => p.clone().into_any_element(),
                    Overlay::Usages(p) => p.clone().into_any_element(),
                    Overlay::Merge(p) => p.clone().into_any_element(),
                    Overlay::Recents(p) => p.clone().into_any_element(),
                    Overlay::Settings(p) => p.clone().into_any_element(),
                };
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .key_context("QuickOpen")
                        .track_focus(&self.overlay_focus)
                        .on_action(cx.listener(Self::on_palette_up))
                        .on_action(cx.listener(Self::on_palette_down))
                        .on_action(cx.listener(Self::on_palette_dismiss))
                        .on_action(cx.listener(Self::on_palette_confirm))
                        .child(div().mt(px(110.)).child(content)),
                )
            })
    }
}

fn main() {
    APP_START.set(Instant::now()).ok();
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

        cx.on_action(|_: &Quit, cx| cx.quit());
        // 原生菜单栏(条目对齐旧版 lib.rs build_menu)
        cx.set_menus([
            Menu::new("Nib").items([
                MenuItem::action("设置…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("退出 Nib", Quit),
            ]),
            Menu::new("File").items([
                MenuItem::action("打开文件夹…", OpenFolder),
                MenuItem::action("最近项目…", ShowRecentProjects),
                MenuItem::separator(),
                MenuItem::action("保存", SaveFile),
                MenuItem::action("关闭标签", CloseTab),
            ]),
            Menu::new("Arthas").items([
                MenuItem::action("Watch 光标方法", ArthasWatch),
                MenuItem::action("Trace 光标方法", ArthasTrace),
                MenuItem::action("Stack 光标方法", ArthasStack),
                MenuItem::action("Monitor 光标方法", ArthasMonitor),
                MenuItem::action("TimeTunnel 光标方法", ArthasTt),
            ]),
            Menu::new("Go").items([
                MenuItem::action("快速打开文件…", ToggleQuickOpen),
                MenuItem::action("在项目中搜索…", ToggleSearch),
                MenuItem::action("跳转到定义", GotoDefinition),
                MenuItem::action("查找引用", FindUsages),
                MenuItem::action("Markdown 预览", ToggleMdPreview),
            ]),
            // 对齐旧版 View 菜单(Terminal 项;Git/Astore 在侧栏页签,不重复列)
            Menu::new("View").items([
                MenuItem::action("Terminal", ToggleTerminal),
                MenuItem::action("Astore", ToggleAstore),
            ]),
        ]);

        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-o", OpenFolder, Some("Workbench")),
            KeyBinding::new("f12", GotoDefinition, Some("Workbench")),
            KeyBinding::new("shift-f12", FindUsages, Some("Workbench")),
            KeyBinding::new("cmd-shift-v", ToggleMdPreview, Some("Workbench")),
            KeyBinding::new("ctrl-`", ToggleTerminal, Some("Workbench")),
            KeyBinding::new("cmd-,", OpenSettings, Some("Workbench")),
            KeyBinding::new("enter", PaletteConfirm, Some("QuickOpen")),
            KeyBinding::new("cmd-s", SaveFile, Some("Workbench")),
            KeyBinding::new("cmd-w", CloseTab, Some("Workbench")),
            KeyBinding::new("cmd-p", ToggleQuickOpen, Some("Workbench")),
            KeyBinding::new("cmd-shift-f", ToggleSearch, Some("Workbench")),
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
