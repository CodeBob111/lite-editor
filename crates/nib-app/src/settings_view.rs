// 设置浮层(M1,对齐旧版 settings-ui 的 4 个编辑器偏好):字号 / Tab 宽度 /
// 自动换行 / 代码折叠。保存即持久化(扁平键 schema 不变)并热应用——
// 字号、换行、折叠对已开标签即时生效;Tab 宽度新开标签生效。

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, Sizable as _,
};

use crate::session::EditorSettings;

pub enum SettingsEvent {
    Apply(EditorSettings),
}

pub struct SettingsView {
    font_input: Entity<InputState>,
    tab_input: Entity<InputState>,
    word_wrap: bool,
    folding: bool,
    error: SharedString,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(settings: EditorSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let font_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(format!("{}", settings.font_size))
        });
        let tab_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(format!("{}", settings.tab_size))
        });
        Self {
            font_input,
            tab_input,
            word_wrap: settings.word_wrap,
            folding: settings.folding,
            error: "".into(),
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let font_size = match self.font_input.read(cx).value().trim().parse::<f32>() {
            Ok(v) if (8.0..=40.0).contains(&v) => v,
            _ => {
                self.error = "字号需是 8-40 的数字".into();
                cx.notify();
                return;
            }
        };
        let tab_size = match self.tab_input.read(cx).value().trim().parse::<u32>() {
            Ok(v) if (1..=16).contains(&v) => v,
            _ => {
                self.error = "Tab 宽度需是 1-16 的整数".into();
                cx.notify();
                return;
            }
        };
        cx.emit(SettingsEvent::Apply(EditorSettings {
            font_size,
            tab_size,
            word_wrap: self.word_wrap,
            folding: self.folding,
        }));
    }

    fn toggle_row(
        &self,
        id: &'static str,
        label: &'static str,
        value: bool,
        on_toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(96.)).text_size(px(12.)).child(label))
            .child(
                Button::new(id)
                    .xsmall()
                    .label(if value { "开" } else { "关" })
                    .when(value, |b| b.primary())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        on_toggle(this, cx);
                        cx.notify();
                    })),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w(px(380.))
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
                    .child("设置"),
            )
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(96.)).text_size(px(12.)).child("字号"))
                            .child(div().w(px(80.)).child(Input::new(&self.font_input))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(96.)).text_size(px(12.)).child("Tab 宽度"))
                            .child(div().w(px(80.)).child(Input::new(&self.tab_input))),
                    )
                    .child(self.toggle_row(
                        "set-wrap",
                        "自动换行",
                        self.word_wrap,
                        |this, _| this.word_wrap = !this.word_wrap,
                        cx,
                    ))
                    .child(self.toggle_row(
                        "set-folding",
                        "代码折叠",
                        self.folding,
                        |this, _| this.folding = !this.folding,
                        cx,
                    ))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child("字号/换行/折叠即时生效;Tab 宽度对新打开的标签生效"),
                    )
                    .when(!self.error.is_empty(), |s| {
                        s.child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().danger)
                                .child(self.error.clone()),
                        )
                    }),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1())
                    .child(
                        Button::new("settings-save")
                            .primary()
                            .xsmall()
                            .label("保存")
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}
