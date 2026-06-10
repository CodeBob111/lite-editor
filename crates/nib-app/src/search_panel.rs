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
            _subscription: subscription,
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
                    .px_3()
                    .py_1()
                    .gap_2()
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
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(format!("{}:{}", rel, hit.line + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(hit.text.clone()),
                    )
            })
            .collect();

        v_flex()
            .w(px(640.))
            .max_h(px(460.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::Search).small())
                    .child(div().flex_1().child(Input::new(&self.input))),
            )
            .child(
                h_flex()
                    .px_3()
                    .pb_1()
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
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .px_1()
                    .pb_1()
                    .children(rows),
            )
    }
}
