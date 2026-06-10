// Nib 原生工作台(M0b):文件树 + 代码编辑器 + 保存。
// 性能纪律(RFC v2 §5):主线程零阻塞 IO——目录遍历/读写文件全部经 nib-core
// 自持 runtime,结果回主线程更新实体。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputState, TabSize},
    list::ListItem,
    tree::{tree, TreeItem, TreeState},
    v_flex, ActiveTheme, Root,
};

actions!(nib, [SaveFile]);

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

struct Workbench {
    window_handle: AnyWindowHandle,
    tree_state: Entity<TreeState>,
    editor_state: Entity<InputState>,
    current_file: Option<PathBuf>,
    status: SharedString,
    /// 主线程停顿哨兵计数(>32ms 漂移即记,对齐 webview 版 rAF 哨兵哲学:可举证,不凭感觉)
    stall_count: usize,
}

impl Workbench {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = std::env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let tree_state = cx.new(|cx| TreeState::new(cx));
        let editor_state = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("markdown")
                .multi_line(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    ..Default::default()
                })
                .default_value("从左侧文件树选择文件。\n\n中文 IME 验收:请直接在这里输入中文。\n")
        });

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
                if this.current_file.as_ref() != Some(&path) {
                    this.open_file(path, cx);
                }
            }
        })
        .detach();

        Self::start_stall_sentinel(cx);

        Self {
            window_handle: window.window_handle(),
            tree_state,
            editor_state,
            current_file: None,
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
        self.current_file = Some(path.clone());
        self.status = format!("打开 {} …", path.display()).into();
        cx.notify();

        let editor = self.editor_state.clone();
        let window_handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            // 读盘在 core runtime;陈旧守卫:回来时仍是当前文件才回灌
            let content = nib_core::fs::read_file(path.to_string_lossy().to_string()).await;
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.current_file.as_ref() != Some(&path) {
                        return;
                    }
                    match content {
                        Ok(text) => {
                            let lang = language_for(&path.to_string_lossy());
                            editor.update(cx, |state, cx| {
                                state.set_highlighter(lang, cx);
                                state.set_value(text, window, cx);
                            });
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

    fn on_save(&mut self, _: &SaveFile, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.current_file.clone() else {
            return;
        };
        let text = self.editor_state.read(cx).value().to_string();
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
}

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .key_context("Workbench")
            .on_action(cx.listener(Self::on_save))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(280.))
                            .h_full()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(tree(&self.tree_state, |ix, entry, _, _, _| {
                                let item = entry.item();
                                ListItem::new(ix)
                                    .pl(px(8.) + px(12.) * entry.depth() as f32)
                                    .child(item.label.clone())
                            })),
                    )
                    .child(
                        div().flex_1().h_full().min_w_0().child(
                            Input::new(&self.editor_state)
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(cx.theme().mono_font_size)
                                .size_full(),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .h(px(26.))
                    .px_2()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(12.))
                    .child(self.status.clone())
                    .when(self.stall_count > 0, |this| {
                        this.child(
                            div()
                                .ml_auto()
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
        cx.bind_keys([KeyBinding::new("cmd-s", SaveFile, Some("Workbench"))]);

        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("Nib (native)".into()),
                        ..Default::default()
                    }),
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
