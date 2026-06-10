// Diff 视图(M2c,自绘——RFC 三选一决策:深定制视图不押注组件库):
// 双行号 unified 布局,新增/删除行按旧版语义着色;数据由 nib-core diff 内核
// 在 core runtime 算好,这里只渲染行对。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme, Sizable as _,
};
use nib_core::diff::{DiffKind, LineDiff};

pub enum DiffViewEvent {
    OpenFile(PathBuf),
}

pub struct DiffView {
    rel_path: String,
    abs_path: PathBuf,
    diff: LineDiff,
}

impl EventEmitter<DiffViewEvent> for DiffView {}

impl DiffView {
    pub fn new(rel_path: String, abs_path: PathBuf, diff: LineDiff) -> Self {
        Self {
            rel_path,
            abs_path,
            diff,
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let added_bg = cx.theme().success.opacity(0.13);
        let removed_bg = cx.theme().danger.opacity(0.13);
        let rows: Vec<_> = self
            .diff
            .rows
            .iter()
            .map(|row| {
                let (bg, sign): (Option<Hsla>, &str) = match row.kind {
                    DiffKind::Same => (None, " "),
                    DiffKind::Added => (Some(added_bg), "+"),
                    DiffKind::Removed => (Some(removed_bg), "-"),
                };
                let ln = |n: Option<u32>| -> SharedString {
                    n.map(|v| v.to_string()).unwrap_or_default().into()
                };
                h_flex()
                    .px_2()
                    .gap_2()
                    .items_start()
                    .when_some(bg, |s, bg| s.bg(bg))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(12.))
                    .child(
                        div()
                            .w(px(40.))
                            .text_color(cx.theme().muted_foreground)
                            .text_right()
                            .child(ln(row.old_line)),
                    )
                    .child(
                        div()
                            .w(px(40.))
                            .text_color(cx.theme().muted_foreground)
                            .text_right()
                            .child(ln(row.new_line)),
                    )
                    .child(
                        div()
                            .w(px(12.))
                            .text_color(match row.kind {
                                DiffKind::Added => cx.theme().success,
                                DiffKind::Removed => cx.theme().danger,
                                DiffKind::Same => cx.theme().muted_foreground,
                            })
                            .child(SharedString::from(sign.to_string())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(SharedString::from(row.text.clone())),
                    )
            })
            .collect();

        let abs = self.abs_path.clone();
        v_flex()
            .w(px(860.))
            .max_h(px(560.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(px(13.))
                            .child(SharedString::from(self.rel_path.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(cx.theme().success)
                            .child(format!("+{}", self.diff.stat.added)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(cx.theme().danger)
                            .child(format!("-{}", self.diff.stat.removed)),
                    )
                    .child(
                        Button::new("open-file")
                            .ghost()
                            .xsmall()
                            .label("打开文件")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(DiffViewEvent::OpenFile(abs.clone()));
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("diff-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py_1()
                    .children(rows),
            )
    }
}
