// 全局搜索面板(Cmd+Shift+F,对齐旧版 search.ts):查询经 nib-core 的并行
// 遍历搜索(ripgrep 同款 walker,core runtime 上跑),结果列表选中跳转到行。
// 每次输入都带序号守卫——慢查询的旧结果绝不覆盖新查询(本项目踩过三次的坑)。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme, Icon, IconName, Sizable as _,
};
use nib_core::search::SearchResult;

const MAX_SHOWN: usize = 100;

pub enum SearchEvent {
    Open {
        path: PathBuf,
        line: u32,
        column: u32,
    },
}

pub struct SearchPanel {
    input: Entity<InputState>,
    project_root: PathBuf,
    results: Arc<Vec<SearchResult>>,
    selected: usize,
    searching: bool,
    /// 查询序号守卫:只接受最新一次查询的结果
    query_seq: u64,
    /// 选中结果的附近代码块:(行号 0-based, 文本, 是否命中行),随选中变化重载。
    preview: Vec<(u32, String, bool)>,
    _subscription: Subscription,
}

impl EventEmitter<SearchEvent> for SearchPanel {}

impl SearchPanel {
    pub fn new(project_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("在项目中搜索(回车跳转)…"));
        let subscription = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            match event {
                InputEvent::Change => this.start_search(cx),
                InputEvent::PressEnter { .. } => this.confirm(cx),
                _ => {}
            }
        });

        Self {
            input,
            project_root,
            results: Arc::new(Vec::new()),
            selected: 0,
            searching: false,
            query_seq: 0,
            preview: Vec::new(),
            _subscription: subscription,
        }
    }

    /// 读选中结果所在文件命中行上下文(各 ~5 行),命中行高亮。
    fn load_preview(&mut self) {
        self.preview.clear();
        let Some(hit) = self.results.get(self.selected) else {
            return;
        };
        let Ok(content) = std::fs::read_to_string(&hit.path) else {
            return;
        };
        let lines: Vec<&str> = content.lines().collect();
        let center = hit.line as usize;
        let start = center.saturating_sub(4);
        let end = (center + 6).min(lines.len());
        for (i, line) in lines.iter().enumerate().take(end).skip(start) {
            self.preview.push((i as u32, line.to_string(), i == center));
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn start_search(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.query_seq += 1;
        let seq = self.query_seq;

        if query.trim().is_empty() {
            self.results = Arc::new(Vec::new());
            self.selected = 0;
            self.searching = false;
            cx.notify();
            return;
        }

        self.searching = true;
        cx.notify();

        let root = self.project_root.to_string_lossy().to_string();
        cx.spawn(async move |weak, cx| {
            let result =
                nib_core::search::search_in_files(root, query, Some(false), Some(500)).await;
            let _ = weak.update(cx, |this, cx| {
                // 序号守卫:期间又敲了字就丢弃本次结果
                if this.query_seq != seq {
                    return;
                }
                this.results = Arc::new(result.unwrap_or_default());
                this.selected = 0;
                this.load_preview();
                this.searching = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let len = self.results.len().min(MAX_SHOWN) as i32;
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).rem_euclid(len);
        self.selected = next as usize;
        self.load_preview();
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(hit) = self.results.get(self.selected) {
            cx.emit(SearchEvent::Open {
                path: PathBuf::from(hit.path.clone()),
                line: hit.line,
                column: hit.column,
            });
        }
    }

    fn select_and_open(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        self.confirm(cx);
    }

    fn rel(&self, abs: &str) -> String {
        let root = self.project_root.to_string_lossy();
        abs.strip_prefix(root.as_ref())
            .map(|s| s.trim_start_matches('/').to_string())
            .unwrap_or_else(|| abs.to_string())
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.results.len();
        let mono = cx.theme().mono_font_family.clone();
        // 列表行:代码片段(左,主)+ 相对路径:行(右,次)—— 对齐 IDEA Find in Files
        let rows: Vec<_> = self
            .results
            .iter()
            .take(MAX_SHOWN)
            .enumerate()
            .map(|(row, hit)| {
                let rel = self.rel(&hit.path);
                let selected = row == self.selected;
                h_flex()
                    .id(row)
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_3()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .when(selected, |s| s.bg(cx.theme().list_active))
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.select_and_open(row, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family(mono.clone())
                            .text_size(px(12.5))
                            .child(hit.text.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(format!("{} {}", rel, hit.line + 1)),
                    )
            })
            .collect();

        // 下半:选中结果的附近代码块(可滑动),命中行高亮
        let hit_bg = cx.theme().info.opacity(0.14);
        let preview_rows: Vec<_> = self
            .preview
            .iter()
            .map(|(ln, text, hit)| {
                h_flex()
                    .px_2()
                    .gap_3()
                    .items_start()
                    .when(*hit, |s| s.bg(hit_bg))
                    .font_family(mono.clone())
                    .text_size(px(12.))
                    .child(
                        div()
                            .w(px(48.))
                            .flex_none()
                            .text_right()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{}", ln + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(SharedString::from(text.clone())),
                    )
            })
            .collect();

        v_flex()
            .w(px(760.))
            .h(px(580.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .overflow_hidden()
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .flex_none()
                    .items_center()
                    .child(Icon::new(IconName::Search).small())
                    .child(div().flex_1().child(Input::new(&self.input))),
            )
            .child(
                h_flex()
                    .px_3()
                    .pb_1()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.searching {
                        "搜索中…".to_string()
                    } else if total > MAX_SHOWN {
                        format!("{} 个结果(显示前 {})", total, MAX_SHOWN)
                    } else {
                        format!("{} 个结果", total)
                    }),
            )
            .child(
                v_flex()
                    .id("search-rows")
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .px_1()
                    .pb_1()
                    .children(rows),
            )
            .when(!preview_rows.is_empty(), |c| {
                c.child(
                    v_flex()
                        .id("search-preview")
                        .flex_1()
                        .min_h(px(120.))
                        .overflow_y_scroll()
                        .py_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .children(preview_rows),
                )
            })
    }
}
