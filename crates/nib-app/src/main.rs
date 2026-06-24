// Nib 原生工作台(M1):Warm Earth 主题 + 标签页 + quick-open + 会话恢复 + 脏标记。
// 性能纪律(RFC v2 §5):主线程零阻塞 IO——目录遍历/读写文件全部经 nib-core
// 自持 runtime,结果回主线程更新实体;异步回灌一律带陈旧守卫。

mod diff_view;
mod file_icons;
mod git_panel;
mod maven_panel;
mod merge_view;
mod preview;
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
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, TabSize},
    list::ListItem,
    menu::ContextMenuExt,
    notification::{Notification, NotificationType},
    resizable::{h_resizable, resizable_panel, ResizableState},
    tooltip::Tooltip,
    tree::{tree, TreeItem, TreeState},
    v_flex, ActiveTheme, Icon, IconName, Root, Theme, ThemeMode, ThemeRegistry, TitleBar, WindowExt,
};

use futures::StreamExt as _;
use diff_view::{DiffView, DiffViewEvent};
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
        ShowRecentProjects,
        OpenSettings,
        ArthasWatch,
        ArthasTrace,
        ArthasStack,
        ArthasMonitor,
        ArthasTt,
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
        // Git 分支右键菜单:checkout 切换到右键的那个分支(目标分支存在 GitPanel.ctx_branch)
        CheckoutBranch,
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

