// Nib 原生工作台(M1):Warm Earth 主题 + 标签页 + quick-open + 会话恢复 + 脏标记。
// 性能纪律(RFC v2 §5):主线程零阻塞 IO——目录遍历/读写文件全部经 nib-core
// 自持 runtime,结果回主线程更新实体;异步回灌一律带陈旧守卫。

mod astore_panel;
mod diff_view;
mod file_icons;
mod arthas_panel;
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState, TabSize},
    list::ListItem,
    menu::ContextMenuExt,
    resizable::{h_resizable, resizable_panel, ResizableState},
    tree::{tree, TreeItem, TreeState},
    v_flex, ActiveTheme, Icon, IconName, Root, Theme, ThemeMode, ThemeRegistry, TitleBar,
};

use futures::StreamExt as _;
use diff_view::{DiffView, DiffViewEvent};
use arthas_panel::ArthasPanel;
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
        ToggleArthas,
        // 导航历史 cmd+[ / cmd+]
        NavBack,
        NavForward,
        // 文件树操作
        NewFile,
        NewFolder,
        RenameItem,
        DeleteItem,
        CopyItem,
        CutItem,
        PasteItem,
        UndoFileOp,
        CopyPath,
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
    // 压缩单子目录链(对齐设计稿 / VS Code):文件夹仅含一个子文件夹时合并成 a/b 一行,
    // 否则 src→main→java→com→… 每层一行,缩进会把文件名挤到截断。
    let mut cur = node;
    let mut label = node.name.clone();
    while let Some(children) = &cur.children {
        if children.len() == 1 && children[0].children.is_some() {
            cur = &children[0];
            label = format!("{}/{}", label, cur.name);
        } else {
            break;
        }
    }
    let item = TreeItem::new(cur.path.clone(), label).expanded(expanded.contains(&cur.path));
    match &cur.children {
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
    /// 文件树命名输入(新建文件/文件夹/重命名):输入框 + 标题 + 待执行操作
    NameInput {
        input: Entity<InputState>,
        title: SharedString,
        op: NameOp,
    },
}

/// 命名输入浮层确认后要执行的文件操作
#[derive(Clone)]
enum NameOp {
    /// 在该目录下新建文件
    NewFile(String),
    /// 在该目录下新建文件夹
    NewFolder(String),
    /// 重命名(旧绝对路径)
    Rename(String),
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
    /// 上次应用的 LSP 诊断签名:jdtls 对同一文件常重复推送相同诊断,
    /// 签名不变就跳过 set.reset+重渲染,避免空闲期每秒重复整屏重绘
    diag_sig: u64,
}

/// 导航历史一个落点(文件 + 0-based 行)。cmd+[ 后退 / cmd+] 前进(IDEA 式)。
#[derive(Clone, PartialEq)]
struct NavLoc {
    path: PathBuf,
    line: u32,
}

/// 资源管理器文件操作的可撤销记录(cmd+Z)。每个变体存还原所需的信息。
enum UndoOp {
    /// 删除=移到废纸篓:还原=从废纸篓移回原位
    Trashed { original: PathBuf, trashed: PathBuf },
    /// 剪切粘贴(移动):还原=移回
    Moved { from: PathBuf, to: PathBuf },
    /// 新建 / 粘贴副本:还原=移到废纸篓
    Created { path: PathBuf },
    /// 重命名:还原=改回原名
    Renamed { from: PathBuf, to: PathBuf },
}

/// 复制粘贴到同目录时生成不冲突的副本名:`<stem> copy.<ext>` → `<stem> copy 2.<ext>` …
fn unique_copy_path(dir: &str, name: &str) -> PathBuf {
    let p = std::path::Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let base = std::path::Path::new(dir);
    let first = base.join(format!("{stem} copy{ext}"));
    if !first.exists() {
        return first;
    }
    for i in 2..1000 {
        let c = base.join(format!("{stem} copy {i}{ext}"));
        if !c.exists() {
            return c;
        }
    }
    first
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
            SidebarView::Files => "资源管理器",
            SidebarView::Commit => "源代码管理",
            SidebarView::Git => "Git 图",
            SidebarView::Maven => "Maven",
        }
    }
}

/// 底部面板的三个 tab(对齐设计稿 nib-editor.html:问题 / 终端 / 输出)
#[derive(Clone, Copy, PartialEq)]
enum PanelTab {
    Problems,
    Terminal,
    Output,
}

/// 正在拖动调整哪个面板的尺寸(侧栏/Astore 宽,终端高)
#[derive(Clone, Copy, PartialEq)]
enum Resizing {
    Sidebar,
    Astore,
    Terminal,
}

/// 当前项目 java LSP(jdtls)的真实状态——状态栏据此显示,取代旧的硬编码"jdtls 就绪"。
/// 由 insert_tab(起 jdtls)、start_lsp 结果、LspProgress 事件、切项目共同驱动。
#[derive(Clone, PartialEq)]
enum LspPhase {
    /// 非 java 项目 / 未启动
    Off,
    /// 文件已打开,jdtls 启动中(spawn + initialize)
    Starting,
    /// 已连上,正在索引/导入(带进度消息);此阶段跳转可能返回空
    Indexing(String),
    /// 索引完成,可正常跳转/查引用
    Ready,
    /// jdtls 启动失败(如未安装/spawn 失败)
    Failed,
}

struct Workbench {
    focus_handle: FocusHandle,
    sidebar_view: SidebarView,
    astore_visible: bool,
    /// 可拖动面板尺寸(侧栏/Astore 宽、终端高)+ 当前拖动目标
    sidebar_width: f32,
    astore_width: f32,
    terminal_height: f32,
    resizing: Option<Resizing>,
    git_panel: Entity<GitPanel>,
    maven_panel: Entity<MavenPanel>,
    astore_panel: Entity<AstorePanel>,
    _git_sub: Subscription,
    window_handle: AnyWindowHandle,
    project_root: PathBuf,
    project_name: SharedString,
    tree_state: Entity<TreeState>,
    /// 资源管理器内部文件剪贴板:(路径列表, is_cut)。复制/剪切→存,粘贴→读。
    /// 与系统剪贴板分开:cmd+C 同时写系统剪贴板(跨应用),内部这份供 cmd+V 在树内粘贴。
    file_clipboard: Option<(Vec<PathBuf>, bool)>,
    /// 资源管理器多选集(cmd+click 切换)。非空时 复制/剪切/删除 作用于整集;
    /// 空时回落到 tree 控件的单选项。普通点击(无修饰键)清空回到单选。
    selected_paths: std::collections::HashSet<String>,
    /// 当前项目 java LSP(jdtls)的真实状态(状态栏据此显示,取代硬编码"就绪")
    lsp_phase: LspPhase,
    /// 导航历史(IDEA 式 cmd+[ 后退 / cmd+] 前进)。jump 时记离开点+目标点;
    /// 新 jump 截断 nav_index 之后的 forward 历史。nav_restoring 期间不记录。
    nav_history: Vec<NavLoc>,
    nav_index: i32,
    nav_restoring: bool,
    /// 资源管理器文件操作撤销栈(cmd+Z 时焦点在树上)
    undo_stack: Vec<UndoOp>,
    tabs: Vec<OpenTab>,
    active_tab: Option<usize>,
    /// 全项目文件清单缓存(quick-open 用;core runtime 预载)
    all_files: Arc<Vec<String>>,
    /// 最近项目(欢迎页用;启动异步加载 + 廉价类型探测)
    recents: Vec<RecentEntry>,
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
    /// md 预览左右分栏的可拖动状态(记住拖动后的比例)
    md_split_state: Entity<ResizableState>,
    terminal: Option<Entity<TerminalPanel>>,
    terminal_visible: bool,
    /// 底部面板当前 tab(问题/终端/输出)
    panel_tab: PanelTab,
    arthas: Option<Entity<ArthasPanel>>,
    arthas_visible: bool,
    expanded_paths: std::collections::HashSet<String>,
    /// git 状态标记(绝对路径→状态首字母),Explorer 树着色用(对齐旧版)
    git_marks: Arc<std::collections::HashMap<String, char>>,
    /// 工作区项目清单(对齐旧版 project-bar;会话持久化保全全部项目)
    projects: Vec<session::ProjectSession>,
    active_project: usize,
    status: SharedString,
    /// 主线程停顿哨兵计数(>32ms 漂移即记,可举证不凭感觉)
    stall_count: usize,
    /// 最近一次主线程重操作的标签+起始时刻;哨兵卡顿时据此归因「卡在哪」
    last_op: Option<(SharedString, Instant)>,
    first_frame_logged: bool,
    last_shift: Option<Instant>,
    prev_modifiers: Modifiers,
}

/// 欢迎页「最近项目」一行的展示数据(路径 + 派生的名称/缩写/类型标签/首字母色)。
struct RecentEntry {
    path: String,
    name: String,
    display_path: String,
    /// 类型标签(Rust/Maven/Gradle;探测不到为空)
    tag: &'static str,
    fav: char,
    /// 首字母图标色(取自 file_icons 同色系)
    fav_color: u32,
}

