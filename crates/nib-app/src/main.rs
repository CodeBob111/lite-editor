// Nib 原生工作台(M1 第一刀):暗色主题 + 标题栏 + 标签页 + 文件树图标。
// 性能纪律(RFC v2 §5):主线程零阻塞 IO——目录遍历/读写文件全部经 nib-core
// 自持 runtime,结果回主线程更新实体;异步回灌一律带陈旧守卫。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputState, TabSize},
    list::ListItem,
    tab::{Tab, TabBar},
    tree::{tree, TreeItem, TreeState},
    v_flex, ActiveTheme, Icon, IconName, Root, Sizable as _, Theme, ThemeMode, TitleBar,
};

actions!(nib, [SaveFile, CloseTab]);

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
    editor: Entity<InputState>,
}

struct Workbench {
    window_handle: AnyWindowHandle,
    project_name: SharedString,
    tree_state: Entity<TreeState>,
    tabs: Vec<OpenTab>,
    active_tab: Option<usize>,
    status: SharedString,
    /// 主线程停顿哨兵计数(>32ms 漂移即记,可举证不凭感觉)
    stall_count: usize,
}

impl Workbench {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = std::env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let project_name: SharedString = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string())
            .into();

        let tree_state = cx.new(|cx| TreeState::new(cx));

        // 目录遍历在 core runtime 上跑,完成后回主线程灌树
        let tree_for_load = tree_state.clone();
        let root_str = root.to_string_lossy().to_string();
        cx.spawn(async move |_, cx| {
            let result = nib_core::fs::read_dir_tree(root_str, Some(12)).await;
            if let Ok(node) = result {
                let items = match &node.children {
                    Some(children) => children.iter().map(file_node_to_tree_item).collect(),
                    None => vec![file_node_to_tree_item(&node)],
                };
                tree_for_load.update(cx, |state, cx| state.set_items(items, cx));
            }
        })
        .detach();

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

        Self {
            window_handle: window.window_handle(),
            project_name,
            tree_state,
            tabs: Vec::new(),
            active_tab: None,
            status: root.display().to_string().into(),
            stall_count: 0,
        }
    }

    /// 帧时/主线程停顿哨兵(RFC v2 §5.6):每 100ms 一个心跳回主线程,
    /// 漂移 >32ms 视为一次可感知停顿,记证据到 stderr + 状态栏计数。
    fn start_stall_sentinel(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            const BEAT: std::time::Duration = std::time::Duration::from_millis(100);
            const BUDGET: std::time::Duration = std::time::Duration::from_millis(32);
            let mut last = std::time::Instant::now();
            loop {
                cx.background_executor().timer(BEAT).await;
                let alive = this.update(cx, |this, cx| {
                    let now = std::time::Instant::now();
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

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // 已开 → 直接切标签
        if let Some(ix) = self.tabs.iter().position(|t| t.path == path) {
            if self.active_tab != Some(ix) {
                self.active_tab = Some(ix);
                cx.notify();
            }
            return;
        }

        self.status = format!("打开 {} …", path.display()).into();
        cx.notify();

        let window_handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            // 读盘在 core runtime;回主线程后再次查重(防快速连点重复开)
            let content = nib_core::fs::read_file(path.to_string_lossy().to_string()).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.tabs.iter().any(|t| t.path == path) {
                        return;
                    }
                    match content {
                        Ok(text) => {
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
                            let title: SharedString = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.display().to_string())
                                .into();
                            this.tabs.push(OpenTab {
                                path: path.clone(),
                                title,
                                lang,
                                editor,
                            });
                            this.active_tab = Some(this.tabs.len() - 1);
                            this.status = path.display().to_string().into();
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
                    Ok(()) => format!("已保存 {}", path.display()).into(),
                    Err(err) => format!("保存失败: {}", err).into(),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn on_close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_tab else { return };
        self.tabs.remove(ix);
        self.active_tab = if self.tabs.is_empty() {
            None
        } else {
            Some(ix.min(self.tabs.len() - 1))
        };
        cx.notify();
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
            .bg(cx.theme().background)
            .key_context("Workbench")
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_close_tab))
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
                                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                            this.active_tab = Some(*ix);
                                            cx.notify();
                                        }))
                                        .children(
                                            self.tabs
                                                .iter()
                                                .map(|t| Tab::new().label(t.title.clone())),
                                        ),
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
                                            .child("从左侧文件树选择文件;Cmd+S 保存,Cmd+W 关标签"),
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
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().min_w_0().overflow_hidden().child(self.status.clone()))
                    .when(!active_lang.is_empty(), |this| this.child(active_lang))
                    .when(self.stall_count > 0, |this| {
                        this.child(
                            div()
                                .text_color(cx.theme().danger)
                                .child(format!("卡顿 ×{}", self.stall_count)),
                        )
                    }),
            )
    }
}

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.bind_keys([
            KeyBinding::new("cmd-s", SaveFile, Some("Workbench")),
            KeyBinding::new("cmd-w", CloseTab, Some("Workbench")),
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
