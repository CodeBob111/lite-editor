// Find Usages 浮层(M3,对齐旧版 usages-popup):lsp_find_references 结果列表,
// ↑↓ 选择 / Enter / 点击跳转,Esc 关闭。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::lsp::LspUsage;

pub enum UsagesEvent {
    Open {
        path: PathBuf,
        line: u32,
        character: u32,
    },
}

pub struct UsagesView {
    symbol_file: String,
    usages: Vec<LspUsage>,
    selected: usize,
}

impl EventEmitter<UsagesEvent> for UsagesView {}

impl UsagesView {
    pub fn new(symbol_file: String, usages: Vec<LspUsage>) -> Self {
        Self {
            symbol_file,
            usages,
            selected: 0,
        }
    }

    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.usages.is_empty() {
            return;
        }
        let len = self.usages.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(len)) as usize;
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(u) = self.usages.get(self.selected) {
            cx.emit(UsagesEvent::Open {
                path: PathBuf::from(u.uri.strip_prefix("file://").unwrap_or(&u.uri)),
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
        let rows: Vec<_> = self
            .usages
            .iter()
            .enumerate()
            .map(|(ix, u)| {
                let file = u
                    .uri
                    .rsplit('/')
                    .next()
                    .unwrap_or(&u.uri)
                    .to_string();
                let selected = ix == self.selected;
                h_flex()
                    .id(ix)
                    .px_3()
                    .py_1()
                    .gap_2()
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
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(format!("{}:{}", file, u.line + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .child(SharedString::from(u.text.clone())),
                    )
            })
            .collect();

        v_flex()
            .w(px(680.))
            .max_h(px(440.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
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
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .children(rows),
            )
    }
}