/// 从项目路径派生欢迎页展示数据:名称取末段、路径做 ~ 缩写、按 marker 文件廉价探测类型。
fn classify_recent(path: &str) -> RecentEntry {
    let p = std::path::Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let display_path = match dirs::home_dir() {
        Some(home) if path.starts_with(&*home.to_string_lossy()) => {
            format!("~{}", &path[home.to_string_lossy().len()..])
        }
        _ => path.to_string(),
    };
    let (tag, fav_color): (&'static str, u32) = if p.join("Cargo.toml").exists() {
        ("Rust", 0xd08a5c)
    } else if p.join("pom.xml").exists() {
        ("Maven", 0x7faedb)
    } else if p.join("build.gradle").exists() || p.join("build.gradle.kts").exists() {
        ("Gradle", 0x3fb950)
    } else {
        ("", 0x7a8699)
    };
    let fav = name.chars().next().unwrap_or('?').to_ascii_uppercase();
    RecentEntry {
        path: path.to_string(),
        name,
        display_path,
        tag,
        fav,
        fav_color,
    }
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
            sidebar_width: SIDEBAR_WIDTH,
            astore_width: ASTORE_WIDTH,
            terminal_height: terminal_panel::PANEL_HEIGHT,
            resizing: None,
            git_panel,
            maven_panel,
            astore_panel,
            _git_sub: git_sub,
            window_handle: window.window_handle(),
            project_root: root.clone(),
            project_name: "".into(),
            tree_state,
            file_clipboard: None,
            selected_paths: std::collections::HashSet::new(),
            lsp_phase: LspPhase::Off,
            nav_history: Vec::new(),
            nav_index: -1,
            nav_restoring: false,
            undo_stack: Vec::new(),
            tabs: Vec::new(),
            active_tab: None,
            all_files: Arc::new(Vec::new()),
            recents: Vec::new(),
            overlay: None,
            _overlay_sub: None,
            overlay_focus: cx.focus_handle(),
            watcher: Arc::new(nib_core::watch::WatcherState::default()),
            lsp: Arc::new(nib_core::lsp::LspState::default()),
            events_sink: Arc::new(ChannelSink(tx)),
            settings: session::EditorSettings::default(),
            md_preview: false,
            md_split_state: cx.new(|_| ResizableState::default()),
            terminal: None,
            terminal_visible: false,
            panel_tab: PanelTab::Terminal,
            arthas: None,
            arthas_visible: false,
            expanded_paths: std::collections::HashSet::new(),
            git_marks: Arc::new(std::collections::HashMap::new()),
            projects: Vec::new(),
            active_project: 0,
            status: "".into(),
            stall_count: 0,
            last_op: None,
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

        // 只有显式传了路径(命令行/`open <dir>`)才初始加载该项目。
        // Dock/Finder 启动的 macOS app 其 cwd = `/`,绝不能把 `/` 当项目 load_project——
        // 那会 read_dir_tree("/") + list_all_files("/") 遍历整个文件系统,撞上 ~/Desktop /
        // ~/Documents / ~/Downloads,每次启动弹一堆 TCC 授权框(还把 "/" 污染进最近项目)。
        // 无路径参数时交给下面的会话恢复加载上次项目;无会话则停在欢迎页。
        if arg_root.is_some() {
            this.load_project(root, cx);
        }

        // 加载编辑器偏好(含旧 settings.json 一次性导入),回来后应用到已开标签
        cx.spawn(async move |weak, cx| {
            let settings = session::load_settings().await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    let (wrap, folding) = (settings.word_wrap, settings.folding);
                    this.settings = settings;
                    this.apply_maven_config(cx);
                    for tab in &this.tabs {
                        tab.editor.update(cx, |state, cx| {
                            state.set_soft_wrap(wrap, window, cx);
                            state.set_folding(folding, window, cx);
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
                let ix = sess.active_project_index.min(sess.projects.len().saturating_sub(1));
                let Some(project) = sess.projects.get(ix).cloned() else {
                    return;
                };
                let _ = weak.update(cx, |this, cx| {
                    this.projects = sess.projects.clone();
                    this.active_project = ix;
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

        // 欢迎页最近项目:异步读持久化列表 + 廉价类型探测,回来填字段
        cx.spawn(async move |weak, cx| {
            let paths = session::load_recents().await;
            let entries: Vec<RecentEntry> = paths.iter().map(|p| classify_recent(p)).collect();
            let _ = weak.update(cx, |this, cx| {
                this.recents = entries;
                cx.notify();
            });
        })
        .detach();

        this
    }

    /// 切换/加载项目:重置树与文件清单(均在 core runtime 上跑,带陈旧守卫)
    fn load_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let old_root = self.project_root.to_string_lossy().to_string();
        let _ = nib_core::watch::stop_file_watcher(&old_root, &self.watcher);
        // 切项目时停掉旧项目的 jdtls。不停的话每切一次就多一个 jdtls 实例(每个
        // 1.5GB / 250% CPU),互相抢 CPU → 新项目的 initialize 30s 超时 → start_lsp
        // 返回 Err → server 不入 servers map → 跳转报 "No LSP server"(尽管 jdtls
        // 在跑);累积的实例也是卡顿的来源。LSP 状态归零,等新项目重新起。
        let new_root_str = root.to_string_lossy().to_string();
        if old_root != new_root_str && !old_root.is_empty() && old_root != "/" {
            let lsp = self.lsp.clone();
            cx.spawn(async move |_, _| {
                let _ = nib_core::lsp::stop_lsp("java".into(), old_root, &lsp).await;
            })
            .detach();
        }
        self.lsp_phase = LspPhase::Off;
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

    /// 把文件在项目内的所有祖先目录写进 expanded_paths。打开/激活文件时
    /// set_selected_item 会在控件层自动展开祖先,但这些展开**不回写** expanded_paths
    /// (控件 entries 私有读不到),于是 reload_tree 用 expanded_paths 重建时,凡是仅靠
    /// 自动展开露出的链全塌回根(尤其删掉活动文件后,连自动展开都没了→整棵塌)。
    /// 这里显式补上,让 expanded_paths 成为完整真源。
    fn reveal_ancestors(&mut self, file_path: &std::path::Path) {
        let root = self.project_root.as_path();
        let mut cur = file_path.parent();
        while let Some(dir) = cur {
            if dir == root || !dir.starts_with(root) {
                break;
            }
            self.expanded_paths.insert(dir.to_string_lossy().to_string());
            cur = dir.parent();
        }
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
        // jdtls 索引进度:驱动状态栏真实状态(替代硬编码"就绪")。
        // jdtls 分多段 $/progress,首个 end 即视为可用并保持 Ready,
        // 不被后续后台 progress 拉回"索引中"(那会让状态栏抖动且误导成不可用)。
        if let nib_core::CoreEvent::LspProgress {
            language,
            kind,
            message,
            percentage,
        } = &event
        {
            if language == "java" {
                // 只有 jdtls 发 ServiceReady(serviceReady)才算真就绪;$/progress 的
                // end 只是某个早期工作项结束,此时还在导入,不能当就绪(会误导用户早点)。
                if kind == "serviceReady" {
                    self.lsp_phase = LspPhase::Ready;
                } else if self.lsp_phase != LspPhase::Ready {
                    let label = match percentage {
                        Some(p) => format!("{} {}%", message, p),
                        None => message.clone(),
                    };
                    let label = if label.trim().is_empty() {
                        "索引中".to_string()
                    } else {
                        label
                    };
                    self.lsp_phase = LspPhase::Indexing(label);
                }
                cx.notify();
            }
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
        self.mark_op(if has_structural {
            "文件变更·重建树+git"
        } else {
            "文件变更·git刷新"
        });
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
        // 廉价先读 uri:jdtls 对整个工作区的文件发诊断,大多不是当前打开的标签。
        // 先判断文件是否打开,避免为非打开文件反序列化整个(可能很大的)诊断负载。
        let Some(uri) = params.get("uri").and_then(|v| v.as_str()) else {
            return;
        };
        let path = match uri.strip_prefix("file://") {
            Some(p) => PathBuf::from(p),
            None => return,
        };
        let Some(tab_ix) = self.tabs.iter().position(|t| t.path == path) else {
            return;
        };
        let Ok(params) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params)
        else {
            return;
        };
        // 诊断签名:jdtls 空闲时常重复推送一模一样的诊断,签名不变就整条跳过,
        // 不 reset、不重渲染——这是空闲期每秒卡顿的主因之一
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for d in &params.diagnostics {
                d.range.start.line.hash(&mut h);
                d.range.start.character.hash(&mut h);
                d.range.end.line.hash(&mut h);
                d.range.end.character.hash(&mut h);
                d.severity.map(|s| format!("{s:?}")).hash(&mut h);
                d.message.hash(&mut h);
            }
            params.diagnostics.len().hash(&mut h);
            h.finish()
        };
        if self.tabs[tab_ix].diag_sig == sig {
            return;
        }
        self.tabs[tab_ix].diag_sig = sig;
        self.mark_op(format!("LSP诊断 {}", self.tabs[tab_ix].title));
        self.tabs[tab_ix].editor.update(cx, |state, cx| {
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

    /// 记一条主线程操作面包屑(标签+时刻),卡顿哨兵据此归因「卡在哪」
    fn mark_op(&mut self, label: impl Into<SharedString>) {
        self.last_op = Some((label.into(), Instant::now()));
    }

    /// 拖动面板把手时(光标移动)实时更新对应面板尺寸(根元素挂全局 mouse_move)。
    fn on_resize_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = self.resizing else { return };
        let vp = window.viewport_size();
        let x = f32::from(event.position.x);
        let y = f32::from(event.position.y);
        match kind {
            Resizing::Sidebar => self.sidebar_width = (x - ACTIVITY_WIDTH).clamp(160., 520.),
            Resizing::Astore => self.astore_width = (f32::from(vp.width) - x).clamp(180., 560.),
            // 终端高 = 视口高 - 状态栏(24) - 光标 y
            Resizing::Terminal => {
                self.terminal_height = (f32::from(vp.height) - 24. - y).clamp(120., 640.)
            }
        }
        cx.notify();
    }

    /// 松开鼠标结束拖动。
    fn on_resize_end(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
            cx.notify();
        }
    }

    // ===== 文件树操作(右键菜单 / 快捷键) =====

    /// 当前选中的树项绝对路径(TreeItem.id 即路径)。
    fn selected_tree_path(&self, cx: &App) -> Option<String> {
        self.tree_state
            .read(cx)
            .selected_item()
            .map(|i| i.id.to_string())
    }

    /// 文件操作的目标集:多选非空→整集(去重),否则→tree 单选项(0/1 个)。
    fn target_paths(&self, cx: &App) -> Vec<String> {
        if !self.selected_paths.is_empty() {
            self.selected_paths.iter().cloned().collect()
        } else {
            self.selected_tree_path(cx).into_iter().collect()
        }
    }

    /// 树行 cmd+click:切换该路径在多选集中的去留(不打开文件)。
    fn toggle_multi_select(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.selected_paths.remove(&path) {
            self.selected_paths.insert(path);
        }
        cx.notify();
    }

    /// 普通点击(无修饰键):清空多选集,回到 tree 单选。
    fn clear_multi_select(&mut self, cx: &mut Context<Self>) {
        if !self.selected_paths.is_empty() {
            self.selected_paths.clear();
            cx.notify();
        }
    }

    /// 新建的目标目录:选中是目录→该目录;是文件→其父目录;无选中→项目根。
    fn selected_dir(&self, cx: &App) -> String {
        let root = self.project_root.to_string_lossy().to_string();
        match self.selected_tree_path(cx) {
            Some(p) => {
                let path = std::path::Path::new(&p);
                if path.is_dir() {
                    p
                } else {
                    path.parent()
                        .map(|d| d.to_string_lossy().to_string())
                        .unwrap_or(root)
                }
            }
            None => root,
        }
    }

    /// 弹出命名输入浮层(新建/重命名共用)。
    fn open_name_input(
        &mut self,
        title: impl Into<SharedString>,
        default: &str,
        op: NameOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(default.to_string()));
        // 单行输入回车即确认(订阅 PressEnter)
        let sub = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.confirm_name_input(cx);
            }
        });
        let fh = input.read(cx).focus_handle(cx);
        self.overlay = Some(Overlay::NameInput {
            input,
            title: title.into(),
            op,
        });
        self._overlay_sub = Some(sub);
        window.focus(&fh, cx);
        cx.notify();
    }

    fn on_new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.selected_dir(cx);
        self.open_name_input("新建文件", "", NameOp::NewFile(dir), window, cx);
    }

    fn on_new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.selected_dir(cx);
        self.open_name_input("新建文件夹", "", NameOp::NewFolder(dir), window, cx);
    }

    fn on_rename_item(&mut self, _: &RenameItem, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected_tree_path(cx) else {
            return;
        };
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.open_name_input("重命名", &name, NameOp::Rename(path), window, cx);
    }

    fn on_delete_item(&mut self, _: &DeleteItem, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.target_paths(cx);
        if paths.is_empty() {
            return;
        }
        // 删除的文件(或被删目录下的文件)若在标签里打开,先关掉——否则编辑器还显示已删文件。
        // starts_with 同时覆盖"删的就是该文件"和"删的是其所在目录"。高→低索引关,避免串位。
        let del: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let mut to_close: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| del.iter().any(|d| t.path.starts_with(d)))
            .map(|(i, _)| i)
            .collect();
        to_close.sort_unstable_by(|a, b| b.cmp(a));
        for ix in to_close {
            self.close_tab_at(ix, window, cx);
        }
        self.status = format!("已删除 {} 项(cmd+Z 撤销)", paths.len()).into();
        self.selected_paths.clear();
        cx.spawn(async move |weak, cx| {
            // 删除=移到废纸篓(可 cmd+Z 还原 / 也能从废纸篓找回),不永久 rm
            let mut undos = Vec::new();
            for p in paths {
                if let Ok(trashed) = nib_core::fs::move_to_trash(p.clone()).await {
                    undos.push(UndoOp::Trashed {
                        original: PathBuf::from(p),
                        trashed: PathBuf::from(trashed),
                    });
                }
            }
            let _ = weak.update(cx, |this, cx| {
                this.undo_stack.extend(undos);
                this.reload_tree(cx);
            });
        })
        .detach();
    }

    fn on_undo_file_op(&mut self, _: &UndoFileOp, _: &mut Window, cx: &mut Context<Self>) {
        let Some(op) = self.undo_stack.pop() else {
            self.status = "没有可撤销的文件操作".into();
            cx.notify();
            return;
        };
        self.status = "撤销中…".into();
        cx.notify();
        let s = |p: PathBuf| p.to_string_lossy().to_string();
        cx.spawn(async move |weak, cx| {
            match op {
                // 还原删除:从废纸篓移回原位
                UndoOp::Trashed { original, trashed } => {
                    let _ = nib_core::fs::rename_path(s(trashed), s(original)).await;
                }
                // 还原移动 / 重命名:移回原处
                UndoOp::Moved { from, to } | UndoOp::Renamed { from, to } => {
                    let _ = nib_core::fs::rename_path(s(to), s(from)).await;
                }
                // 还原新建 / 粘贴副本:移到废纸篓
                UndoOp::Created { path } => {
                    let _ = nib_core::fs::move_to_trash(s(path)).await;
                }
            }
            let _ = weak.update(cx, |this, cx| {
                this.status = "已撤销".into();
                this.reload_tree(cx);
            });
        })
        .detach();
    }

    fn on_copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.selected_tree_path(cx) {
            let _ = nib_core::clipboard::copy_text_to_clipboard(path);
            self.status = "已复制路径".into();
            cx.notify();
        }
    }

    fn on_copy_item(&mut self, _: &CopyItem, _: &mut Window, cx: &mut Context<Self>) {
        let paths = self.target_paths(cx);
        if paths.is_empty() {
            return;
        }
        // 系统剪贴板(跨应用粘贴) + 内部剪贴板(cmd+V 树内粘贴)
        let _ = nib_core::clipboard::copy_files_to_clipboard(paths.clone());
        self.file_clipboard = Some((paths.iter().map(PathBuf::from).collect(), false));
        self.status = format!("已复制 {} 项", paths.len()).into();
        cx.notify();
    }

    fn on_cut_item(&mut self, _: &CutItem, _: &mut Window, cx: &mut Context<Self>) {
        let paths = self.target_paths(cx);
        if paths.is_empty() {
            return;
        }
        self.file_clipboard = Some((paths.iter().map(PathBuf::from).collect(), true));
        self.status = format!("已剪切 {} 项", paths.len()).into();
        cx.notify();
    }

    fn on_paste_item(&mut self, _: &PasteItem, _: &mut Window, cx: &mut Context<Self>) {
        let Some((srcs, is_cut)) = self.file_clipboard.clone() else {
            return;
        };
        let dir = self.selected_dir(cx);
        self.status = if is_cut { "正在移动…" } else { "正在粘贴…" }.into();
        if is_cut {
            self.file_clipboard = None;
        }
        cx.spawn(async move |weak, cx| {
            let mut undos = Vec::new();
            for src in srcs {
                let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    continue;
                };
                let mut dest = std::path::Path::new(&dir).join(&name);
                if is_cut {
                    // 移到同目录 = no-op
                    if dest == src {
                        continue;
                    }
                    let d = dest.to_string_lossy().to_string();
                    let s = src.to_string_lossy().to_string();
                    if nib_core::fs::rename_path(s, d).await.is_ok() {
                        undos.push(UndoOp::Moved {
                            from: src.clone(),
                            to: dest,
                        });
                    }
                } else {
                    // 复制:粘到同目录(或同名)→ 生成 "xxx copy" 副本名,不再静默跳过
                    if dest == src || dest.exists() {
                        dest = unique_copy_path(&dir, &name);
                    }
                    let d = dest.to_string_lossy().to_string();
                    let s = src.to_string_lossy().to_string();
                    if nib_core::fs::copy_path(s, d).await.is_ok() {
                        undos.push(UndoOp::Created { path: dest });
                    }
                }
            }
            let _ = weak.update(cx, |this, cx| {
                this.status = "".into();
                this.undo_stack.extend(undos);
                if is_cut {
                    // 剪切的源已移走,清掉残留的多选高亮
                    this.selected_paths.clear();
                }
                this.reload_tree(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// 命名输入浮层确认(Enter):按 op 执行新建/重命名,刷新树。
    fn confirm_name_input(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::NameInput { input, op, .. }) = &self.overlay else {
            return;
        };
        let name = input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let op = op.clone();
        self.overlay = None;
        self._overlay_sub = None;
        // 执行并产出可撤销记录(新建→Created,重命名→Renamed)
        let task = async move {
            match op {
                NameOp::NewFile(dir) => {
                    let p = std::path::Path::new(&dir).join(&name);
                    nib_core::fs::create_file(p.to_string_lossy().to_string())
                        .await
                        .ok()
                        .map(|_| UndoOp::Created { path: p })
                }
                NameOp::NewFolder(dir) => {
                    let p = std::path::Path::new(&dir).join(&name);
                    nib_core::fs::create_dir(p.to_string_lossy().to_string())
                        .await
                        .ok()
                        .map(|_| UndoOp::Created { path: p })
                }
                NameOp::Rename(old) => {
                    let new = std::path::Path::new(&old)
                        .parent()
                        .map(|d| d.join(&name))
                        .unwrap_or_else(|| std::path::PathBuf::from(&name));
                    nib_core::fs::rename_path(old.clone(), new.to_string_lossy().to_string())
                        .await
                        .ok()
                        .map(|_| UndoOp::Renamed {
                            from: PathBuf::from(old),
                            to: new,
                        })
                }
            }
        };
        cx.spawn(async move |weak, cx| {
            let undo = task.await;
            let _ = weak.update(cx, |this, cx| {
                if let Some(u) = undo {
                    this.undo_stack.push(u);
                }
                this.reload_tree(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// 面板边缘的拖动把手(absolute 贴边;按下记下拖动目标,根元素的 mouse_move 接管)。
    /// 侧栏=右边、Astore=左边(竖条 col-resize);终端=顶边(横条 row-resize)。
    fn resize_handle(
        &self,
        id: &'static str,
        kind: Resizing,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let base = div()
            .id(id)
            .absolute()
            .hover(|s| s.bg(cx.theme().primary.opacity(0.4)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.resizing = Some(kind);
                    cx.notify();
                }),
            );
        match kind {
            Resizing::Sidebar => base
                .top_0()
                .bottom_0()
                .right(px(-2.5))
                .w(px(5.))
                .cursor_col_resize(),
            Resizing::Astore => base
                .top_0()
                .bottom_0()
                .left(px(-2.5))
                .w(px(5.))
                .cursor_col_resize(),
            Resizing::Terminal => base
                .left_0()
                .right_0()
                .top(px(-2.5))
                .h(px(5.))
                .cursor_row_resize(),
        }
    }

    /// 帧时/主线程停顿哨兵(RFC v2 §5.6):每 100ms 一个心跳回主线程,漂移 >32ms 视为
    /// 一次可感知停顿。除 stderr + 状态栏计数外,把当时的操作面包屑落盘 jank.log 供事后分析:
    /// 哨兵心跳排在被阻塞的主线程队列里,跑到时 last_op 正指向那个操作,op_age≈drift 即元凶。
    /// 只记原始信号(drift/op/op_age/ts),归因留到分析时,省阈值调参。
    fn start_stall_sentinel(cx: &mut Context<Self>) {
        // 日志写入口:独立线程串行 append,主线程只 send 一行(O(1)),绝不让日志器自造卡顿
        // (gpui foreground task 的 await 续体仍在主线程,同步 fs 写会卡→写→更卡的正反馈)
        let log_path = session::data_dirs().app_data.join("jank.log");
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            use std::io::Write;
            if let Some(dir) = log_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            while let Ok(line) = rx.recv() {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        });
        cx.spawn(async move |this, cx| {
            const BEAT: Duration = Duration::from_millis(100);
            const BUDGET: Duration = Duration::from_millis(32);
            // 连续卡顿合并:距上次落盘 <500ms 不重复写,防病态渲染循环灌爆日志
            const LOG_GAP: Duration = Duration::from_millis(500);
            let mut last = Instant::now();
            let mut last_log: Option<Instant> = None;
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
                        // 最近操作 = 自身埋点 vs 终端埋点,起始更晚者(更贴近这次停顿)
                        let term_op = this.terminal.as_ref().and_then(|t| t.read(cx).last_op());
                        let op = match (this.last_op.clone(), term_op) {
                            (Some(a), Some(b)) => Some(if a.1 >= b.1 { a } else { b }),
                            (a, b) => a.or(b),
                        };
                        let emit = last_log.is_none_or(|t| now.duration_since(t) > LOG_GAP);
                        if emit {
                            last_log = Some(now);
                            let ts_ms = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_millis())
                                .unwrap_or(0);
                            let (op_label, op_age): (String, i64) = match op {
                                Some((l, started)) => (
                                    l.to_string(),
                                    now.duration_since(started).as_millis() as i64,
                                ),
                                None => ("?".to_string(), -1),
                            };
                            let line = format!(
                                "{{\"ts_ms\":{ts_ms},\"drift_ms\":{},\"op\":{op_label:?},\"op_age_ms\":{op_age},\"count\":{}}}\n",
                                drift.as_millis(),
                                this.stall_count
                            );
                            let _ = tx.send(line);
                        }
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
        let label = format!(
            "打开文件 {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        self.mark_op(label);
        let lang = language_for(&path.to_string_lossy());
        let text_for_lsp = text.clone();
        let settings = self.settings.clone();
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(lang)
                // 禁用编辑器内置右键菜单,改由外层提供(含 Arthas 命令)
                .context_menu(false)
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
        // Java:起 jdtls(幂等)并 didOpen——diagnostics 会经 EventSink 流回。
        // 仅对工程内文件;反编译的库源码(临时目录)不归 jdtls 管,别触发启动/didOpen,
        // 否则会把状态栏 LSP 状态从就绪打回"启动中",且给 jdtls 灌工程外的脏文档。
        if lang == "java" && path.starts_with(&self.project_root) {
            // 还没起(或上次失败)→ 进入"启动中",让状态栏如实显示
            if self.lsp_phase == LspPhase::Off || self.lsp_phase == LspPhase::Failed {
                self.lsp_phase = LspPhase::Starting;
            }
            let lsp = self.lsp.clone();
            let sink = self.events_sink.clone();
            let root = self.project_root.to_string_lossy().to_string();
            let file = path.to_string_lossy().to_string();
            let content = text_for_lsp;
            cx.spawn(async move |weak, cx| {
                let jdtls_root = session::data_dirs().jdtls_workspaces();
                // 临时诊断:把 start_lsp 的 root 与结果落盘(/tmp/nib-goto.log),
                // 定位"jdtls 就绪却 No LSP server"=server 没入 map 的真因
                let log = |m: String| {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("/tmp/nib-goto.log")
                    {
                        let _ = writeln!(f, "{m}");
                    }
                };
                log(format!("[start_lsp] 调用 root={root}"));
                if let Err(err) = nib_core::lsp::start_lsp(
                    "java".into(),
                    root,
                    sink as Arc<dyn nib_core::EventSink>,
                    jdtls_root,
                    &lsp,
                )
                .await
                {
                    log(format!("[start_lsp] 失败 → server 不入 map: {err}"));
                    eprintln!("[nib-lsp] jdtls 启动失败: {}", err);
                    let _ = weak.update(cx, |this: &mut Workbench, cx| {
                        this.lsp_phase = LspPhase::Failed;
                        cx.notify();
                    });
                    return;
                }
                log("[start_lsp] 成功 → server 已入 map".to_string());
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
            diag_sig: 0,
        });
        self.tabs.len() - 1
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let label = format!("切换标签 {}", self.tabs[ix].title);
        self.mark_op(label);
        self.active_tab = Some(ix);
        self.tabs[ix].last_used = Instant::now();
        let path = self.tabs[ix].path.clone();
        let title = self.tabs[ix].title.clone();
        // 路径在面包屑显示;状态栏左侧留给 git/诊断(对齐设计稿),切标签清空消息
        self.status = "".into();
        let handle = self.tabs[ix].editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        // 把当前文件的祖先链写进 expanded_paths,reload_tree 后树不塌(见 reveal_ancestors)
        self.reveal_ancestors(&path);
        // 树高亮跟随当前标签(按 id 匹配,自动展开祖先;观察者对已激活文件是 no-op)
        let tree_item = TreeItem::new(path.to_string_lossy().to_string(), title);
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
                self.status = "".into();
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

    /// 当前项目状态快照(open_files/active_file)
    fn current_project_slot(&self) -> session::ProjectSession {
        session::ProjectSession {
            path: self.project_root.to_string_lossy().to_string(),
            open_files: self
                .tabs
                .iter()
                .map(|t| t.path.to_string_lossy().to_string())
                .collect(),
            active_file: self.active().map(|t| t.path.to_string_lossy().to_string()),
        }
    }

    /// 持久化:整张项目清单保全,只覆写当前项目 slot——单项目覆写会把
    /// 旧版会话里的其余项目全部丢掉(数据破坏,已修)
    fn persist_session(&mut self, _cx: &mut Context<Self>) {
        let slot = self.current_project_slot();
        match self.projects.iter().position(|p| p.path == slot.path) {
            Some(ix) => {
                self.projects[ix] = slot;
                self.active_project = ix;
            }
            None => {
                self.projects.push(slot);
                self.active_project = self.projects.len() - 1;
            }
        }
        let sess = session::PersistedSession {
            version: 1,
            projects: self.projects.clone(),
            active_project_index: self.active_project,
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
                        // 保存后立即刷新 git 改动列表 + 树徽标(不必等 watcher 500ms 防抖)
                        this.git_panel.update(cx, |p, cx| p.refresh(cx));
                        this.refresh_git_marks(cx);
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
        // 导航历史:记离开点(当前光标)+ 目标点(back/forward 触发时不记)
        self.nav_record_jump(
            NavLoc {
                path: path.clone(),
                line,
            },
            cx,
        );
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

    /// 当前光标落点(文件 + 0-based 行),用于导航历史记录离开点。
    fn current_nav_loc(&self, cx: &App) -> Option<NavLoc> {
        let tab = self.active()?;
        let line = tab.editor.read(cx).cursor_position().line;
        Some(NavLoc {
            path: tab.path.clone(),
            line,
        })
    }

    /// 记录一个导航落点:与栈顶相同则跳过;否则截断 forward 历史后追加,上限 100。
    fn nav_push(&mut self, loc: NavLoc) {
        if self.nav_index >= 0 {
            if let Some(top) = self.nav_history.get(self.nav_index as usize) {
                if *top == loc {
                    return;
                }
            }
        }
        self.nav_history.truncate((self.nav_index + 1).max(0) as usize);
        self.nav_history.push(loc);
        if self.nav_history.len() > 100 {
            self.nav_history.remove(0);
        }
        self.nav_index = self.nav_history.len() as i32 - 1;
    }

    /// jump 时(open_file_at/open_file 顶部)调用:记离开点(当前光标)+ 目标点。
    /// nav_restoring 期间(back/forward 触发的打开)不记录,否则历史会无限自增。
    fn nav_record_jump(&mut self, target: NavLoc, cx: &App) {
        if self.nav_restoring {
            return;
        }
        if let Some(leave) = self.current_nav_loc(cx) {
            self.nav_push(leave);
        }
        self.nav_push(target);
    }

    fn on_nav_back(&mut self, _: &NavBack, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav_index <= 0 {
            return;
        }
        self.nav_index -= 1;
        let loc = self.nav_history[self.nav_index as usize].clone();
        self.nav_restoring = true;
        self.open_file_at(loc.path, loc.line, 0, window, cx);
        self.nav_restoring = false;
    }

    fn on_nav_forward(&mut self, _: &NavForward, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav_index < 0 || self.nav_index + 1 >= self.nav_history.len() as i32 {
            return;
        }
        self.nav_index += 1;
        let loc = self.nav_history[self.nav_index as usize].clone();
        self.nav_restoring = true;
        self.open_file_at(loc.path, loc.line, 0, window, cx);
        self.nav_restoring = false;
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
        let files = self.all_files.clone();
        let window_handle = self.window_handle;
        self.status = "跳转定义…".into();
        cx.notify();
        cx.spawn(async move |weak, cx| {
            // 先问 jdtls;命中 file:// 用它,jdt:// 是库代码(单独提示),
            // 空/超时/出错 → 文本兜底(移植重构前逻辑,不依赖 jdtls 是否就绪)。
            let lsp_res =
                nib_core::lsp::lsp_goto_definition(file.clone(), pos.line, pos.character, &lsp)
                    .await;
            enum Goto {
                File(PathBuf, u32, u32),
                Status(String),
                NotFound,
            }
            let goto = match lsp_res {
                Ok(Some(u)) if u.uri.starts_with("file://") => {
                    let p = PathBuf::from(u.uri.strip_prefix("file://").unwrap_or(&u.uri));
                    Goto::File(p, u.line, u.character)
                }
                // 依赖 jar 里的定义:jdtls 返回 jdt://。取反编译源码 → 写临时 .java →
                // 当普通文件打开并跳到定义行(像 IDEA 跳进反编译的 .class)。
                Ok(Some(u)) if u.uri.starts_with("jdt://") => {
                    match nib_core::lsp::lsp_class_file_contents(u.uri.clone(), file.clone(), &lsp)
                        .await
                    {
                        Ok(text) => match jdt_temp_path(&u.uri) {
                            Some(path) => {
                                let w = std::fs::create_dir_all(
                                    path.parent().unwrap_or(path.as_path()),
                                )
                                .and_then(|_| std::fs::write(&path, text));
                                match w {
                                    Ok(_) => Goto::File(path, u.line, u.character),
                                    Err(e) => Goto::Status(format!("写库源码临时文件失败: {e}")),
                                }
                            }
                            None => Goto::Status(format!("无法解析库类 URI: {}", u.uri)),
                        },
                        Err(e) => Goto::Status(format!("取库源码失败: {e}")),
                    }
                }
                Ok(Some(_)) => Goto::NotFound,
                _ => match nib_core::lsp::text_fallback_definition(
                    file,
                    pos.line,
                    pos.character,
                    (*files).clone(),
                )
                .await
                {
                    Some(u) => {
                        let p = PathBuf::from(u.uri.strip_prefix("file://").unwrap_or(&u.uri));
                        Goto::File(p, u.line, u.character)
                    }
                    None => Goto::NotFound,
                },
            };
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    match goto {
                        Goto::File(path, line, character) => {
                            this.status = path.display().to_string().into();
                            this.open_file_at(path, line, character, window, cx);
                        }
                        Goto::Status(msg) => this.status = msg.into(),
                        Goto::NotFound => this.status = "未找到定义".into(),
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
        if matches!(self.overlay, Some(Overlay::NameInput { .. })) {
            self.confirm_name_input(cx);
            return;
        }
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
                // 同步打开底部 Arthas 诊断面板并定位目标(命令构造器与剪贴板同源)
                this.show_arthas(fqn, method, cmd, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 打开/复用底部 Arthas 面板,并把光标解析出的目标方法注入(供 arthas_command 调用)。
    fn show_arthas(
        &mut self,
        fqn: String,
        method: Option<String>,
        cmd: nib_core::arthas::ArthasCommand,
        cx: &mut Context<Self>,
    ) {
        let panel = match &self.arthas {
            Some(panel) => panel.clone(),
            None => {
                let panel = cx.new(|cx| ArthasPanel::new(cx));
                self.arthas = Some(panel.clone());
                panel
            }
        };
        self.arthas_visible = true;
        panel.update(cx, |p, cx| p.set_target(fqn, method, cmd, cx));
    }

    /// 底部 Arthas 面板开/关(菜单 Arthas → 诊断面板 / Ctrl+Shift+A)。
    fn on_toggle_arthas(&mut self, _: &ToggleArthas, _: &mut Window, cx: &mut Context<Self>) {
        self.arthas_visible = !self.arthas_visible;
        if self.arthas_visible && self.arthas.is_none() {
            self.arthas = Some(cx.new(|cx| ArthasPanel::new(cx)));
        }
        cx.notify();
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
    /// 把设置里的 Maven 配置(home/settings/repo)推给 Maven 面板;变了会重刷依赖树。
    fn apply_maven_config(&mut self, cx: &mut Context<Self>) {
        let home = self.settings.maven_home.clone();
        let settings = self.settings.maven_settings.clone();
        let repo = self.settings.maven_repo.clone();
        self.maven_panel
            .update(cx, |p, cx| p.set_config(home, settings, repo, cx));
    }

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
        let view = cx.new(|cx| SettingsView::new(self.settings.clone(), window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this: &mut Workbench, _, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Apply(settings) => {
                    // 设置页改动实时生效:存盘(异步)+ 热应用到已开标签,**不关闭页面**
                    // (整页设计下改动即时反馈;关闭由 Esc 负责)。
                    let settings = settings.clone();
                    let (wrap, folding) = (settings.word_wrap, settings.folding);
                    this.settings = settings.clone();
                    session::save_settings(settings);
                    this.apply_maven_config(cx);
                    for tab in &this.tabs {
                        tab.editor.update(cx, |state, cx| {
                            state.set_soft_wrap(wrap, window, cx);
                            state.set_folding(folding, window, cx);
                        });
                    }
                    this.status = "设置已更新 ✓".into();
                    cx.notify();
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
                                    this.open_project_path(root, cx);
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
                        this.open_project_path(root, cx);
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
            Some(Overlay::Diff(_))
            | Some(Overlay::Merge(_))
            | Some(Overlay::Settings(_))
            | Some(Overlay::NameInput { .. })
            | None => {}
        }
    }

    fn on_palette_down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            Some(Overlay::QuickOpen(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Search(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Usages(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Recents(p)) => p.update(cx, |p, cx| p.move_selection(1, cx)),
            Some(Overlay::Diff(_))
            | Some(Overlay::Merge(_))
            | Some(Overlay::Settings(_))
            | Some(Overlay::NameInput { .. })
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
#[allow(clippy::too_many_arguments)]
/// 把 jdt:// 库类 URI 映射到一个临时 .java 路径(按包名建目录,便于阅读和复用)。
/// 形如 jdt://contents/<jar>/<dotted.package>/<Class.java>?<query> → temp/nib-jdt-sources/<pkg path>/<Class.java>
fn jdt_temp_path(uri: &str) -> Option<PathBuf> {
    let no_query = uri.split('?').next()?;
    let after = no_query.strip_prefix("jdt://contents/")?;
    let segs: Vec<&str> = after.split('/').filter(|s| !s.is_empty()).collect();
    let class_file = *segs.last()?;
    let mut base = std::env::temp_dir();
    base.push("nib-jdt-sources");
    if segs.len() >= 2 {
        base.push(segs[segs.len() - 2].replace('.', "/"));
    }
    base.push(class_file);
    Some(base)
}

#[allow(clippy::too_many_arguments)]
fn render_tree_item(
    ix: usize,
    entry: &gpui_component::tree::TreeEntry,
    selected: bool,
    _: &mut Window,
    app: &mut App,
    marks: &std::collections::HashMap<String, char>,
    multi: &std::collections::HashSet<String>,
    weak: &WeakEntity<Workbench>,
) -> ListItem {
    let item = entry.item();
    let in_multi = multi.contains(item.id.as_ref());
    let muted = app.theme().muted_foreground;
    // 树行字号 13px(对齐设计稿 .tree{font-size:13px});gap 6px
    let row = h_flex().gap(px(6.)).items_center().text_size(px(13.));
    let row = if entry.is_folder() {
        // 文件夹:▶ 小三角(twist) + ▾展开/▸折叠(fico) + 名字(对齐设计稿 .row-t)
        let fico = if entry.is_expanded() { "▾" } else { "▸" };
        row.child(
            div()
                .w(px(14.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(9.))
                .text_color(muted)
                .child("▶"),
        )
        .child(
            div()
                .w(px(16.))
                .flex_none()
                .text_center()
                .text_size(px(11.))
                .text_color(muted)
                .child(fico),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .when(entry.depth() == 0, |s| s.font_weight(FontWeight::SEMIBOLD))
                .child(item.label.clone()),
        )
    } else {
        let meta = file_icons::file_icon_meta(&item.label);
        let mark = marks.get(item.id.as_ref()).copied();
        let name_color = match mark {
            Some('M') | Some('R') => Some(app.theme().warning),
            Some('A') | Some('U') => Some(app.theme().success),
            Some('D') => Some(app.theme().danger),
            _ if meta.dim => Some(muted),
            _ => None,
        };
        // 右侧 git 状态徽标(对齐设计稿 .gstat)
        let badge: Option<(char, Hsla)> = mark.and_then(|m| {
            let c = match m {
                'M' | 'R' => app.theme().warning,
                'A' | 'U' => app.theme().success,
                'D' => app.theme().danger,
                _ => return None,
            };
            Some((m, c))
        });
        row.child(div().w(px(14.)).flex_none())
            .child(
                div()
                    .w(px(16.))
                    .flex_none()
                    .text_center()
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(meta.color)
                    .child(meta.glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when_some(name_color, |s, c| s.text_color(c))
                    .child(item.label.clone()),
            )
            .when_some(badge, |s, (m, c)| {
                s.child(
                    div()
                        .flex_none()
                        .w(px(14.))
                        .text_center()
                        .text_size(px(11.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c)
                        .child(m.to_string()),
                )
            })
    };
    // 选中态:淡蓝底(ListItem 自带 list_active)+ 左侧 2px 蓝条(对齐设计稿 .row-t.sel::before)
    // 多选态(cmd+click):tree 不知道,自己补底+左条
    let primary = app.theme().primary;
    let list_active = app.theme().list_active;
    let id = item.id.to_string();
    ListItem::new(ix)
        .relative()
        .pl(px(8.) + px(12.) * entry.depth() as f32)
        .when(in_multi, |li| li.bg(list_active))
        .when(selected || in_multi, |li| {
            li.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(primary),
            )
        })
        // cmd+click 切换多选(stop_propagation 压住 tree 的 on_entry_click,不打开文件);
        // 普通点击清空多选回到单选(tree 自己处理打开)
        .on_mouse_down(MouseButton::Left, {
            let weak = weak.clone();
            move |ev, win, cx| {
                if ev.modifiers.platform {
                    cx.stop_propagation();
                    let id = id.clone();
                    let _ = weak.update(cx, |this, cx| {
                        this.toggle_multi_select(id, cx);
                        // stop_propagation 会阻断外层 tree-area 的聚焦 handler,
                        // 这里补聚焦,保证首次就 cmd+click 时 cmd-c/x/v 仍在分发路径上
                        this.tree_state.update(cx, |s, cx| s.focus(win, cx));
                    });
                } else if !ev.modifiers.shift {
                    let _ = weak.update(cx, |this, cx| this.clear_multi_select(cx));
                }
            }
        })
        .child(row)
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
        icon: IconName,
        view: SidebarView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.sidebar_view == view;
        let color = if active {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        };
        div()
            .id(id)
            .w(px(40.))
            .h(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(cx.theme().radius)
            .hover(|s| s.bg(cx.theme().accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.set_sidebar_view(view, cx)),
            )
            // 选中态:仅图标变亮(对齐设计稿,无蓝条/底色块)
            .child(Icon::new(icon).size(px(20.)).text_color(color))
    }

    /// 欢迎/空态页(对齐 welcome.html):品牌 + tagline + 开始/最近两栏 + 快捷键速查。
    /// 「打开文件夹」「最近项目」为真功能;「克隆/新建」无后端,渲染为不可点提案项。
    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let primary_fg = cx.theme().primary_foreground;
        let accent = cx.theme().accent;
        let secondary = cx.theme().secondary;
        let info = cx.theme().info;
        let border = cx.theme().border;
        let mono = cx.theme().mono_font_family.clone();

        // 快捷键药丸
        let kbd = {
            let mono = mono.clone();
            move |key: &str, color: Hsla| {
                div()
                    .font_family(mono.clone())
                    .text_size(px(11.))
                    .text_color(color)
                    .border_1()
                    .border_color(border)
                    .rounded(px(5.))
                    .px(px(7.))
                    .py(px(2.))
                    .child(key.to_string())
            }
        };

        // 开始栏「打开文件夹」=真功能可点
        let start_open = h_flex()
            .id("welcome-open")
            .w_full()
            .items_center()
            .gap(px(13.))
            .px(px(12.))
            .py(px(11.))
            .rounded(px(9.))
            .cursor_pointer()
            .hover(|s| s.bg(secondary))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.on_open_folder(&OpenFolder, window, cx)),
            )
            .child(
                div()
                    .w(px(34.))
                    .h(px(34.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .bg(accent)
                    .text_color(primary)
                    .text_size(px(16.))
                    .child("▤"),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child("打开文件夹…"),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(muted)
                            .child("选择本地项目根目录"),
                    ),
            )
            .child(kbd("⌘O", muted));

        // 提案项(无后端,不可点;muted + 「提案」标)
        let proposal = |glyph: &'static str, title: &'static str, sub: &'static str, hint: &'static str| {
            h_flex()
                .w_full()
                .items_center()
                .gap(px(13.))
                .px(px(12.))
                .py(px(11.))
                .rounded(px(9.))
                .child(
                    div()
                        .w(px(34.))
                        .h(px(34.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .bg(secondary)
                        .text_color(muted)
                        .text_size(px(16.))
                        .child(glyph),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .text_size(px(13.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(muted)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(muted)
                                        .border_1()
                                        .border_color(border)
                                        .rounded(px(4.))
                                        .px(px(4.))
                                        .child("提案"),
                                ),
                        )
                        .child(div().text_size(px(11.5)).text_color(muted).child(sub)),
                )
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_size(px(11.))
                        .text_color(muted)
                        .child(hint),
                )
        };

        // 最近项目行(真功能,点击开项目)
        let recents_list: Vec<AnyElement> = if self.recents.is_empty() {
            vec![div()
                .px(px(12.))
                .py(px(9.))
                .text_size(px(12.5))
                .text_color(muted)
                .child("暂无最近项目")
                .into_any_element()]
        } else {
            self.recents
                .iter()
                .filter(|r| std::path::Path::new(&r.path) != self.project_root)
                .take(8)
                .enumerate()
                .map(|(ix, r)| {
                    let path = r.path.clone();
                    h_flex()
                        .id(ix)
                        .w_full()
                        .items_center()
                        .gap(px(12.))
                        .px(px(12.))
                        .py(px(9.))
                        .rounded(px(9.))
                        .cursor_pointer()
                        .hover(|s| s.bg(secondary))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.open_project_path(PathBuf::from(path.clone()), cx)
                            }),
                        )
                        .child(
                            div()
                                .w(px(30.))
                                .h(px(30.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(7.))
                                .bg(accent)
                                .font_family(mono.clone())
                                .font_weight(FontWeight::EXTRA_BOLD)
                                .text_size(px(13.))
                                .text_color(rgb(r.fav_color))
                                .child(r.fav.to_string()),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(px(13.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(fg)
                                        .child(r.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .font_family(mono.clone())
                                        .text_color(muted)
                                        .overflow_hidden()
                                        .child(r.display_path.clone()),
                                ),
                        )
                        .when(!r.tag.is_empty(), |row| {
                            row.child(
                                div()
                                    .text_size(px(10.5))
                                    .font_family(mono.clone())
                                    .text_color(info)
                                    .border_1()
                                    .border_color(border)
                                    .rounded(px(5.))
                                    .px(px(7.))
                                    .py(px(2.))
                                    .child(r.tag),
                            )
                        })
                        .into_any_element()
                })
                .collect()
        };

        // 快捷键速查(3 列 × 3 行)
        let shortcuts: [(&str, &str); 9] = [
            ("快速打开", "⌘P"),
            ("保存", "⌘S"),
            ("全局搜索", "⇧⌘F"),
            ("跳转定义", "F12"),
            ("查找引用", "⇧F12"),
            ("切换终端", "⌃`"),
            ("Markdown 预览", "⇧⌘V"),
            ("关闭标签", "⌘W"),
            ("设置", "⌘,"),
        ];
        let kbd_for_grid = kbd.clone();
        let shortcut_rows = shortcuts.chunks(3).map(move |chunk| {
            let kbd = kbd_for_grid.clone();
            h_flex()
                .gap(px(32.))
                .children(chunk.iter().map(move |(label, key)| {
                    h_flex()
                        .w(px(232.))
                        .items_center()
                        .justify_between()
                        .gap(px(14.))
                        .child(div().text_size(px(12.5)).text_color(muted).child(*label))
                        .child(kbd(key, fg))
                }))
        });

        div()
            .id("welcome")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                v_flex()
                    .w(px(880.))
                    .px(px(48.))
                    .py(px(64.))
                    // hero
                    .child(
                        h_flex()
                            .items_end()
                            .gap(px(20.))
                            .mb(px(6.))
                            .child(
                                h_flex()
                                    .child(
                                        div()
                                            .text_size(px(64.))
                                            .font_weight(FontWeight::EXTRA_BOLD)
                                            .text_color(primary_fg)
                                            .child("Nib"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(64.))
                                            .font_weight(FontWeight::EXTRA_BOLD)
                                            .text_color(primary)
                                            .child("."),
                                    ),
                            )
                            .child(
                                div()
                                    .mb(px(8.))
                                    .text_size(px(12.))
                                    .font_family(mono.clone())
                                    .text_color(muted)
                                    .border_1()
                                    .border_color(border)
                                    .rounded_full()
                                    .px(px(10.))
                                    .py(px(3.))
                                    .child("v0.1.0"),
                            ),
                    )
                    // tagline
                    .child(
                        div()
                            .max_w(px(560.))
                            .mt(px(14.))
                            .mb(px(40.))
                            .text_size(px(14.5))
                            .text_color(muted)
                            .child("Java-first 原生代码编辑器,Rust + GPUI 构建。内置 jdtls 语言服务、完整 Git 客户端、Maven 依赖面板、集成终端,以及 Arthas 在线诊断与 Astore 内网仓库直连。"),
                    )
                    // 两栏:开始 + 最近
                    .child(
                        h_flex()
                            .gap(px(40.))
                            .mb(px(44.))
                            .items_start()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .child(
                                        div()
                                            .mb(px(14.))
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(muted)
                                            .child("开始"),
                                    )
                                    .child(
                                        v_flex()
                                            .gap(px(4.))
                                            .child(start_open)
                                            .child(proposal("⎘", "克隆 Astore 仓库…", "从内网 Astore 拉取代码", "建议 ⇧⌘C"))
                                            .child(proposal("＋", "新建文件", "空白缓冲区", "建议 ⌘N")),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .child(
                                        div()
                                            .mb(px(14.))
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(muted)
                                            .child("最近"),
                                    )
                                    .child(v_flex().gap(px(2.)).children(recents_list)),
                            ),
                    )
                    // 快捷键速查
                    .child(
                        v_flex()
                            .border_t_1()
                            .border_color(border)
                            .pt(px(26.))
                            .child(
                                div()
                                    .mb(px(16.))
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(muted)
                                    .child("常用快捷键"),
                            )
                            .child(v_flex().gap(px(10.)).children(shortcut_rows)),
                    ),
            )
    }

    /// 底部面板(对齐设计稿):问题/终端/输出 tab 栏 + 按 tab 切换内容。
    /// 问题=按文件的诊断计数(可点开;gpui-component 未开放逐条诊断 API,故只到文件级);
    /// 终端=既有终端面板;输出=占位(暂无构建/LSP 日志源)。
    fn render_bottom_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let problems: usize = self
            .tabs
            .iter()
            .map(|t| t.editor.read(cx).diagnostics().map(|d| d.len()).unwrap_or(0))
            .sum();

        let tab_btn = |id: &'static str, label: String, tab: PanelTab, cx: &mut Context<Self>| {
            let active = self.panel_tab == tab;
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded(cx.theme().radius)
                .text_size(px(12.))
                .cursor_pointer()
                .when(active, |s| s.text_color(fg).bg(cx.theme().list_active))
                .when(!active, |s| s.text_color(muted))
                .hover(|s| s.bg(cx.theme().accent))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.panel_tab = tab;
                        cx.notify();
                    }),
                )
                .child(label)
        };

        v_flex()
            .h(px(self.terminal_height))
            .relative()
            .border_t_1()
            .border_color(border)
            .child(self.resize_handle("rz-terminal", Resizing::Terminal, cx))
            .child(
                h_flex()
                    .h(px(30.))
                    .flex_none()
                    .items_center()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(tab_btn(
                        "pt-problems",
                        if problems > 0 {
                            format!("问题 {problems}")
                        } else {
                            "问题".into()
                        },
                        PanelTab::Problems,
                        cx,
                    ))
                    .child(tab_btn("pt-terminal", "终端".into(), PanelTab::Terminal, cx))
                    .child(tab_btn("pt-output", "输出".into(), PanelTab::Output, cx))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("pt-collapse")
                            .px_2()
                            .text_size(px(13.))
                            .text_color(muted)
                            .cursor_pointer()
                            .hover(|s| s.text_color(fg))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.on_toggle_terminal(&ToggleTerminal, window, cx)
                                }),
                            )
                            .child("⌄"),
                    ),
            )
            .child(div().flex_1().min_h_0().map(|c| match self.panel_tab {
                PanelTab::Terminal => {
                    if let Some(panel) = self.terminal.clone() {
                        c.child(panel)
                    } else {
                        c.child(div().p_3().text_color(muted).child("终端未启动"))
                    }
                }
                PanelTab::Problems => {
                    let rows: Vec<_> = self
                        .tabs
                        .iter()
                        .enumerate()
                        .filter_map(|(ix, t)| {
                            let n =
                                t.editor.read(cx).diagnostics().map(|d| d.len()).unwrap_or(0);
                            (n > 0).then(|| (ix, t.title.clone(), n))
                        })
                        .collect();
                    if rows.is_empty() {
                        c.child(
                            div()
                                .p_3()
                                .text_size(px(12.))
                                .text_color(cx.theme().success)
                                .child("✓ 没有问题"),
                        )
                    } else {
                        c.child(v_flex().p_1().children(rows.into_iter().map(
                            |(ix, title, n)| {
                                h_flex()
                                    .id(ix)
                                    .px_2()
                                    .py_0p5()
                                    .gap_2()
                                    .items_center()
                                    .rounded(cx.theme().radius)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().accent))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, window, cx| {
                                            this.activate_tab(ix, window, cx)
                                        }),
                                    )
                                    .child(
                                        div()
                                            .text_color(cx.theme().warning)
                                            .text_size(px(12.))
                                            .child(format!("⚠ {n}")),
                                    )
                                    .child(div().text_size(px(12.)).text_color(fg).child(title))
                            },
                        )))
                    }
                }
                PanelTab::Output => c.child(
                    div()
                        .p_3()
                        .text_size(px(12.))
                        .text_color(muted)
                        .child("输出 — 暂未接入构建/LSP 日志源"),
                ),
            }))
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

    /// 项目标签切换(对齐旧版 project-bar):先存当前 slot,再装目标并恢复其标签
    fn switch_project(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix == self.active_project || ix >= self.projects.len() {
            return;
        }
        self.persist_session(cx);
        let target = self.projects[ix].clone();
        let label = format!(
            "切换项目 {}",
            std::path::Path::new(&target.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| target.path.clone())
        );
        self.mark_op(label);
        self.active_project = ix;
        self.tabs.clear();
        self.active_tab = None;
        let root = PathBuf::from(&target.path);
        if root.exists() {
            self.load_project(root, cx);
            self.restore_tabs(target.open_files.clone(), target.active_file.clone(), cx);
        }
        self.persist_session(cx);
        cx.notify();
    }

    /// 关项目标签(最后一个不关);关的是当前项目则切到邻位
    fn close_project(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.projects.len() || self.projects.len() <= 1 {
            return;
        }
        let closing_active = ix == self.active_project;
        self.projects.remove(ix);
        if self.active_project > ix {
            self.active_project -= 1;
        }
        if closing_active {
            let next = ix.min(self.projects.len() - 1);
            let target = self.projects[next].clone();
            self.active_project = next;
            self.tabs.clear();
            self.active_tab = None;
            let root = PathBuf::from(&target.path);
            if root.exists() {
                self.load_project(root, cx);
                self.restore_tabs(target.open_files.clone(), target.active_file.clone(), cx);
            }
        }
        let sess = session::PersistedSession {
            version: 1,
            projects: self.projects.clone(),
            active_project_index: self.active_project,
        };
        session::save(&sess);
        cx.notify();
    }

    /// 打开/聚焦一个项目路径:已在清单→切过去;新路径→追加并切换
    fn open_project_path(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let path_str = root.to_string_lossy().to_string();
        self.persist_session(cx);
        if let Some(ix) = self.projects.iter().position(|p| p.path == path_str) {
            self.switch_project(ix, cx);
            return;
        }
        self.projects.push(session::ProjectSession {
            path: path_str,
            open_files: Vec::new(),
            active_file: None,
        });
        let ix = self.projects.len() - 1;
        self.active_project = ix;
        self.tabs.clear();
        self.active_tab = None;
        self.load_project(root, cx);
        self.persist_session(cx);
        cx.notify();
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
            // 面板拖动:把手按下后,根元素的 mouse_move/up 实时改尺寸(仅 resizing 时生效)
            .on_mouse_move(cx.listener(Self::on_resize_drag))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_resize_end))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_toggle_quick_open))
            .on_action(cx.listener(Self::on_toggle_search))
            .on_action(cx.listener(Self::on_open_folder))
            .on_action(cx.listener(Self::on_goto_definition))
            .on_action(cx.listener(Self::on_find_usages))
            .on_action(cx.listener(Self::on_nav_back))
            .on_action(cx.listener(Self::on_nav_forward))
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
            .on_action(cx.listener(Self::on_toggle_arthas))
            .on_action(cx.listener(Self::on_new_file))
            .on_action(cx.listener(Self::on_new_folder))
            .on_action(cx.listener(Self::on_rename_item))
            .on_action(cx.listener(Self::on_delete_item))
            .on_action(cx.listener(Self::on_copy_item))
            .on_action(cx.listener(Self::on_cut_item))
            .on_action(cx.listener(Self::on_paste_item))
            .on_action(cx.listener(Self::on_undo_file_op))
            .on_action(cx.listener(Self::on_copy_path))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .child(TitleBar::new().child(div().text_sm().child(title)))
            .when(self.projects.len() > 1, |this| {
                this.child(
                    h_flex()
                        .id("project-bar")
                        .h(px(38.))
                        .px_2()
                        .gap_1()
                        .items_center()
                        .bg(cx.theme().sidebar)
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .children(self.projects.iter().enumerate().map(|(ix, proj)| {
                            let name = std::path::Path::new(&proj.path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| proj.path.clone());
                            let active = ix == self.active_project;
                            h_flex()
                                .id(("proj-tab", ix))
                                .h(px(30.))
                                .px_3()
                                .gap_2()
                                .items_center()
                                // 平分铺满整条项目栏(对齐 IDEA 项目/编辑器标签的拉伸行为)
                                .flex_1()
                                .min_w_0()
                                .rounded(cx.theme().radius)
                                .text_size(px(13.))
                                .when(active, |s| s.bg(cx.theme().background))
                                .when(!active, |s| {
                                    s.text_color(cx.theme().muted_foreground)
                                        .hover(|s| s.bg(cx.theme().accent))
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.switch_project(ix, cx)
                                    }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_center()
                                        .child(name),
                                )
                                .when(active && self.projects.len() > 1, |s| {
                                    s.child(
                                        div()
                                            .id(("proj-close", ix))
                                            .w(px(16.))
                                            .h(px(16.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(cx.theme().radius)
                                            .text_size(px(12.))
                                            .text_color(cx.theme().muted_foreground)
                                            .hover(|s| s.bg(cx.theme().accent))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.close_project(ix, cx);
                                                }),
                                            )
                                            .child("×"),
                                    )
                                })
                        })),
                )
            })
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
                            .child(self.activity_btn("act-files", IconName::Folder, SidebarView::Files, cx))
                            .child(self.activity_btn("act-commit", IconName::Inbox, SidebarView::Commit, cx))
                            .child(self.activity_btn("act-git", IconName::Github, SidebarView::Git, cx))
                            .child(self.activity_btn("act-maven", IconName::GalleryVerticalEnd, SidebarView::Maven, cx))
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
                                    .hover(|s| s.bg(cx.theme().accent))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.on_open_settings(&OpenSettings, window, cx)
                                        }),
                                    )
                                    .child(
                                        Icon::new(IconName::Settings)
                                            .size(px(19.))
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .w(px(self.sidebar_width))
                            .relative()
                            .h_full()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .child(self.resize_handle("rz-sidebar", Resizing::Sidebar, cx))
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
                                    SidebarView::Files => this.child(
                                        div()
                                            .id("tree-area")
                                            .size_full()
                                            // 左键点击树区聚焦 tree("Tree" 上下文),cmd-c/x/v 才在按键分发路径上
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, window, cx| {
                                                    this.tree_state.update(cx, |s, cx| {
                                                        s.focus(window, cx)
                                                    });
                                                }),
                                            )
                                            // 右键菜单(作用于当前选中项;先左键选中再右键)
                                            .context_menu(|menu, _w, _c| {
                                                menu.menu("新建文件", Box::new(NewFile))
                                                    .menu("新建文件夹", Box::new(NewFolder))
                                                    .separator()
                                                    .menu("重命名", Box::new(RenameItem))
                                                    .menu("删除", Box::new(DeleteItem))
                                                    .separator()
                                                    .menu("复制", Box::new(CopyItem))
                                                    .menu("剪切", Box::new(CutItem))
                                                    .menu("粘贴", Box::new(PasteItem))
                                                    .separator()
                                                    // 撤销上一步文件操作:菜单项与焦点无关,
                                                    // 解决"粘贴后焦点在编辑器、cmd+Z 撤的是文本"
                                                    .menu("撤销上一步", Box::new(UndoFileOp))
                                                    .menu("复制路径", Box::new(CopyPath))
                                            })
                                            .child(tree(&self.tree_state, {
                                                let marks = self.git_marks.clone();
                                                let multi = self.selected_paths.clone();
                                                let weak = cx.entity().downgrade();
                                                move |ix, entry, sel, window, app| {
                                                    render_tree_item(
                                                        ix, entry, sel, window, app, &marks,
                                                        &multi, &weak,
                                                    )
                                                }
                                            })),
                                    ),
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
                                        // markdown 文件:右键菜单加"预览"项(标签随当前预览开关变化)
                                        let is_md = tab.lang == "markdown";
                                        let preview_on = self.md_preview;
                                        // 外层包一层捕获 cmd+click:编辑器点击会把光标移到点击处,
                                        // 我们在 mouse_up 时(光标已定)复用 F12 的跳转定义链路(支持跨文件开标签)。
                                        let editor_el = div()
                                            .id("editor-area")
                                            .size_full()
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(
                                                    |this, event: &MouseUpEvent, window, cx| {
                                                        if event.modifiers.platform {
                                                            this.on_goto_definition(
                                                                &GotoDefinition,
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                    },
                                                ),
                                            )
                                            // 右键菜单:(markdown)预览 + Arthas 命令 + 跳转/复制粘贴
                                            .context_menu(move |menu, _window, _cx| {
                                                let menu = menu
                                                    .menu("Watch 光标方法", Box::new(ArthasWatch))
                                                    .menu("Trace 光标方法", Box::new(ArthasTrace))
                                                    .menu("Stack 光标方法", Box::new(ArthasStack))
                                                    .menu(
                                                        "Monitor 光标方法",
                                                        Box::new(ArthasMonitor),
                                                    )
                                                    .menu(
                                                        "TimeTunnel 光标方法",
                                                        Box::new(ArthasTt),
                                                    )
                                                    .separator()
                                                    .menu("诊断面板", Box::new(ToggleArthas))
                                                    .separator()
                                                    .menu("跳转定义", Box::new(GotoDefinition))
                                                    .menu(
                                                        "复制",
                                                        Box::new(gpui_component::input::Copy),
                                                    )
                                                    .menu(
                                                        "粘贴",
                                                        Box::new(gpui_component::input::Paste),
                                                    );
                                                // markdown 文件才显示预览开关(标签随当前状态)
                                                if is_md {
                                                    menu.separator().menu(
                                                        if preview_on {
                                                            "隐藏 Markdown 预览"
                                                        } else {
                                                            "显示 Markdown 预览"
                                                        },
                                                        Box::new(ToggleMdPreview),
                                                    )
                                                } else {
                                                    menu
                                                }
                                            })
                                            .child(
                                                Input::new(&tab.editor)
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .text_size(px(self.settings.font_size))
                                                    .size_full(),
                                            );
                                        if self.md_preview && tab.lang == "markdown" {
                                            let text = tab.editor.read(cx).value();
                                            // 左右两栏可拖动分隔(h_resizable + 持久状态)
                                            this.child(
                                                h_resizable("md-split")
                                                    .with_state(&self.md_split_state)
                                                    .child(
                                                        resizable_panel()
                                                            .size(px(560.))
                                                            .child(
                                                                div()
                                                                    .size_full()
                                                                    .min_w_0()
                                                                    .child(editor_el),
                                                            ),
                                                    )
                                                    .child(
                                                        resizable_panel().size(px(560.)).child(
                                                            div()
                                                                .id("md-preview")
                                                                .size_full()
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
                                                    ),
                                            )
                                        } else {
                                            this.child(editor_el)
                                        }
                                    }
                                    None => this.child(self.render_welcome(cx)),
                                }
                            }))
                            .when(self.terminal_visible, |this| {
                                this.child(self.render_bottom_panel(cx))
                            })
                            .when(self.arthas_visible, |this| {
                                this.when_some(self.arthas.clone(), |this, panel| {
                                    this.child(
                                        div()
                                            .h(px(arthas_panel::PANEL_HEIGHT))
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
                                .w(px(self.astore_width))
                                .relative()
                                .h_full()
                                .border_l_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().sidebar)
                                .child(self.resize_handle("rz-astore", Resizing::Astore, cx))
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
                    // 状态栏左侧可点「终端」按钮(等价 ctrl+`):给底部终端一个显眼入口
                    .child(
                        div()
                            .id("status-terminal")
                            .flex_none()
                            .px_1p5()
                            .rounded(cx.theme().radius)
                            .cursor_pointer()
                            .whitespace_nowrap()
                            .hover(|s| s.bg(cx.theme().accent))
                            .when(self.terminal_visible, |s| {
                                s.text_color(cx.theme().foreground)
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.on_toggle_terminal(&ToggleTerminal, window, cx)
                                }),
                            )
                            .child(">_ 终端"),
                    )
                    .map(|bar| {
                        // 左侧对齐设计稿:分支 · git 领先/落后 · 改动数 · 诊断数
                        let (branch, ahead, behind, changes) = {
                            let gp = self.git_panel.read(cx);
                            let (a, b) = gp.ahead_behind();
                            (gp.branch(), a, b, gp.change_count())
                        };
                        // 诊断只能取总数(gpui-component 的 DiagnosticSet::iter 是 pub(crate))
                        let problems: usize = self
                            .tabs
                            .iter()
                            .map(|t| t.editor.read(cx).diagnostics().map(|d| d.len()).unwrap_or(0))
                            .sum();
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
                        .when(ahead > 0 || behind > 0, |s| {
                            s.child(
                                div()
                                    .whitespace_nowrap()
                                    .child(format!("↑{ahead} ↓{behind}")),
                            )
                        })
                        .when(changes > 0, |s| {
                            s.child(
                                div()
                                    .text_color(cx.theme().warning)
                                    .whitespace_nowrap()
                                    .child(format!("● {changes}")),
                            )
                        })
                        .when(problems > 0, |s| {
                            s.child(
                                div()
                                    .text_color(cx.theme().warning)
                                    .whitespace_nowrap()
                                    .child(format!("⚠ {problems}")),
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
                        this.child(format!("空格:{}", self.settings.tab_size))
                            .child("UTF-8")
                            .child("LF")
                    })
                    .when(!active_lang.is_empty(), |this| {
                        this.child(lang_display(active_lang))
                    })
                    .when(active_lang == "java", |this| {
                        // jdtls 真实状态(取代硬编码"就绪"):点颜色 + 文案随 lsp_phase。
                        // 索引中点黄,让用户知道此时跳转可能为空、需等待而非"坏了"。
                        let (dot, label) = match &self.lsp_phase {
                            LspPhase::Ready => (cx.theme().success, "jdtls 就绪".to_string()),
                            LspPhase::Starting => {
                                (cx.theme().warning, "jdtls 启动中".to_string())
                            }
                            LspPhase::Indexing(_) => {
                                (cx.theme().warning, "jdtls 索引中".to_string())
                            }
                            LspPhase::Failed => {
                                (cx.theme().danger, "jdtls 未启动".to_string())
                            }
                            LspPhase::Off => {
                                (cx.theme().muted_foreground, "jdtls 未连接".to_string())
                            }
                        };
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(dot))
                                .child(div().text_color(cx.theme().muted_foreground).child(label)),
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
                    Overlay::NameInput { input, title, .. } => v_flex()
                        .w(px(380.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius_lg)
                        .shadow_lg()
                        .p_3()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child(title.clone()),
                        )
                        .child(Input::new(input))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child("回车确认 · Esc 取消"),
                        )
                        .into_any_element(),
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
    // 注册 gpui-component 内置图标资源(嵌入二进制),否则 IconName::* 的 svg 图标
    // (活动栏 文件/源码/Git/Maven、设置齿轮、定位准星)无 AssetSource → 渲染空白。
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
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
            Menu::new("Go").items([
                MenuItem::action("快速打开文件…", ToggleQuickOpen),
                MenuItem::action("在项目中搜索…", ToggleSearch),
                MenuItem::action("跳转到定义", GotoDefinition),
                MenuItem::action("查找引用", FindUsages),
                MenuItem::action("Markdown 预览", ToggleMdPreview),
            ]),
            // 对齐旧版 View 菜单(Terminal 项;Git/Astore 在侧栏页签,不重复列)
            Menu::new("View").items([
                MenuItem::action("Markdown 预览  ⇧⌘V", ToggleMdPreview),
                MenuItem::action("终端  ⌃`", ToggleTerminal),
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
            KeyBinding::new("ctrl-shift-a", ToggleArthas, Some("Workbench")),
            KeyBinding::new("cmd-,", OpenSettings, Some("Workbench")),
            KeyBinding::new("enter", PaletteConfirm, Some("QuickOpen")),
            KeyBinding::new("cmd-s", SaveFile, Some("Workbench")),
            KeyBinding::new("cmd-w", CloseTab, Some("Workbench")),
            KeyBinding::new("cmd-p", ToggleQuickOpen, Some("Workbench")),
            KeyBinding::new("cmd-shift-f", ToggleSearch, Some("Workbench")),
            // 导航历史(IDEA 式):cmd+[ 后退、cmd+] 前进。
            // 编辑器("Input")默认把 cmd-[/] 绑成反缩进/缩进,且它是更深的 context——
            // 必须在 "Input" 上重绑覆盖(我的 bind_keys 在 gpui_component::init 之后,
            // 同深度按后绑优先)。缩进仍可用 Tab/Shift-Tab。再绑 "Workbench" 兜底非编辑器焦点。
            KeyBinding::new("cmd-[", NavBack, Some("Input")),
            KeyBinding::new("cmd-]", NavForward, Some("Input")),
            KeyBinding::new("cmd-[", NavBack, Some("Workbench")),
            KeyBinding::new("cmd-]", NavForward, Some("Workbench")),
            // 资源管理器文件操作:绑到 tree 控件的 "Tree" 上下文,只有焦点在树上才触发,
            // 不影响编辑器("Input" 上下文)自带的文本 cmd-c/x/v
            KeyBinding::new("cmd-c", CopyItem, Some("Tree")),
            KeyBinding::new("cmd-x", CutItem, Some("Tree")),
            KeyBinding::new("cmd-v", PasteItem, Some("Tree")),
            // 删除键(删到废纸篓,可撤销)+ cmd+Z 撤销文件操作(焦点在树上时)
            KeyBinding::new("backspace", DeleteItem, Some("Tree")),
            KeyBinding::new("delete", DeleteItem, Some("Tree")),
            KeyBinding::new("cmd-backspace", DeleteItem, Some("Tree")),
            KeyBinding::new("cmd-delete", DeleteItem, Some("Tree")),
            KeyBinding::new("cmd-z", UndoFileOp, Some("Tree")),
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
