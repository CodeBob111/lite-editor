// Find Usages 浮层(对齐 IDEA Find Usages):上半 = 引用列表(代码片段 + 文件:行),
// 下半 = 选中项的附近代码块预览(可滑动)。↑↓ 选择 / Enter / 点击跳转,Esc 关闭。

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::lsp::LspUsage;

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
    /// 选中项的附近代码块:(行号 0-based, 文本, 是否命中行),随选中变化重载。
    preview: Vec<(u32, String, bool)>,
}

impl EventEmitter<UsagesEvent> for UsagesView {}

impl UsagesView {
    pub fn new(symbol_file: String, usages: Vec<LspUsage>) -> Self {
        let mut this = Self {
            symbol_file,
            usages,
            selected: 0,
            preview: Vec::new(),
        };
        this.load_preview();
        this
    }

    /// 读选中引用所在文件命中行上下文(各 ~5 行)。jdt:// 引用无本地文件 → 不预览。
    fn load_preview(&mut self) {
        self.preview.clear();
        let Some(u) = self.usages.get(self.selected) else {
            return;
        };
        if !u.uri.starts_with("file://") {
            return;
        }
        let path = nib_core::lsp::path_from_file_uri(&u.uri);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let lines: Vec<&str> = content.lines().collect();
        let center = u.line as usize;
        let start = center.saturating_sub(4);
        let end = (center + 6).min(lines.len());
        for (i, line) in lines.iter().enumerate().take(end).skip(start) {
            self.preview.push((i as u32, line.to_string(), i == center));
        }
    }

    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.usages.is_empty() {
            return;
        }
        let len = self.usages.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(len)) as usize;
        self.load_preview();
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
                // 代码片段(左,主)+ 文件:行(右,次)—— 对齐 IDEA Find Usages 行样式
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

        // 下半:选中引用的附近代码块(可滑动),命中行高亮
        let info = cx.theme().info;
        let hit_bg = info.opacity(0.14);
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
                            .w(px(44.))
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
            .max_h(px(560.))
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
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .p_1()
                    .children(rows),
            )
            .when(!preview_rows.is_empty(), |c| {
                c.child(
                    v_flex()
                        .id("usages-preview")
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