/// 目录树根节点 → 树控件 items(根本身不显示,显示其 children;无 children 则把根当单项)。
fn tree_items_from_node(
    node: &nib_core::fs::FileNode,
    expanded: &std::collections::HashSet<String>,
) -> Vec<TreeItem> {
    match &node.children {
        Some(children) => children
            .iter()
            .map(|c| file_node_to_tree_item(c, expanded))
            .collect(),
        None => vec![file_node_to_tree_item(node, expanded)],
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

/// Maven 未配置提醒通知的去重类型 id(同类只留一条,切项目不叠多条)
struct MavenConfigPrompt;

/// render 耗时守卫:在 Workbench::render 开头创建,作用域结束(元素树构建完)时把这次构建耗时
/// 写进共享原子量,供卡顿哨兵读取——区分卡顿是「render 构建树」还是「GPUI 内部布局/塑形/paint」。
struct RenderTimer {
    t0: Instant,
    out: Arc<std::sync::atomic::AtomicU64>,
}
impl Drop for RenderTimer {
    fn drop(&mut self) {
        self.out.store(
            self.t0.elapsed().as_micros() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// 标签上限(LRU 淘汰,脏标签豁免)
const MAX_TABS: usize = 30;

/// 侧栏宽度单源(terminal_panel 推算可用列数也用它;旧版 --sidebar-width: 256px)
pub const SIDEBAR_WIDTH: f32 = 256.;
/// 活动栏宽(旧版 --activity-width: 48px)
pub const ACTIVITY_WIDTH: f32 = 48.;

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
    /// 本标签**独立**的 didChange 防抖序号。曾用全局单计数器:400ms 内先敲 A 再敲 B
    /// 会把 A 的 didChange 取消(B 的 +seq 顶掉了 A 的 seq),导致 A 的诊断不刷新。
    /// 每标签各持各的序号后,跨标签编辑互不取消。
    lsp_change_seq: u64,
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

/// 正在拖动调整哪个面板的尺寸(侧栏宽,终端高)
#[derive(Clone, Copy, PartialEq)]
enum Resizing {
    Sidebar,
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
    /// 已连上,正在索引/导入(带进度百分比);此阶段跳转可能返回空
    Indexing(Option<u8>),
    /// 索引完成,可正常跳转/查引用
    Ready,
    /// jdtls 启动失败(如未安装/spawn 失败)
    Failed,
}

struct Workbench {
    focus_handle: FocusHandle,
    sidebar_view: SidebarView,
    /// 可拖动面板尺寸(侧栏宽、终端高)+ 当前拖动目标
    sidebar_width: f32,
    terminal_height: f32,
    resizing: Option<Resizing>,
    git_panel: Entity<GitPanel>,
    maven_panel: Entity<MavenPanel>,
    _git_sub: Subscription,
    window_handle: AnyWindowHandle,
    project_root: PathBuf,
    project_name: SharedString,
    tree_state: Entity<TreeState>,
    /// per-project 目录树缓存(FileNode),切项目时命中即时出树、避免大仓每次重 walk。
    tree_cache: std::collections::HashMap<PathBuf, nib_core::fs::FileNode>,
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
    /// 导航历史按项目各自保留(内存级,App 重启丢弃):切项目时把当前项目的
    /// (nav_history, nav_index) 存进来、取出目标项目的。不存绝对路径跨项目串台
    /// (切到 B 按 cmd+[ 跳回 A 的文件),又不像清空那样丢掉来回切换时的历史。
    nav_histories: std::collections::HashMap<PathBuf, (Vec<NavLoc>, i32)>,
    /// 资源管理器文件操作撤销栈(cmd+Z 时焦点在树上)
    undo_stack: Vec<UndoOp>,
    tabs: Vec<OpenTab>,
    active_tab: Option<usize>,
    /// 切项目/恢复会话时标签在异步读盘中。此时 active_tab 短暂为空,不能渲染欢迎页,
    /// 否则项目切换会闪一下 welcome。
    restoring_tabs: bool,
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
    /// md 预览滚动句柄:预览按标题分段渲染为子元素,点锚点 → scroll_to_top_of_item 滚到该段
    md_scroll: ScrollHandle,
    terminal: Option<Entity<TerminalPanel>>,
    terminal_visible: bool,
    /// 底部面板当前 tab(问题/终端/输出)
    panel_tab: PanelTab,
    expanded_paths: std::collections::HashSet<String>,
    /// 库源码临时文件(nib-jdt-sources)→ (真实 jdt:// URI, 来源项目根)。
    /// goto-def 进库时记下;之后在库源码里继续 goto-def / find-usages 时,用真实 jdt:// URI
    /// 经来源项目的 jdtls 解析(临时路径不被 jdtls 跟踪,但内容 = classFileContents,位置 1:1)。
    jdt_sources: std::collections::HashMap<PathBuf, (String, String)>,
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
    /// 最近一次鼠标按下的位置(窗口坐标),供引用列表浮层锚到鼠标附近弹出。
    last_mouse: Point<Pixels>,
    /// Markdown 预览防抖序号:每次击键 +1,输入停 150ms 才刷新预览(避免每键重渲染重解析)。
    md_seq: u64,
    /// 上一次 Workbench::render 构建元素树的耗时(微秒),由 RenderTimer 守卫在 render 末尾写入,
    /// 卡顿哨兵读它写进 jank.log——区分「卡在 render 构建」还是「卡在 GPUI 布局/塑形/paint」。
    render_us: Arc<std::sync::atomic::AtomicU64>,
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
        let git_sub = cx.subscribe(
            &git_panel,
            |this: &mut Workbench, _, event: &GitPanelEvent, cx| match event {
                GitPanelEvent::OpenDiff {
                    repo,
                    rel_path,
                    abs_path,
                } => this.open_diff(repo.clone(), rel_path.clone(), abs_path.clone(), cx),
                GitPanelEvent::OpenMerge { repo, rel_path } => {
                    this.open_merge(repo.clone(), rel_path.clone(), cx)
                }
                // git_panel 刷新已经跑过 git status,直接据此建改动标记,不再单独跑一次。
                GitPanelEvent::StatusUpdated(changes) => this.apply_git_marks(changes, cx),
            },
        );

        Self::start_stall_sentinel(window.window_handle(), cx);

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
            sidebar_width: SIDEBAR_WIDTH,
            terminal_height: terminal_panel::PANEL_HEIGHT,
            resizing: None,
            git_panel,
            maven_panel,
            _git_sub: git_sub,
            window_handle: window.window_handle(),
            project_root: root.clone(),
            project_name: "".into(),
            tree_state,
            tree_cache: std::collections::HashMap::new(),
            file_clipboard: None,
            selected_paths: std::collections::HashSet::new(),
            lsp_phase: LspPhase::Off,
            nav_history: Vec::new(),
            nav_index: -1,
            nav_restoring: false,
            nav_histories: std::collections::HashMap::new(),
            undo_stack: Vec::new(),
            tabs: Vec::new(),
            active_tab: None,
            restoring_tabs: false,
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
            md_scroll: ScrollHandle::new(),
            terminal: None,
            terminal_visible: false,
            panel_tab: PanelTab::Terminal,
            expanded_paths: std::collections::HashSet::new(),
            jdt_sources: std::collections::HashMap::new(),
            git_marks: Arc::new(std::collections::HashMap::new()),
            projects: Vec::new(),
            active_project: 0,
            status: "".into(),
            stall_count: 0,
            last_op: None,
            first_frame_logged: false,
            last_shift: None,
            prev_modifiers: Modifiers::default(),
            last_mouse: Point::default(),
            md_seq: 0,
            render_us: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
        // 切项目前把当前项目的导航历史存档(按项目各自保留;project_root 下面才会被改写)
        if !self.project_root.as_os_str().is_empty() {
            self.nav_histories.insert(
                self.project_root.clone(),
                (std::mem::take(&mut self.nav_history), self.nav_index),
            );
        }
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
        // 取出目标项目的导航历史(无则空;index=-1 表示无当前位置,与初始化一致)
        let (hist, ix) = self
            .nav_histories
            .remove(&root)
            .unwrap_or_else(|| (Vec::new(), -1));
        self.nav_history = hist;
        self.nav_index = ix;
        self.nav_restoring = false;
        // 资源管理器多选集 / 文件操作撤销栈都绑定旧项目的绝对路径,切项目必须清空,
        // 否则在新项目里 复制/剪切/删除 会作用到旧项目、cmd+Z 会撤销旧项目的文件操作。
        self.selected_paths.clear();
        self.undo_stack.clear();
        // 浮层都持有旧项目上下文(搜索面板绑旧 root、Usages 是旧项目的结果、重命名框针对旧
        // 项目文件),切项目一律关掉,避免在新项目里对旧项目搜索/操作。
        self.overlay = None;
        self._overlay_sub = None;
        session::remember_recent(root.to_string_lossy().to_string());
        if let Some(panel) = &self.terminal {
            panel.update(cx, |panel, cx| panel.set_project(root.clone(), cx));
        }

        self.reload_tree(cx);
        self.refresh_git_marks(cx);
        let git_root = self.project_root.clone();
        self.git_panel
            .update(cx, |panel, cx| panel.set_project(git_root.clone(), cx));
        self.maven_panel
            .update(cx, |panel, cx| panel.set_project(git_root, cx));

        // 检测到 Maven 工程但未配置 Maven → 主动弹提醒去设置(内网 amaven 工程依赖
        // 解析常需指定 settings.xml/私服;不主动提示用户不知道要去哪配)。
        self.maybe_prompt_maven_config(cx);

        // quick-open / 文本跳转兜底用的文件清单:加载时预载,结构变化时也走同一方法刷新
        self.refresh_all_files(cx);

        // 后台预热其余已开项目的目录树缓存 → 之后切到它们直接命中即时出树(大仓 depth-64 全量
        // 遍历要数秒,不预热则每个项目首次切入都要空窗等遍历)。
        self.prewarm_tree_cache(cx);
    }

    /// 后台预热所有已开项目(除当前与已缓存)的目录树缓存:逐个顺序遍历(避免多个大仓并发抢
    /// CPU/IO),完成即写 tree_cache。之后切到这些项目直接命中缓存,不在切换路径上等遍历。
    fn prewarm_tree_cache(&mut self, cx: &mut Context<Self>) {
        let current = self.project_root.clone();
        let roots: Vec<PathBuf> = self
            .projects
            .iter()
            .map(|p| PathBuf::from(&p.path))
            .filter(|r| *r != current && !self.tree_cache.contains_key(r))
            .collect();
        if roots.is_empty() {
            return;
        }
        cx.spawn(async move |weak, cx| {
            for root in roots {
                // 期间可能已被切过去时的 reload_tree 缓存,跳过避免重复遍历。
                let need = weak
                    .read_with(cx, |this, _| !this.tree_cache.contains_key(&root))
                    .unwrap_or(false);
                if !need {
                    continue;
                }
                let root_str = root.to_string_lossy().to_string();
                if let Ok(node) = nib_core::fs::read_dir_tree(root_str, Some(64)).await {
                    if weak
                        .update(cx, |this, _| {
                            this.tree_cache.insert(root.clone(), node);
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
        .detach();
    }

    /// 重建 quick-open 与文本跳转兜底用的全量文件清单。原来只在项目加载时生成一次,
    /// 新建/删除/重命名文件虽刷新了文件树,却不更新这份清单 → 新文件 quick-open 搜不到、
    /// 删除的文件仍在清单里。结构变化(has_structural)时调用,与 reload_tree 并列。
    /// core runtime 上跑 + 陈旧守卫(期间切项目则丢弃)。
    fn refresh_all_files(&mut self, cx: &mut Context<Self>) {
        let files_root = self.project_root.to_string_lossy().to_string();
        let guard_root = self.project_root.clone();
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
    /// per-project 缓存:大仓 depth-64 全量遍历要数秒,切走再回若每次重 walk、慢结果又被
    /// 陈旧守卫丢弃 → 树永不更新成该项目(实测 rateplatform 25k 文件即此症)。命中缓存即时
    /// 出树;后台重 walk 完成时**无条件**写缓存(即便已切走),下次切回即时。重建后回放展开态
    /// + 当前标签选中(set_items 会清掉两者)。
    fn reload_tree(&mut self, cx: &mut Context<Self>) {
        let root = self.project_root.clone();
        let expanded = self.expanded_paths.clone();
        let active_item = self
            .active()
            .map(|t| (t.path.to_string_lossy().to_string(), t.title.clone()));
        // 命中缓存 → 立即出树(切项目瞬间不空窗/不陈旧);未命中(项目首开)→ 先清空,
        // 避免显示上一个项目的陈旧树。两种情况下面都会后台 walk 刷新。
        let cached = self.tree_cache.get(&root).cloned();
        let items = match &cached {
            Some(node) => tree_items_from_node(node, &expanded),
            None => Vec::new(),
        };
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
            if let Some((id, title)) = &active_item {
                state.set_selected_item(Some(&TreeItem::new(id.clone(), title.clone())), cx);
            }
        });
        cx.notify();

        let tree_for_load = self.tree_state.clone();
        let root_str = root.to_string_lossy().to_string();
        let guard_root = root;
        cx.spawn(async move |weak, cx| {
            // 深度上限 64:12 会把深 Java 包(com/alibaba/.../xxx 常超 12 层)截断成空目录。
            // should_skip 已剪掉 target/node_modules 等产物目录。(真·最优是目录懒加载,属较大重构,另议。)
            let Ok(node) = nib_core::fs::read_dir_tree(root_str, Some(64)).await else {
                return;
            };
            // 无条件写缓存(即便期间已切走):下次切回即时命中。
            if weak
                .update(cx, |this, _| {
                    this.tree_cache.insert(guard_root.clone(), node.clone());
                })
                .is_err()
            {
                return;
            }
            // 陈旧守卫:期间切了项目就不再设 items(缓存已写,切回时即时显示)。
            let Ok((still_current, expanded, active_item)) = weak.read_with(cx, |this, _| {
                (
                    this.project_root == guard_root,
                    this.expanded_paths.clone(),
                    this.active()
                        .map(|t| (t.path.to_string_lossy().to_string(), t.title.clone())),
                )
            }) else {
                return;
            };
            if !still_current {
                return;
            }
            let items = tree_items_from_node(&node, &expanded);
            tree_for_load.update(cx, |state, cx| {
                state.set_items(items, cx);
                if let Some((id, title)) = active_item {
                    state.set_selected_item(Some(&TreeItem::new(id, title)), cx);
                }
            });
            // set_items 只 notify tree_state(其 observe 仅处理选中变化)→ 显式 notify Workbench 重渲读新节点。
            let _ = weak.update(cx, |_, cx| cx.notify());
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
            message: _,
            percentage,
        } = &event
        {
            if language == "java" {
                // 只有 jdtls 发 ServiceReady(serviceReady)才算真就绪;$/progress 的
                // end 只是某个早期工作项结束,此时还在导入,不能当就绪(会误导用户早点)。
                if kind == "serviceReady" {
                    self.lsp_phase = LspPhase::Ready;
                } else if self.lsp_phase != LspPhase::Ready {
                    self.lsp_phase =
                        LspPhase::Indexing((*percentage).map(|p| p.min(100) as u8));
                }
                cx.notify();
            }
            return;
        }
        let nib_core::CoreEvent::FileChanged {
            project,
            has_structural,
            paths,
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
            // 文件结构变了 → quick-open / 文本跳转兜底的文件清单也要刷新(否则新文件搜不到)。
            self.refresh_all_files(cx);
        }
        // 文件变更只需轻量刷新(branch + status + conflicts),用于改动标记/变更列表;
        // 分支列表与 50 条 log 只在面板打开/手动刷新/提交后才加载,不在每次文件事件里跑。
        // refresh_light 跑 git status 后经 StatusUpdated 事件回传 → Workbench 建改动标记,
        // 这里不再单独 refresh_git_marks(避免每次文件变更两次 git status)。
        self.git_panel.update(cx, |panel, cx| panel.refresh_light(cx));
        // 磁盘文件被外部改动 → 只重载本次突发里实际变更、且正打开的标签(定向,不再把所有
        // 已打开文件全读一遍)。git checkout / 另一编辑器 / 格式化工具改盘都走这。
        let changed: std::collections::HashSet<String> = paths.into_iter().collect();
        // 唯一的外部重载路径:只读本次实际变更、且正打开、非脏的标签(含光标恢复 + jdtls
        // didChange)。早先这里还跟着一段无差别 `for tab in &self.tabs` 全量读盘的旧实现,
        // 与本调用重复且每次文件变更都把所有打开文件读一遍 —— 即「全标签重复读盘」,已删。
        self.reload_open_tabs_from_disk(&changed, cx);
        let _ = window;
    }

    /// 编辑后防抖发 didChange,让 jdtls 重新分析 → publishDiagnostics → 边敲边报错。
    /// 仅 java 工程内文件;每次击键 +seq 重置防抖,~400ms 静默后只有 seq 未变才发,
    /// 避免每键都把整份文件写给 jdtls。
    fn schedule_did_change(&mut self, editor: Entity<InputState>, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.editor == editor) else {
            return;
        };
        if tab.lang != "java" || !tab.path.starts_with(&self.project_root) {
            return;
        }
        let path = tab.path.to_string_lossy().to_string();
        // 用本标签自己的序号:跨标签编辑不再互相取消对方的 didChange
        tab.lsp_change_seq += 1;
        let seq = tab.lsp_change_seq;
        let lsp = self.lsp.clone();
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let content = weak
                .update(cx, |this, cx| {
                    let tab = this.tabs.iter().find(|t| t.path.to_string_lossy() == path)?;
                    if tab.lsp_change_seq != seq {
                        return None; // 期间这个标签又敲了 → 放弃,让最后一次发
                    }
                    Some(tab.editor.read(cx).value().to_string())
                })
                .ok()
                .flatten();
            if let Some(content) = content {
                let _ = nib_core::lsp::lsp_did_change(path, content, &lsp).await;
            }
        })
        .detach();
    }

    /// Markdown 预览防抖刷新:输入停 150ms 后才 cx.notify 一次(让预览栏重渲染拿最新文本),
    /// 避免每次击键都整窗重渲染 + 重读全文 + 重建 TextView。
    fn schedule_md_refresh(&mut self, cx: &mut Context<Self>) {
        self.md_seq += 1;
        let seq = self.md_seq;
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let _ = weak.update(cx, |this, cx| {
                if this.md_seq == seq && this.md_preview {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 磁盘文件被外部改动 → 只重载**本次突发实际变更**且正打开、未保存改动的标签(脏标签
    /// 跳过避免覆盖编辑),保留光标。set_value 内部 emit_events=false 不会误标脏;内容没变
    /// 就不重设(常见:本应用刚保存)。changed 为空 = 没有命中已打开文件,直接返回。
    fn reload_open_tabs_from_disk(
        &mut self,
        changed: &std::collections::HashSet<String>,
        cx: &mut Context<Self>,
    ) {
        let window_handle = self.window_handle;
        let targets: Vec<(usize, String, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.dirty)
            .map(|(i, t)| {
                let java = t.lang == "java" && t.path.starts_with(&self.project_root);
                (i, t.path.to_string_lossy().to_string(), java)
            })
            .filter(|(_, path, _)| changed.contains(path))
            .collect();
        if targets.is_empty() {
            return;
        }
        for (ix, path, is_java) in targets {
            let lsp = self.lsp.clone();
            cx.spawn(async move |weak, cx| {
                let Ok(disk) = nib_core::fs::read_file(path.clone()).await else {
                    return;
                };
                let reloaded = cx
                    .update_window(window_handle, {
                        let disk = disk.clone();
                        let path = path.clone();
                        move |_, window, cx| {
                            weak.update(cx, |this, cx| {
                                let Some(tab) = this.tabs.get(ix) else {
                                    return false;
                                };
                                // 重读期间标签可能切走/变脏/换路径 → 守卫
                                if tab.dirty || tab.path.to_string_lossy() != path {
                                    return false;
                                }
                                let editor = tab.editor.clone();
                                if editor.read(cx).value() == disk {
                                    return false; // 内容没变 → 不重置光标
                                }
                                // 只对活动标签恢复光标:set_cursor_position 末尾会聚焦,
                                // 对后台标签调用会把焦点偷到看不见的编辑器上(键入跑错地方)。
                                let is_active = this.active_tab == Some(ix);
                                let pos = editor.read(cx).cursor_position();
                                editor.update(cx, |state, cx| {
                                    state.set_value(disk.clone(), window, cx);
                                    if is_active {
                                        state.set_cursor_position(pos, window, cx);
                                    }
                                });
                                cx.notify();
                                true
                            })
                            .unwrap_or(false)
                        }
                    })
                    .unwrap_or(false);
                // 重载了才告诉 jdtls 新内容(set_value 不发 didChange)
                if reloaded && is_java {
                    let _ = nib_core::lsp::lsp_did_change(path, disk, &lsp).await;
                }
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
        if !uri.starts_with("file://") {
            return;
        }
        let path = PathBuf::from(nib_core::lsp::path_from_file_uri(uri));
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
                    // 标签已填回 → 落盘正确的 open_files / active_project_index(switch_project 不再
                    // 提前 persist,空标签也覆盖到:见那里的注释)。
                    this.restoring_tabs = false;
                    this.persist_session(cx);
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

    /// Git 分支行右键菜单「checkout 切换分支」:目标分支由 GitPanel 的 context_menu 闭包写在
    /// ctx_branch,这里转交给 GitPanel 切换。context menu 的 action 经此 Workbench on_action 接住。
    fn on_checkout_branch(&mut self, _: &CheckoutBranch, _: &mut Window, cx: &mut Context<Self>) {
        self.git_panel.update(cx, |p, cx| p.checkout_context(cx));
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
    /// 侧栏=右边(竖条 col-resize);终端=顶边(横条 row-resize)。
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
    fn start_stall_sentinel(window_handle: AnyWindowHandle, cx: &mut Context<Self>) {
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
            // 心跳开始这一刻窗口是否在前台。在 timer await **之前**取一次,await **之后**再取
            // 一次:只有整段心跳期间都在前台才算真实停顿。否则后台休眠(macOS 节流定时器)再
            // 回到前台的首拍,await 后 active=true 会把整段后台时长记成一次巨停。
            let mut active_before = cx
                .update_window(window_handle, |_, window, _| window.is_window_active())
                .unwrap_or(false);
            loop {
                cx.background_executor().timer(BEAT).await;
                let active_after = cx
                    .update_window(window_handle, |_, window, _| window.is_window_active())
                    .unwrap_or(false);
                // 回到 Nib 前台 → 清掉终端响铃累积的 Dock 角标(幂等:计数为 0 时不碰 OS,
                // 故每拍安全调用,无需做上升沿判定)。哨兵循环在前台执行器=主线程,符合 AppKit 约束。
                if active_after {
                    nib_core::dock::clear_badge();
                }
                let active = active_before && active_after;
                active_before = active_after;
                let alive = this.update(cx, |this, cx| {
                    let now = Instant::now();
                    let drift = now.duration_since(last).saturating_sub(BEAT);
                    if active && drift > BUDGET {
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
                            // 上一次 render 构建树的耗时:render_us≈drift → 卡在 render 构建;
                            // render_us≪drift → 卡在 GPUI 内部布局/文本塑形/paint(render 之外)。
                            let render_us =
                                this.render_us.load(std::sync::atomic::Ordering::Relaxed);
                            let line = format!(
                                "{{\"ts_ms\":{ts_ms},\"drift_ms\":{},\"op\":{op_label:?},\"op_age_ms\":{op_age},\"render_us\":{render_us},\"count\":{}}}\n",
                                drift.as_millis(),
                                this.stall_count
                            );
                            let _ = tx.send(line);
                        }
                        // 不在此 cx.notify():检测到停顿立即触发全窗重绘 → Markdown 重解析/
                        // 文本重排 → 更卡的正反馈环(这正是长卡顿放大的源头)。stall_count 让它
                        // 在下一次自然渲染时更新即可,哨兵只负责记录、绝不自造重绘。
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
                // 光标居中(cursor_surrounding_lines)默认关:开着会让 layout_cursor 每帧把光标往
                // 半视口安全区赶,点击任意位置都把视图滚去居中(very annoying)。改成动态:cmd-f
                // 打开查找时才调高(搜索结果居中,见 on_editor_search),点击编辑器即调回(见 on_mouse_up)。
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
                // Markdown 预览防抖刷新:每键都 cx.notify 会让 Workbench 整窗重渲染 + 重读
                // 编辑器全文 + 重建 TextView,输入停 150ms 才刷一次(编辑器自身仍实时更新)。
                if this.md_preview {
                    this.schedule_md_refresh(cx);
                }
                // 实时诊断:防抖后把改动发给 jdtls,边敲边报错(不必等保存)
                this.schedule_did_change(editor_for_sub.clone(), cx);
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
            // 把用户配的 settings.xml 喂给 jdtls(空=jdtls 回退默认 ~/.m2/settings.xml),
            // 让 Java 索引/跳转与依赖树面板走同一套 Maven 私服配置。
            let maven_settings = settings.maven_settings.clone();
            let maven_offline = settings.maven_offline;
            cx.spawn(async move |weak, cx| {
                let jdtls_root = session::data_dirs().jdtls_workspaces();
                if let Err(err) = nib_core::lsp::start_lsp(
                    "java".into(),
                    root,
                    sink as Arc<dyn nib_core::EventSink>,
                    jdtls_root,
                    maven_settings,
                    maven_offline,
                    &lsp,
                )
                .await
                {
                    eprintln!("[nib-lsp] jdtls 启动失败: {}", err);
                    let _ = weak.update(cx, |this: &mut Workbench, cx| {
                        this.lsp_phase = LspPhase::Failed;
                        cx.notify();
                    });
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
            diag_sig: 0,
            lsp_change_seq: 0,
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

    /// Cmd+Q / 菜单退出:**同步**落盘当前项目的标签再退。普通 persist 的写盘是异步
    /// fire-and-forget,cx.quit() 会赶在写盘前杀进程 → 最后一次 session 丢失(重启后该项目标签全没)。
    fn on_quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
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
        session::save_sync(&sess);
        cx.quit();
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
                        // 保存后立即轻量刷新 git 改动列表 + 树徽标(不必等 watcher 防抖)。
                        // refresh_light 经 StatusUpdated 已驱动改动标记,不再单独 refresh_git_marks
                        // (原来两条都各跑一次 git status,重复)。分支/log 不受单文件保存影响,不刷。
                        this.git_panel.update(cx, |p, cx| p.refresh_light(cx));
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

    /// 跳转定位时让目标行一次性落到视口中部附近。不能先 set_cursor_position 再下一帧
    /// 手动 set_scroll_offset:编辑器自身会先 reveal 光标,随后我们再居中,视觉上就会"跳一下"。
    fn set_jump_cursor(
        editor: Entity<InputState>,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::Position;

        editor.update(cx, |state, cx| {
            state.set_cursor_surrounding_lines(Some(9999), window, cx);
            state.set_cursor_position(Position::new(line, column), window, cx);
        });
        window.on_next_frame(move |window, cx| {
            editor.update(cx, |state, cx| {
                state.set_cursor_surrounding_lines(None, window, cx);
            });
        });
    }

    fn center_editor_line_now(editor: &Entity<InputState>, line: u32, cx: &mut App) -> bool {
        editor.update(cx, |state, cx| {
            let Some(line_height) = state.line_height() else {
                return false;
            };
            let visible_rows = state.visible_row_range().map(|range| range.len()).unwrap_or(20);
            let target_top = (line as i64 - visible_rows as i64 / 2).max(0) as f32;
            let mut offset = state.scroll_offset();
            offset.y = px(-(target_top * f32::from(line_height)));
            state.set_scroll_offset(offset, cx);
            true
        })
    }

    fn center_editor_line(editor: Entity<InputState>, line: u32, window: &mut Window) {
        window.on_next_frame(move |window, cx| {
            if !Self::center_editor_line_now(&editor, line, cx) {
                let editor = editor.clone();
                window.on_next_frame(move |_, cx| {
                    Self::center_editor_line_now(&editor, line, cx);
                });
            }
        });
    }

    fn set_centered_jump_cursor(
        editor: Entity<InputState>,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::Position;

        editor.update(cx, |state, cx| {
            state.set_cursor_position(Position::new(line, column), window, cx);
        });
        Self::center_editor_line(editor, line, window);
    }

    fn open_file_at(
        &mut self,
        path: PathBuf,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_at_impl(path, line, column, false, window, cx);
    }

    fn open_file_at_centered(
        &mut self,
        path: PathBuf,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_at_impl(path, line, column, true, window, cx);
    }

    fn open_file_at_impl(
        &mut self,
        path: PathBuf,
        line: u32,
        column: u32,
        centered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                if centered {
                    Self::set_centered_jump_cursor(tab.editor.clone(), line, column, window, cx);
                } else {
                    Self::set_jump_cursor(tab.editor.clone(), line, column, window, cx);
                }
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
                            if centered {
                                Self::set_centered_jump_cursor(
                                    tab.editor.clone(),
                                    line,
                                    column,
                                    window,
                                    cx,
                                );
                            } else {
                                Self::set_jump_cursor(tab.editor.clone(), line, column, window, cx);
                            }
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
    /// 双击落在行末 / 行内空白(无单词)→ 整行选中。判据:gpui-component 的 select_word 在无词处
    /// 直接早退、不改选区,故双击后 selected_range 为空即「没选中词」→ 改成选整行;双击单词时选区
    /// 非空 → 保持选词,不动。机制:InputState 无公开「设选区」API,故 set_cursor_position 到行首
    /// (公开、同步、且会聚焦编辑器),再派发 SelectToEndOfLine 扩到行尾。
    /// cmd-f 打开编辑器内查找 → 把当前编辑器的光标居中打开,让搜索匹配跳到屏幕中间。
    /// Search action 先派发到聚焦的编辑器(开查找面板)、再冒泡到 Workbench 这里,故此时查找已打开;
    /// 点击编辑器时由 on_mouse_up 关掉(离开搜索语境,避免点击都滚去居中)。
    fn on_editor_search(
        &mut self,
        _: &gpui_component::input::Search,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.active() {
            let editor = tab.editor.clone();
            editor.update(cx, |s, cx| {
                s.set_cursor_surrounding_lines(Some(9999), window, cx)
            });
        }
    }

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
        let title = tab.title.to_string();
        let lsp = self.lsp.clone();
        let files = self.all_files.clone();
        let window_handle = self.window_handle;
        let project_root = self.project_root.to_string_lossy().to_string();
        // 当前文件若是已记录映射的库源码临时文件 → 它的真实 jdt:// URI + 来源项目根
        let jdt_src = self.jdt_sources.get(tab.path.as_path()).cloned();
        self.status = "跳转定义…".into();
        cx.notify();
        cx.spawn(async move |weak, cx| {
            let in_project = file.starts_with(&project_root);
            // 1) 求定义:项目文件问其 jdtls;库源码临时文件优先用真实 jdt:// URI 经来源项目
            //    jdtls(方法/类型/字段都行);无映射再退 workspace/symbol(只大写类型名)。
            let lsp_res = if in_project {
                nib_core::lsp::lsp_goto_definition(file.clone(), pos.line, pos.character, &lsp).await
            } else if let Some((jdt_uri, proj)) = &jdt_src {
                nib_core::lsp::lsp_definition_via_jdt(
                    jdt_uri.clone(),
                    file.clone(),
                    pos.line,
                    pos.character,
                    proj.clone(),
                    &lsp,
                )
                .await
            } else {
                nib_core::lsp::lsp_workspace_symbol_definition(
                    file.clone(),
                    pos.line,
                    pos.character,
                    project_root.clone(),
                    &lsp,
                )
                .await
            };
            enum Goto {
                File(PathBuf, u32, u32, Option<(String, String)>),
                Usages(Vec<nib_core::lsp::LspUsage>),
                Status(String),
                NotFound,
            }
            // 2) 声明检测:definition 落回光标所在文档的同一行 = 你正站在声明上 →
            //    改成 find-usages(IDEA 语义:在定义处"跳转" = 看谁调用了它)。
            let on_decl = match &lsp_res {
                Ok(Some(u)) if u.line == pos.line => {
                    if in_project {
                        nib_core::lsp::path_from_file_uri(&u.uri) == file
                    } else {
                        // 库类身份在 jdt:// URI 的 '?' 前(后面是 jar/项目元数据,格式可能有出入)
                        let key = |s: &str| s.split('?').next().unwrap_or(s).to_string();
                        jdt_src.as_ref().is_some_and(|(ju, _)| key(&u.uri) == key(ju))
                    }
                }
                _ => false,
            };
            // jdt:// 取源码 / find-references 都要按"项目"定位 server(库临时文件拿自身会落空)。
            let lsp_ctx = if in_project {
                file.clone()
            } else {
                project_root.clone()
            };
            let goto = if on_decl {
                let refs = if in_project {
                    nib_core::lsp::lsp_find_references(file.clone(), pos.line, pos.character, &lsp)
                        .await
                } else if let Some((jdt_uri, proj)) = &jdt_src {
                    nib_core::lsp::lsp_references_via_jdt(
                        jdt_uri.clone(),
                        file.clone(),
                        pos.line,
                        pos.character,
                        proj.clone(),
                        &lsp,
                    )
                    .await
                } else {
                    Ok(Vec::new())
                };
                match refs {
                    Ok(u) if !u.is_empty() => Goto::Usages(u),
                    _ => Goto::NotFound,
                }
            } else {
                match lsp_res {
                    Ok(Some(u)) if u.uri.starts_with("file://") => {
                        let p = PathBuf::from(nib_core::lsp::path_from_file_uri(&u.uri));
                        Goto::File(p, u.line, u.character, None)
                    }
                    // 依赖 jar 里的定义:jdtls 返回 jdt://。取反编译源码 → 写临时 .java →
                    // 当普通文件打开并跳到定义行(像 IDEA 跳进反编译的 .class);记下
                    // 临时文件 → (jdt:// URI, 来源项目)映射,供之后在库源码里继续跳/查引用。
                    Ok(Some(u)) if u.uri.starts_with("jdt://") => {
                        match nib_core::lsp::lsp_class_file_contents(
                            u.uri.clone(),
                            lsp_ctx.clone(),
                            &lsp,
                        )
                        .await
                        {
                            Ok(text) => match jdt_temp_path(&u.uri) {
                                Some(path) => {
                                    // 反编译源码可达数百 KB,写盘放后台线程,别阻塞 GPUI 主线程任务。
                                    let w = {
                                        let p = path.clone();
                                        cx.background_executor()
                                            .spawn(async move {
                                                std::fs::create_dir_all(
                                                    p.parent().unwrap_or(p.as_path()),
                                                )
                                                .and_then(|_| std::fs::write(&p, text))
                                            })
                                            .await
                                    };
                                    match w {
                                        Ok(_) => Goto::File(
                                            path,
                                            u.line,
                                            u.character,
                                            Some((u.uri.clone(), lsp_ctx.clone())),
                                        ),
                                        Err(e) => {
                                            Goto::Status(format!("写库源码临时文件失败: {e}"))
                                        }
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
                            let p = PathBuf::from(nib_core::lsp::path_from_file_uri(&u.uri));
                            Goto::File(p, u.line, u.character, None)
                        }
                        None => Goto::NotFound,
                    },
                }
            };
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    match goto {
                        Goto::File(path, line, character, jdt) => {
                            // 来自 jdt:// 的库源码 → 记下映射,之后在其中继续跳/查引用走 jdt://
                            if let Some(m) = jdt {
                                this.jdt_sources.insert(path.clone(), m);
                            }
                            this.status = path.display().to_string().into();
                            this.open_file_at(path, line, character, window, cx);
                        }
                        // 光标在声明上 → 看用法:声明被 references 排除,无"当前用法",选 0
                        Goto::Usages(usages) => {
                            this.show_usages(title, usages, None, window, cx)
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

    /// 抽取 jdt:// 库类源码到临时文件并打开到指定行(供 goto-def / 引用列表里点库类内引用复用)。
    /// 记下临时文件 →(jdt:// URI, 来源项目)映射,之后在其中还能继续跳/查引用。
    fn open_jdt_source(&mut self, jdt_uri: String, line: u32, character: u32, cx: &mut Context<Self>) {
        let lsp = self.lsp.clone();
        let ctx = self.project_root.to_string_lossy().to_string();
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            let Ok(text) =
                nib_core::lsp::lsp_class_file_contents(jdt_uri.clone(), ctx.clone(), &lsp).await
            else {
                return;
            };
            let Some(path) = jdt_temp_path(&jdt_uri) else {
                return;
            };
            if std::fs::create_dir_all(path.parent().unwrap_or(path.as_path()))
                .and_then(|_| std::fs::write(&path, &text))
                .is_err()
            {
                return;
            }
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    this.jdt_sources.insert(path.clone(), (jdt_uri.clone(), ctx.clone()));
                    this.open_file_at(path, line, character, window, cx);
                });
            });
        })
        .detach();
    }

    /// 弹"引用列表"浮层(find-usages 与"在声明处跳转→看用法"共用)。
    fn show_usages(
        &mut self,
        title: String,
        usages: Vec<nib_core::lsp::LspUsage>,
        origin: Option<(String, u32, u32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.status = format!("{} 处引用", usages.len()).into();
        let view = cx.new(|cx| UsagesView::new(title, usages, origin, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this: &mut Workbench, _, event: &UsagesEvent, window, cx| match event {
                UsagesEvent::Open {
                    uri,
                    line,
                    character,
                } => {
                    let (uri, line, character) = (uri.clone(), *line, *character);
                    this.close_palette(window, cx);
                    if uri.starts_with("jdt://") {
                        // 库类内的引用 → 抽取库源码再打开(同 goto-def 的 jdt:// 处理)
                        this.open_jdt_source(uri, line, character, cx);
                    } else {
                        let p = PathBuf::from(nib_core::lsp::path_from_file_uri(&uri));
                        this.open_file_at(p, line, character, window, cx);
                    }
                }
            },
        );
        self.overlay = Some(Overlay::Usages(view));
        self._overlay_sub = Some(sub);
        window.focus(&self.overlay_focus, cx);
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
        // 光标当前所在的(文件, line, character):交给浮层把初始高亮定位到光标所在的引用
        let origin = Some((file.clone(), pos.line, pos.character));
        cx.spawn(async move |weak, cx| {
            let result =
                nib_core::lsp::lsp_find_references(file, pos.line, pos.character, &lsp).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    match result {
                        Ok(usages) if !usages.is_empty() => {
                            this.show_usages(title.clone(), usages, origin.clone(), window, cx);
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
            }),
            SidebarView::Git => self.git_panel.update(cx, |p, cx| {
                p.set_mode(GitPanelMode::Branches, cx);
            }),
            _ => {}
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

    /// 预览里点 `#章节` 锚点 → 把对应章节段滚到顶部(由 main 的 URL 轮询回灌调用)。slug 与各段
    /// 标题都归一化后比对(跨 GitHub/GitLab/手写 slug 差异)。段序与渲染时 split_into_sections 一致。
    fn scroll_md_to_anchor(&mut self, slug: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.active() else { return };
        if tab.lang != "markdown" {
            return;
        }
        let raw = tab.editor.read(cx).value();
        let sections = nib_core::markdown::split_into_sections(&raw);
        if let Some(ix) = nib_core::markdown::find_section_for_anchor(&sections, slug) {
            self.md_scroll.scroll_to_top_of_item(ix);
            cx.notify();
        } else {
            // 锚点没匹配到任何章节(目标不存在 / slug 风格差异过大)→ 状态栏提示,不静默
            self.status = format!("未找到章节: #{slug}").into();
            cx.notify();
        }
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

    /// 检测到根 pom.xml 且 Maven 三项配置全空 → 弹一条带「去设置」按钮的提醒。
    /// 触发条件取「未配置」而非「mvn 跑不起来」:用户明确要的是"有 pom + 没配 Maven
    /// 就提醒"(像 IDEA);配了任意一项就认为用户已知晓,不再打扰。带类型 id 去重,
    /// 切项目反复进 load_project 不会叠多条。
    /// 配置从持久化文件**异步重读**,不信内存 self.settings——启动恢复项目(load_project)
    /// 与 settings 异步加载是两个独立 spawn,谁先不定;读内存可能拿到还没加载完的默认空值,
    /// 对已配置用户误弹。
    fn maybe_prompt_maven_config(&mut self, cx: &mut Context<Self>) {
        let pom = self.project_root.join("pom.xml");
        let window_handle = self.window_handle;
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            if !pom.exists() {
                return;
            }
            let s = session::load_settings().await;
            let configured = !s.maven_home.trim().is_empty()
                || !s.maven_settings.trim().is_empty()
                || !s.maven_repo.trim().is_empty();
            if configured {
                return;
            }
            let _ = cx.update_window(window_handle, move |_, window, cx| {
                window.push_notification(
                    Notification::new()
                        .id::<MavenConfigPrompt>()
                        .with_type(NotificationType::Warning)
                        .title("检测到 Maven 工程")
                        .message(
                            "尚未配置 Maven。依赖解析失败时(尤其内网 amaven),\
                             到设置里指定 Maven home / settings.xml / 本地仓库。",
                        )
                        .action(move |_, _, _| {
                            let weak = weak.clone();
                            Button::new("goto-maven-settings")
                                .primary()
                                .label("去设置")
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |wb, cx| {
                                        wb.open_settings_at_maven(window, cx);
                                    });
                                })
                        }),
                    cx,
                );
            });
        })
        .detach();
    }

    /// 打开设置页并直接切到 Maven 分类(给 Maven 提醒的「去设置」按钮用)。
    fn open_settings_at_maven(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Some(Overlay::Settings(_))) {
            self.on_open_settings(&OpenSettings, window, cx);
        }
        if let Some(Overlay::Settings(view)) = &self.overlay {
            view.update(cx, |v, cx| v.show_maven(cx));
        }
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
                    // 配置已填任意一项 → 移除「检测到 Maven 工程·未配置」提醒(它带「去设置」
                    // 按钮、不自动隐藏,否则配完仍挂在右上角不走)。
                    if !this.settings.maven_home.trim().is_empty()
                        || !this.settings.maven_settings.trim().is_empty()
                        || !this.settings.maven_repo.trim().is_empty()
                    {
                        window.remove_notification::<MavenConfigPrompt>(cx);
                    }
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
    fn open_diff(&mut self, repo: String, rel_path: String, abs_path: PathBuf, cx: &mut Context<Self>) {
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            // 多仓:diff 必须在该文件所属仓的 cwd 跑(嵌套仓的文件不在根仓 HEAD 里)。rel_path 已相对该仓。
            let diff = nib_core::diff::diff_file_against_head(repo.clone(), rel_path.clone()).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut Workbench, cx| {
                    let diff = match diff {
                        Ok(diff) => diff,
                        Err(err) => {
                            this.status = err.into();
                            cx.notify();
                            return;
                        }
                    };
                    let view = cx.new(|_| DiffView::new(rel_path.clone(), abs_path.clone(), diff));
                    let repo_for_revert = repo.clone();
                    let sub = cx.subscribe_in(
                        &view,
                        window,
                        move |this: &mut Workbench, _, event: &DiffViewEvent, window, cx| match event {
                            DiffViewEvent::OpenFile(path) => {
                                let path = path.clone();
                                this.close_palette(window, cx);
                                this.open_file(path, cx);
                            }
                            DiffViewEvent::RevertHunk {
                                path,
                                new_start,
                                new_count,
                                old_content,
                            } => {
                                // 把工作区文件该 hunk 区间替换回旧内容,写盘后重算 diff+刷新 git。
                                // 编辑器标签靠 watcher 自动重载。
                                let path = path.clone();
                                let (new_start, new_count) = (*new_start, *new_count);
                                let old_content = old_content.clone();
                                let rel = path
                                    .strip_prefix(std::path::Path::new(&repo_for_revert))
                                    .ok()
                                    .map(|p| p.to_string_lossy().to_string());
                                let repo_inner = repo_for_revert.clone();
                                let window_handle = this.window_handle;
                                cx.spawn(async move |weak, cx| {
                                    let p = path.to_string_lossy().to_string();
                                    let Ok(content) = nib_core::fs::read_file(p.clone()).await
                                    else {
                                        return;
                                    };
                                    let trailing_nl = content.ends_with('\n');
                                    let mut lines: Vec<String> =
                                        content.lines().map(|s| s.to_string()).collect();
                                    let start = new_start.saturating_sub(1).min(lines.len());
                                    let end = (start + new_count).min(lines.len());
                                    lines.splice(start..end, old_content);
                                    let mut out = lines.join("\n");
                                    if trailing_nl {
                                        out.push('\n');
                                    }
                                    if nib_core::fs::write_file(p, out).await.is_err() {
                                        return;
                                    }
                                    let _ = cx.update_window(window_handle, |_, _, cx| {
                                        let _ = weak.update(cx, |this: &mut Workbench, cx| {
                                            this.git_panel.update(cx, |g, cx| g.refresh(cx));
                                            this.refresh_git_marks(cx);
                                            if let Some(rel) = rel.clone() {
                                                this.open_diff(
                                                    repo_inner.clone(),
                                                    rel,
                                                    path.clone(),
                                                    cx,
                                                );
                                            }
                                        });
                                    });
                                })
                                .detach();
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
    fn open_merge(&mut self, repo: String, rel_path: String, cx: &mut Context<Self>) {
        // 多仓:merge 在冲突文件所属仓的 cwd 跑。
        let view = cx.new(|cx| MergeView::new(repo, rel_path, cx));
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
                    this.open_file_at_centered(path, line, column, window, cx);
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
    // [圆润] IDEA 新 UI 风:选中/悬浮为圆角 pill(左右内缩 6px + rounded),去掉左侧 2px 竖条。
    // 选中底色由 ListItem 内部按 selected 画(作用在 base,随 mx+rounded 内缩成圆角);
    // 多选态(cmd+click)tree 不知道,自己补 list_active 底(同样内缩圆角)。
    let list_active = app.theme().list_active;
    let id = item.id.to_string();
    ListItem::new(ix)
        .relative()
        .selected(selected)
        .mx(px(6.))
        .rounded(app.theme().radius)
        .pl(px(8.) + px(12.) * entry.depth() as f32)
        .when(in_multi, |li| li.bg(list_active))
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
    /// 活动栏按钮(40×40)。[圆润] 激活态 = IDEA 新 UI 圆角填充高亮(蓝色调,区别于
    /// hover 的中性 accent),取代旧的「仅图标变亮」。tip=悬浮工具提示;badge=右下角角标
    /// 数(>0 才显,如 Git 改动数)。
    fn activity_btn(
        &self,
        id: &'static str,
        icon: IconName,
        view: SidebarView,
        tip: &'static str,
        badge: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.sidebar_view == view;
        let color = if active {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        };
        let warning = cx.theme().warning;
        // 角标描边/文字用 gutter 暗色(对齐蓝本 .badge:warning 底 + 深色字 + sidebar 描边)
        let gutter = cx.theme().status_bar;
        div()
            .id(id)
            .relative()
            .w(px(40.))
            .h(px(40.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(cx.theme().radius)
            .when(active, |s| s.bg(cx.theme().list_active))
            .when(!active, |s| s.hover(|s| s.bg(cx.theme().accent)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.set_sidebar_view(view, cx)),
            )
            .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
            .child(Icon::new(icon).size(px(20.)).text_color(color))
            .when_some(badge.filter(|n| *n > 0), |s, n| {
                s.child(
                    div()
                        .absolute()
                        .right(px(3.))
                        .bottom(px(3.))
                        .min_w(px(14.))
                        .h(px(14.))
                        .px(px(3.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.))
                        .bg(warning)
                        .border_2()
                        .border_color(gutter)
                        .text_size(px(9.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(gutter)
                        .child(if n > 99 { "99+".to_string() } else { n.to_string() }),
                )
            })
    }

    /// 侧栏头部动作小按钮(22×22,文字字形 + 工具提示;悬浮整条侧栏才显)。
    /// 复用已有 action/方法(新建文件/文件夹/定位),不新增后端。
    fn side_action_btn(
        &self,
        id: &'static str,
        glyph: &'static str,
        tip: &'static str,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .w(px(22.))
            .h(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(cx.theme().radius)
            .text_size(px(13.))
            .text_color(cx.theme().muted_foreground)
            .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| on_click(this, window, cx)),
            )
            .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
            .child(glyph)
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
                            .child("Java-first 原生代码编辑器,Rust + GPUI 构建。内置 jdtls 语言服务、完整 Git 客户端、Maven 依赖面板、集成终端,以及 Arthas 命令一键复制。"),
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

    fn render_project_loading(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(cx.theme().background)
            .text_color(cx.theme().muted_foreground)
            .text_size(px(12.))
            .child(format!("正在切换到 {} …", self.project_name))
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
                    // CC 在本项目跑完一轮、还没回看终端 → 终端页签后红点(聚焦终端即清)
                    .when(
                        self.terminal
                            .as_ref()
                            .is_some_and(|t| t.read(cx).cc_done()),
                        |s| {
                            s.child(
                                div()
                                    .w(px(7.))
                                    .h(px(7.))
                                    .rounded_full()
                                    .bg(cx.theme().danger),
                            )
                        },
                    )
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
                        // 把内容区实际高(面板高 - 30 顶栏 - 1 边框)推给终端,让它按真实高算 grid 行数;
                        // 否则恒按 const 220 → 拖高后网格仍 ~12 行,TUI(如 Claude Code)展示不全。
                        let content_h = (self.terminal_height - 31.).max(60.);
                        panel.update(cx, |p, _| p.set_height(content_h));
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
        // tooltip 显完整相对路径(标签名被省略号截断后的兜底)
        let rel_path: SharedString = tab
            .path
            .strip_prefix(&self.project_root)
            .unwrap_or(&tab.path)
            .to_string_lossy()
            .to_string()
            .into();
        h_flex()
            .id(ix)
            // [A3/圆润] IDEA 风圆角标签卡片:高 30 底对齐、顶两角圆、去硬分隔线(改间隙)。
            // 仍保留 flex_1 拉伸(刻意的 IDEA 行为);min_w(128) 防多标签挤成丝。
            .h(px(30.))
            .px_3()
            .gap_2()
            .items_center()
            .flex_1()
            .min_w(px(128.))
            .rounded_t(cx.theme().radius)
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
            .tooltip(move |window, cx| Tooltip::new(rel_path.clone()).build(window, cx))
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(icon.color)
                    .child(icon.glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
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

    /// 面包屑(旧版 #breadcrumb):项目名 › 相对路径段 › 文件名。
    /// [A4] 每个目录段做成可悬浮 chip,点击在文件树中定位该目录;文件段带图标、不可点。
    fn render_breadcrumb(&self, path: &std::path::Path, cx: &mut Context<Self>) -> impl IntoElement {
        let rel = path
            .strip_prefix(&self.project_root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.to_path_buf());
        let comps: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let accent = cx.theme().accent;
        let radius = cx.theme().radius;

        let mut els: Vec<AnyElement> = Vec::new();
        // 项目名段(导航到项目根)
        {
            let abs = self.project_root.clone();
            els.push(
                div()
                    .id("cb-root")
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(radius)
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|s| s.bg(accent).text_color(fg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.locate_path_in_tree(abs.clone(), cx)
                        }),
                    )
                    .child(self.project_name.to_string())
                    .into_any_element(),
            );
        }
        let n = comps.len();
        for (i, c) in comps.iter().enumerate() {
            els.push(
                div()
                    .px(px(1.))
                    .text_color(muted.opacity(0.6))
                    .child("›")
                    .into_any_element(),
            );
            if i + 1 == n {
                // 文件段:带文件图标,不可点
                let meta = file_icons::file_icon_meta(c);
                els.push(
                    h_flex()
                        .items_center()
                        .gap(px(5.))
                        .px(px(6.))
                        .text_color(fg)
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(meta.color)
                                .child(meta.glyph),
                        )
                        .child(c.clone())
                        .into_any_element(),
                );
            } else {
                let abs = self
                    .project_root
                    .join(comps[..=i].iter().map(|s| s.as_str()).collect::<PathBuf>());
                els.push(
                    div()
                        .id(("cb", i))
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(radius)
                        .cursor_pointer()
                        .text_color(muted)
                        .hover(|s| s.bg(accent).text_color(fg))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.locate_path_in_tree(abs.clone(), cx)
                            }),
                        )
                        .child(c.clone())
                        .into_any_element(),
                );
            }
        }

        h_flex()
            .h(px(24.))
            .px_2()
            .items_center()
            .gap(px(2.))
            .border_b_1()
            .border_color(cx.theme().border)
            .text_size(px(11.))
            .overflow_hidden()
            .whitespace_nowrap()
            .children(els)
    }

    /// 在文件树中定位某目录/文件(面包屑段点击):切到 Files 视图 + 选中该路径。
    fn locate_path_in_tree(&mut self, abs: PathBuf, cx: &mut Context<Self>) {
        self.set_sidebar_view(SidebarView::Files, cx);
        let name = abs
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| abs.to_string_lossy().to_string());
        let item = TreeItem::new(abs.to_string_lossy().to_string(), name);
        self.tree_state
            .update(cx, |s, cx| s.set_selected_item(Some(&item), cx));
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
            self.restoring_tabs = !target.open_files.is_empty();
            self.load_project(root, cx);
            // 不在这里 persist:此刻 tabs 刚 clear、restore_tabs 还没异步填回,persist 会把目标项目
            // 已存的 open_files 清成空(下次回来文件全丢)。改由 restore_tabs 恢复完成后再 persist。
            self.restore_tabs(target.open_files.clone(), target.active_file.clone(), cx);
        } else {
            self.restoring_tabs = false;
            // 目标路径已不存在:仍需把 active_project_index 落盘
            self.persist_session(cx);
        }
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
                self.restoring_tabs = !target.open_files.is_empty();
                self.load_project(root, cx);
                self.restore_tabs(target.open_files.clone(), target.active_file.clone(), cx);
            } else {
                self.restoring_tabs = false;
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
    /// 从已有 git status 结果(同步,无 IO)建编辑器/树的改动标记。git_panel 刷新后经
    /// StatusUpdated 事件调用(合并查询),以及下面 refresh_git_marks(自跑一次 status 的版本)复用。
    fn apply_git_marks(&mut self, changes: &[nib_core::git::GitChange], cx: &mut Context<Self>) {
        let mut marks = std::collections::HashMap::new();
        for c in changes {
            // 多仓:每条改动的绝对路径 = 所属仓 join 仓内相对路径(c.repo 由 git status 填)。
            let abs = std::path::Path::new(&c.repo)
                .join(&c.path)
                .to_string_lossy()
                .to_string();
            let ch = c.status.chars().next().unwrap_or(' ');
            // 同文件 staged+unstaged 两条:改动类标记优先于已暂存覆盖
            marks.entry(abs).or_insert(ch);
        }
        self.git_marks = Arc::new(marks);
        cx.notify();
    }

    /// 单独跑一次 git status 建改动标记——给「不刷 git_panel 但要更新标记」的场景用
    /// (如保存当前文件)。FileChanged 等会刷 git_panel 的场景不调它,走 StatusUpdated 合并。
    fn refresh_git_marks(&mut self, cx: &mut Context<Self>) {
        let cwd = self.project_root.to_string_lossy().to_string();
        let guard_root = self.project_root.clone();
        cx.spawn(async move |weak, cx| {
            let Ok(changes) = nib_core::git::git_status(cwd).await else {
                return;
            };
            let _ = weak.update(cx, |this: &mut Workbench, cx| {
                if this.project_root == guard_root {
                    this.apply_git_marks(&changes, cx);
                }
            });
        })
        .detach();
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 量这次构建元素树的耗时(守卫在本函数返回后 drop 时写入)
        let _render_timer = RenderTimer {
            t0: Instant::now(),
            out: self.render_us.clone(),
        };
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

        // 本版本 gpui-component 的 Root::render 不渲染通知层(只画 view/tooltip/menu),
        // 需根视图自挂(对齐 story 的 StoryRoot::render)。否则 window.push_notification
        // 只把通知入队、永远不显示——Maven 未配置提醒看不到的真因就在这。
        let notification_layer = Root::render_notification_layer(window, cx);

        // 引用列表浮层锚到鼠标附近弹出(其余浮层仍顶部居中);clamp 进视口避免出屏。
        let is_usages_overlay = matches!(self.overlay, Some(Overlay::Usages(_)));
        let usages_anchor = {
            let vp = window.viewport_size();
            let (vw, vh) = (f32::from(vp.width), f32::from(vp.height));
            let (mx, my) = (f32::from(self.last_mouse.x), f32::from(self.last_mouse.y));
            // 按弹窗实际最大高度(~520)夹紧 y,保证整个弹窗(含底部预览/命中行)在屏内;
            // 鼠标偏低时弹窗整体上移而非被屏幕下沿截断。
            let ax = mx.min(vw - 772.).max(8.);
            let ay = (my + 16.).min(vh - 528.).max(8.);
            point(px(ax), px(ay))
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
            // 记最近鼠标按下位置,引用列表浮层据此弹在鼠标附近(被动记录,不拦事件)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| this.last_mouse = ev.position),
            )
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_toggle_quick_open))
            .on_action(cx.listener(Self::on_toggle_search))
            .on_action(cx.listener(Self::on_open_folder))
            .on_action(cx.listener(Self::on_goto_definition))
            .on_action(cx.listener(Self::on_find_usages))
            .on_action(cx.listener(Self::on_editor_search))
            .on_action(cx.listener(Self::on_nav_back))
            .on_action(cx.listener(Self::on_nav_forward))
            .on_action(cx.listener(Self::on_toggle_md_preview))
            .on_action(cx.listener(Self::on_toggle_terminal))
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
            .on_action(cx.listener(Self::on_new_file))
            .on_action(cx.listener(Self::on_new_folder))
            .on_action(cx.listener(Self::on_rename_item))
            .on_action(cx.listener(Self::on_delete_item))
            .on_action(cx.listener(Self::on_copy_item))
            .on_action(cx.listener(Self::on_cut_item))
            .on_action(cx.listener(Self::on_paste_item))
            .on_action(cx.listener(Self::on_undo_file_op))
            .on_action(cx.listener(Self::on_copy_path))
            .on_action(cx.listener(Self::on_checkout_branch))
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
                                .group(SharedString::from(format!("proj-tab-{ix}")))
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
                                .when(self.projects.len() > 1, |s| {
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
                                            // 非活动 tab:默认隐藏关闭叉,悬浮整个 tab(group)才显出;
                                            // 活动 tab 一直显示。group 名与父级 .group() 一致(每 tab 唯一)。
                                            .when(!active, |s| {
                                                s.opacity(0.).group_hover(
                                                    SharedString::from(format!("proj-tab-{ix}")),
                                                    |s| s.opacity(1.),
                                                )
                                            })
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
                            .child(self.activity_btn("act-files", IconName::Folder, SidebarView::Files, "资源管理器", None, cx))
                            .child(self.activity_btn("act-commit", IconName::Inbox, SidebarView::Commit, "提交改动", None, cx))
                            .child(self.activity_btn(
                                "act-git",
                                IconName::Github,
                                SidebarView::Git,
                                "Git",
                                Some(self.git_panel.read(cx).change_count()),
                                cx,
                            ))
                            .child(self.activity_btn("act-maven", IconName::GalleryVerticalEnd, SidebarView::Maven, "Maven 依赖", None, cx))
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
                                    .tooltip(|window, cx| Tooltip::new("设置").build(window, cx))
                                    .child(
                                        Icon::new(IconName::Settings)
                                            .size(px(19.))
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .group(SharedString::from("sidebar"))
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
                                    // [A2] Files 视图:把右键里已有的 新建文件/文件夹/定位 提到头部,
                                    // 悬浮整条侧栏(group "sidebar")才显出(对齐蓝本 .side-actions)。
                                    // 「全部折叠」按钮未做:TreeState 无 public collapse_all,不造假。
                                    .when(self.sidebar_view == SidebarView::Files, |s| {
                                        s.child(
                                            h_flex()
                                                .gap(px(2.))
                                                .opacity(0.)
                                                .group_hover(
                                                    SharedString::from("sidebar"),
                                                    |s| s.opacity(1.),
                                                )
                                                .child(self.side_action_btn(
                                                    "sh-new-file",
                                                    "＋",
                                                    "新建文件",
                                                    |this, window, cx| {
                                                        this.on_new_file(&NewFile, window, cx)
                                                    },
                                                    cx,
                                                ))
                                                .child(self.side_action_btn(
                                                    "sh-new-folder",
                                                    "▤",
                                                    "新建文件夹",
                                                    |this, window, cx| {
                                                        this.on_new_folder(&NewFolder, window, cx)
                                                    },
                                                    cx,
                                                ))
                                                .child(self.side_action_btn(
                                                    "sh-locate",
                                                    "⌖",
                                                    "定位当前文件",
                                                    |this, _window, cx| {
                                                        this.locate_current_file(cx)
                                                    },
                                                    cx,
                                                )),
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
                                        // [A3] 圆角标签卡片底对齐 + 间隙;接近 30 标签上限时
                                        // 横向溢出滚动(配合单标签 min_w(128))不再挤成丝。
                                        .items_end()
                                        .gap(px(5.))
                                        .px(px(6.))
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
                                        // Java 文件才显示 Arthas 命令项
                                        let is_java = tab.lang == "java";
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
                                                        // 点击编辑器 = 离开搜索语境 → 关掉光标居中,
                                                        // 之后点击/导航不再把视图滚去居中(见构造处注释)。
                                                        if let Some(tab) = this.active() {
                                                            tab.editor.update(cx, |s, cx| {
                                                                s.set_cursor_surrounding_lines(
                                                                    None, window, cx,
                                                                )
                                                            });
                                                        }
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
                                            // 右键菜单:(Java)Arthas 命令 + 复制粘贴 +(markdown)预览
                                            .context_menu(move |menu, _window, _cx| {
                                                // Java 文件才显示 Arthas 命令(生成命令复制到剪贴板)
                                                let menu = if is_java {
                                                    menu.menu(
                                                        "Watch 光标方法",
                                                        Box::new(ArthasWatch),
                                                    )
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
                                                } else {
                                                    menu
                                                };
                                                let menu = menu
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
                                            let raw = tab.editor.read(cx).value();
                                            let base = tab
                                                .path
                                                .parent()
                                                .unwrap_or_else(|| std::path::Path::new("/"))
                                                .to_path_buf();
                                            // 按标题分段:每段一个 TextView 作为滚动容器的直接子元素,
                                            // 点锚点 → md_scroll.scroll_to_top_of_item(段序) 滚到该段
                                            // (段序与 split_into_sections 一致,见 scroll_md_to_anchor)。
                                            // 每段内再把相对文件/锚点链接改写成 nibfile://(经 on_open_urls 路由)。
                                            let section_els: Vec<_> =
                                                nib_core::markdown::split_into_sections(&raw)
                                                    .iter()
                                                    .enumerate()
                                                    .map(|(ix, sec)| {
                                                        let text =
                                                            nib_core::markdown::resolve_relative_links(
                                                                &sec.text, &base,
                                                            );
                                                        gpui_component::text::TextView::markdown(
                                                            ("md-sec", ix),
                                                            text,
                                                        )
                                                        .into_any_element()
                                                    })
                                                    .collect();
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
                                                                .track_scroll(&self.md_scroll)
                                                                .border_l_1()
                                                                .border_color(cx.theme().border)
                                                                .p_4()
                                                                .children(section_els),
                                                        ),
                                                    ),
                                            )
                                        } else {
                                            this.child(editor_el)
                                        }
                                    }
                                    None => {
                                        if self.restoring_tabs {
                                            this.child(self.render_project_loading(cx))
                                        } else {
                                            this.child(self.render_welcome(cx))
                                        }
                                    }
                                }
                            }))
                            .when(self.terminal_visible, |this| {
                                this.child(self.render_bottom_panel(cx))
                            }),
                    ),
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
                                // [A5] 分支段可点 → 切换分支(CheckoutBranch);hover 反馈 + tooltip
                                h_flex()
                                    .id("st-branch")
                                    .gap_1()
                                    .items_center()
                                    .max_w(px(260.))
                                    .min_w_0()
                                    .px_1p5()
                                    .rounded(cx.theme().radius)
                                    .cursor_pointer()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .hover(|s| s.bg(cx.theme().accent))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            this.on_checkout_branch(&CheckoutBranch, window, cx)
                                        }),
                                    )
                                    .tooltip(|window, cx| {
                                        Tooltip::new("切换分支").build(window, cx)
                                    })
                                    .child(
                                        div().flex_none().text_color(cx.theme().info).child("⎇"),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(branch),
                                    ),
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
                                // [A5] 问题段可点 → 打开底部面板并切到「问题」页
                                div()
                                    .id("st-problems")
                                    .px_1p5()
                                    .rounded(cx.theme().radius)
                                    .cursor_pointer()
                                    .text_color(cx.theme().warning)
                                    .whitespace_nowrap()
                                    .hover(|s| s.bg(cx.theme().accent))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.terminal_visible = true;
                                            this.panel_tab = PanelTab::Problems;
                                            cx.notify();
                                        }),
                                    )
                                    .tooltip(|window, cx| {
                                        Tooltip::new("查看问题").build(window, cx)
                                    })
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
                        // 启动/索引阶段显示进度条(jdtls 发的 % 进度),让用户知道在动、要等多久。
                        let (dot, label, starting) = match &self.lsp_phase {
                            LspPhase::Ready => (cx.theme().success, "jdtls 就绪", false),
                            LspPhase::Starting => (cx.theme().warning, "jdtls 启动中", true),
                            LspPhase::Indexing(_) => (cx.theme().warning, "jdtls 启动中", true),
                            LspPhase::Failed => (cx.theme().danger, "jdtls 未启动", false),
                            LspPhase::Off => (cx.theme().muted_foreground, "jdtls 未连接", false),
                        };
                        let percent = match &self.lsp_phase {
                            LspPhase::Indexing(p) => *p,
                            _ => None,
                        };
                        let muted = cx.theme().muted_foreground;
                        let accent = cx.theme().primary;
                        this.child(
                            h_flex()
                                .id("st-jdtls")
                                .items_center()
                                .gap_1p5()
                                .tooltip(|window, cx| {
                                    Tooltip::new("Java 语言服务 (jdtls)").build(window, cx)
                                })
                                .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(dot))
                                .child(div().text_color(muted).child(label))
                                // 进度条:启动/索引阶段显示,有 % 就按比例填充(无 % 显示空槽)
                                .when(starting, |row| {
                                    let frac = percent.unwrap_or(0).min(100) as f32 / 100.;
                                    row.child(
                                        div()
                                            .w(px(64.))
                                            .h(px(5.))
                                            .rounded_full()
                                            .bg(muted.opacity(0.3))
                                            .child(
                                                div()
                                                    .h_full()
                                                    .w(relative(frac))
                                                    .rounded_full()
                                                    .bg(accent),
                                            ),
                                    )
                                })
                                .when_some(percent, |row, p| {
                                    row.child(div().text_color(muted).child(format!("{}%", p)))
                                }),
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
                        // occlude:吃掉落在背板上的鼠标/滚轮事件,别穿透到背后编辑器
                        // (否则设置/diff 页滚动时,后面的代码也跟着滚)。
                        .occlude()
                        .flex()
                        .flex_col()
                        .items_center()
                        .key_context("QuickOpen")
                        .track_focus(&self.overlay_focus)
                        .on_action(cx.listener(Self::on_palette_up))
                        .on_action(cx.listener(Self::on_palette_down))
                        .on_action(cx.listener(Self::on_palette_dismiss))
                        .on_action(cx.listener(Self::on_palette_confirm))
                        // 点浮层外的任意空白处关闭(等价 Esc);浮层框内的 mouse_down
                        // 被下面的 stop_propagation 拦住,不会冒泡到这里 → 框内点击不关。
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                // 消费这次点击(stop_propagation),避免同一下既关浮层又点中
                                // 背后工作区(树里的文件 / 编辑器)
                                cx.stop_propagation();
                                this.close_palette(window, cx);
                            }),
                        )
                        .child(if is_usages_overlay {
                            // 引用列表:绝对定位到鼠标附近(忽略父级 items_center 居中)
                            div()
                                .absolute()
                                .left(usages_anchor.x)
                                .top(usages_anchor.y)
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(content)
                        } else {
                            div()
                                .mt(px(110.))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(content)
                        }),
                )
            })
            // 通知层挂在最上层(浮层之上),右上角弹出
            .children(notification_layer)
    }
}

fn main() {
    APP_START.set(Instant::now()).ok();
    // 注册 gpui-component 内置图标资源(嵌入二进制),否则 IconName::* 的 svg 图标
    // (活动栏 文件/源码/Git/Maven、设置齿轮、定位准星)无 AssetSource → 渲染空白。
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    // markdown 预览里点 nibfile:// 链接 → cx.open_url → OS(LaunchServices)把该 scheme 路由回
    // 本 app。on_open_urls 在 Application(run 之前)上注册,回调签名无 cx,只能把 URL 攒进队列;
    // run 里窗口起好后的轮询任务取出、在 Workbench 标签打开(Zed 同款 scheme 往返思路)。
    let pending_urls: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    app.on_open_urls({
        let p = pending_urls.clone();
        move |urls| {
            if let Ok(mut q) = p.lock() {
                q.extend(urls);
            }
        }
    });
    // 首启自动注册 CC 完成通知 Stop hook(幂等,marker 防重复;只首次写一次 settings.json)。
    // 纯文件 IO,off 主线程,失败只打日志不影响启动。
    std::thread::spawn(|| match nib_core::cc_hook::ensure_registered_once() {
        Ok(nib_core::cc_hook::RegisterOutcome::Registered) => {
            eprintln!("[nib] 已注册 CC 完成通知 hook 到 ~/.claude/settings.json(已备份原文件)");
        }
        Err(e) => eprintln!("[nib] CC hook 注册跳过: {e}"),
        _ => {}
    });

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
        // [圆润] IDEA 新 UI 风:圆角整体提级(单一真源——app 内几乎所有 .rounded(theme.radius)
        // / radius_lg 随之变圆,无需逐处改)。蓝本 --radius 6→8、--radius-lg 10→14。
        {
            let theme = Theme::global_mut(cx);
            theme.radius = px(8.);
            theme.radius_lg = px(14.);
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
            // 对齐旧版 View 菜单(Terminal 项;Git/Maven 在侧栏页签,不重复列)
            Menu::new("View").items([
                MenuItem::action("Markdown 预览  ⇧⌘V", ToggleMdPreview),
                MenuItem::action("终端  ⌃`", ToggleTerminal),
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
            // 捕获 Workbench 弱引用,供下面 URL 处理循环用(open_file 自带 window_handle,无需窗口)。
            let wb_cell: std::rc::Rc<std::cell::RefCell<Option<WeakEntity<Workbench>>>> =
                std::rc::Rc::new(std::cell::RefCell::new(None));
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitleBar::title_bar_options()),
                    ..Default::default()
                },
                {
                    let wb_cell = wb_cell.clone();
                    move |window, cx| {
                        let view = cx.new(|cx| Workbench::new(window, cx));
                        *wb_cell.borrow_mut() = Some(view.downgrade());
                        cx.new(|cx| Root::new(view, window, cx))
                    }
                },
            )
            .expect("Failed to open window");

            let Some(workbench) = wb_cell.borrow().clone() else {
                return;
            };
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(150))
                    .await;
                let urls: Vec<String> = match pending_urls.lock() {
                    Ok(mut q) if !q.is_empty() => std::mem::take(&mut *q),
                    _ => continue,
                };
                for url in urls {
                    // 先判文档内锚点(滚动预览到章节),再判文件链接(新标签打开)
                    if let Some(slug) = nib_core::markdown::anchor_slug_from_nibfile_url(&url) {
                        if workbench
                            .update(cx, |wb, cx| wb.scroll_md_to_anchor(&slug, cx))
                            .is_err()
                        {
                            return; // 窗口/Workbench 已销毁 → 退出循环
                        }
                    } else if let Some(path) = nib_core::markdown::path_from_nibfile_url(&url) {
                        if workbench.update(cx, |wb, cx| wb.open_file(path, cx)).is_err() {
                            return;
                        }
                    }
                }
            }
        })
        .detach();
    });
}
