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
use nib_core::diff::{DiffKind, DiffRow, LineDiff};

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

/// diff 渲染行:hunk 头(@@)或一行 diff。
enum RenderRow<'a> {
    Hunk(SharedString),
    Line(&'a DiffRow),
}

/// 把 core 的「全文扁平行」按变更点分组成 hunk:每个变更点上下保留 CONTEXT 行上下文,
/// 相邻 hunk 合并,远处未变更行折叠掉,并在每个 hunk 前合成标准 unified 头
/// `@@ -旧起,旧行数 +新起,新行数 @@`。git 的函数段标题来自 `git -p`,similar 内核拿不到,
/// 故只给标准行号头、不臆造段名(对齐 nib-git-diff.html 的 hunk 结构,不伪造数据)。
fn build_render_rows(rows: &[DiffRow]) -> Vec<RenderRow<'_>> {
    const CONTEXT: usize = 3;
    if rows.is_empty() {
        return Vec::new();
    }
    let changed: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.kind != DiffKind::Same)
        .map(|(i, _)| i)
        .collect();
    // 全文无变更:原样渲染(不加 hunk 头)
    if changed.is_empty() {
        return rows.iter().map(RenderRow::Line).collect();
    }
    let last_ix = rows.len() - 1;
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for &ci in &changed {
        let start = ci.saturating_sub(CONTEXT);
        let end = (ci + CONTEXT).min(last_ix);
        match windows.last_mut() {
            Some(w) if start <= w.1 + 1 => w.1 = w.1.max(end),
            _ => windows.push((start, end)),
        }
    }
    let mut out = Vec::new();
    for (start, end) in windows {
        let slice = &rows[start..=end];
        let old_start = slice.iter().find_map(|r| r.old_line).unwrap_or(0);
        let new_start = slice.iter().find_map(|r| r.new_line).unwrap_or(0);
        let old_count = slice.iter().filter(|r| r.old_line.is_some()).count();
        let new_count = slice.iter().filter(|r| r.new_line.is_some()).count();
        out.push(RenderRow::Hunk(
            format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@").into(),
        ));
        out.extend(slice.iter().map(RenderRow::Line));
    }
    out
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let added_bg = cx.theme().success.opacity(0.13);
        let removed_bg = cx.theme().danger.opacity(0.13);
        let hunk_bg = cx.theme().info.opacity(0.10);
        let ln = |n: Option<u32>| -> SharedString {
            n.map(|v| v.to_string()).unwrap_or_default().into()
        };
        let rows: Vec<_> = build_render_rows(&self.diff.rows)
            .iter()
            .map(|rr| {
                // hunk 头:整行蓝底,等宽,跨满宽——对齐 nib-git-diff.html 的 .hunk
                let row = match rr {
                    RenderRow::Hunk(label) => {
                        return h_flex()
                            .w_full()
                            .px_2()
                            .py(px(2.))
                            .bg(hunk_bg)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(11.5))
                            .text_color(cx.theme().info)
                            .child(label.clone())
                            .into_any_element();
                    }
                    RenderRow::Line(row) => *row,
                };
                let (bg, sign): (Option<Hsla>, &str) = match row.kind {
                    DiffKind::Same => (None, " "),
                    DiffKind::Added => (Some(added_bg), "+"),
                    DiffKind::Removed => (Some(removed_bg), "-"),
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
                    .into_any_element()
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
