// 最近项目浮层(M1,对齐旧版 recent-projects overlay):列表 + ↑↓/Enter/点击
// 打开,Esc 关闭。数据来自 session::load_recents(上限 12,最近优先)。

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};

pub enum RecentsEvent {
    Open(String),
}

pub struct RecentsView {
    recents: Vec<String>,
    selected: usize,
}

impl EventEmitter<RecentsEvent> for RecentsView {}

impl RecentsView {
    pub fn new(recents: Vec<String>) -> Self {
        Self {
            recents,
            selected: 0,
        }
    }

    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.recents.is_empty() {
            return;
        }
        let len = self.recents.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(len)) as usize;
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.recents.get(self.selected) {
            cx.emit(RecentsEvent::Open(path.clone()));
        }
    }

    fn select_and_open(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        self.confirm(cx);
    }
}

impl Render for RecentsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .recents
            .iter()
            .enumerate()
            .map(|(ix, path)| {
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
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
                    .child(div().text_size(px(13.)).child(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(path.clone()),
                    )
            })
            .collect();

        v_flex()
            .w(px(560.))
            .max_h(px(400.))
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
                    .child("最近项目"),
            )
            .child(
                v_flex()
                    .id("recents-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .when(self.recents.is_empty(), |s| {
                        s.child(
                            div()
                                .p_3()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .child("还没有最近项目记录"),
                        )
                    })
                    .children(rows),
            )
    }
}
