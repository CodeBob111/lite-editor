// Find Usages 浮层(对齐 IDEA Find Usages):上半 = 引用列表(代码片段 + 文件:行),
// 下半 = 选中项所在文件的代码预览(虚拟列表,可滑动整个文件,命中行高亮、初始居中)。
// ↑↓ 选择 / Enter / 点击跳转,Esc 关闭。

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::lsp::LspUsage;

use crate::preview::{line_text, PreviewDoc};

pub enum UsagesEvent {
    /// 跳到某处引用。uri 可能是 file://(项目文件)或 jdt://(库类内引用),
    /// 父级据此决定直接开文件还是抽取库源码再开。
    Open {
        uri: String,
        line: u32,
        character: u32,
    },
}

/// 从引用的 uri 取「文件名」展示用文本。jdt:// URI 的类身份在 '?' 前(后面是
/// jar/项目元数据,含 '/' 与 %3C/%28 编码),必须先切掉 query 再取末段并解码,
/// 否则显示成 `%3Ccom...%28UltronModule.class` 这种乱码。
fn usage_label(uri: &str) -> String {
    let path = uri.split('?').next().unwrap_or(uri);
    let name = path.rsplit('/').next().unwrap_or(path);
    nib_core::lsp::path_from_file_uri(name) // 复用其 percent-decode(无 file:// 前缀时只解码)
}

pub struct UsagesView {
    symbol_file: String,
    usages: Vec<LspUsage>,
    selected: usize,
    /// 选中引用所在文件的预览(异步读盘 + 行偏移切片;jdt:// 引用无本地文件 → None)。
    preview: Option<PreviewDoc>,
    /// 预览读盘防抖序号:连续切选区只让最后一次的异步读应用。
    preview_seq: u64,
    preview_scroll: UniformListScrollHandle,
}

impl EventEmitter<UsagesEvent> for UsagesView {}

impl UsagesView {
    pub fn new(symbol_file: String, usages: Vec<LspUsage>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            symbol_file,
            usages,
            selected: 0,
            preview: None,
            preview_seq: 0,
            preview_scroll: UniformListScrollHandle::default(),
        };
        this.load_preview(cx);
        this
    }

    /// 异步读选中引用所在文件做预览,定位命中行居中。读盘在后台线程(不阻塞 UI);
    /// 选中项还在同一文件 → 只移命中行不重读;带 seq 防抖,连按方向键只应用最后一次。
    fn load_preview(&mut self, cx: &mut Context<Self>) {
        let Some(u) = self.usages.get(self.selected) else {
            self.preview = None;
            return;
        };
        if !u.uri.starts_with("file://") {
            self.preview = None; // jdt:// 库引用无本地文件
            return;
        }
        let path = nib_core::lsp::path_from_file_uri(&u.uri);
        let match_line = u.line as usize;
        // 同文件:只改命中行 + 重新居中,不重读
        if let Some(doc) = &mut self.preview {
            if doc.path == path {
                doc.match_line = match_line.min(doc.line_count().saturating_sub(1));
                self.preview_scroll
                    .scroll_to_item(doc.match_line, ScrollStrategy::Center);
                return;
            }
        }
        self.preview_seq += 1;
        let seq = self.preview_seq;
        cx.spawn(async move |weak, cx| {
            let content = nib_core::fs::read_file(path.clone()).await.ok();
            let _ = weak.update(cx, |this, cx| {
                if this.preview_seq != seq {
                    return; // 期间又切了选区 → 放弃本次
                }
                this.preview = content.map(|c| {
                    let doc = PreviewDoc::new(path, c, match_line);
                    this.preview_scroll
                        .scroll_to_item(doc.match_line, ScrollStrategy::Center);
                    doc
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.usages.is_empty() {
            return;
        }
        let len = self.usages.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(len)) as usize;
        self.load_preview(cx);
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(u) = self.usages.get(self.selected) {
            cx.emit(UsagesEvent::Open {
                uri: u.uri.clone(),
                line: u.line,
                character: u.character,
            });
        }
    }

    fn select_and_open(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        self.confirm(cx);
    }
}

impl Render for UsagesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = cx.theme().mono_font_family.clone();
        let rows: Vec<_> = self
            .usages
            .iter()
            .enumerate()
            .map(|(ix, u)| {
                let selected = ix == self.selected;
                let label = usage_label(&u.uri);
                // 代码片段(左,主)+ 文件名:行(右,次)—— 对齐 IDEA Find Usages 行样式
                let code = if u.text.trim().is_empty() {
                    label.clone()
                } else {
                    u.text.trim_start().to_string()
                };
                h_flex()
                    .id(ix)
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
                        cx.listener(move |this, _, _, cx| this.select_and_open(ix, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family(mono.clone())
                            .text_size(px(12.5))
                            .child(SharedString::from(code)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(format!("{} {}", label, u.line + 1)),
                    )
            })
            .collect();

        // 下半:选中引用所在文件的代码预览(虚拟列表,可滑全文件,命中行高亮)。
        // 捕获 content/line_starts 两个 Arc(克隆廉价),只对可见行切片取文本,不碰全文。
        let (content, line_starts, match_ix, count) = match &self.preview {
            Some(d) => (
                d.content.clone(),
                d.line_starts.clone(),
                d.match_line,
                d.line_count(),
            ),
            None => (std::sync::Arc::new(String::new()), std::sync::Arc::new(vec![]), 0, 0),
        };
        let has_preview = count > 0;
        let preview = uniform_list("usages-preview", count, move |range, _, cx| {
            let mono = cx.theme().mono_font_family.clone();
            let muted = cx.theme().muted_foreground;
            let hit_bg = cx.theme().info.opacity(0.14);
            range
                .map(|i| {
                    h_flex()
                        .px_2()
                        .gap_3()
                        .items_start()
                        .when(i == match_ix, |s| s.bg(hit_bg))
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .child(
                            div()
                                .w(px(48.))
                                .flex_none()
                                .text_right()
                                .text_color(muted)
                                .child(format!("{}", i + 1)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(SharedString::from(line_text(&content, &line_starts, i))),
                        )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.preview_scroll);

        v_flex()
            .w(px(760.))
            .max_h(px(520.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .overflow_hidden()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .flex_none()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_size(px(12.))
                    .child(format!(
                        "{} 处引用 — {}",
                        self.usages.len(),
                        self.symbol_file
                    )),
            )
            .child(
                v_flex()
                    .id("usages-rows")
                    .flex_none()
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .p_1()
                    .children(rows),
            )
            .when(has_preview, |c| {
                c.child(
                    div()
                        .h(px(260.))
                        .py_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background)
                        .child(preview.size_full()),
                )
            })
    }
}
