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
    /// 回退一个 hunk:把工作区文件第 new_start..(new_start+new_count) 行(1-based,
    /// 即该 hunk 在新版里的区间)替换为 old_content(该 hunk 旧版内容)。父级写盘+刷新。
    RevertHunk {
        path: PathBuf,
        new_start: usize,
        new_count: usize,
        old_content: Vec<String>,
    },
}

pub struct DiffView {
    rel_path: String,
    abs_path: PathBuf,
    diff: LineDiff,
    /// 每个 hunk 在 diff.rows 里的 [start, end] 闭区间(与渲染分组一致),供按 hunk 回退。
    hunks: Vec<(usize, usize)>,
}

impl EventEmitter<DiffViewEvent> for DiffView {}

impl DiffView {
    pub fn new(rel_path: String, abs_path: PathBuf, diff: LineDiff) -> Self {
        let hunks = hunk_ranges(&diff.rows);
        Self {
            rel_path,
            abs_path,
            diff,
            hunks,
        }
    }

    /// 回退第 hi 个 hunk:算出该 hunk 新版区间(new_start/new_count)与旧版内容,emit 给父级写盘。
    fn revert_hunk(&mut self, hi: usize, cx: &mut Context<Self>) {
        let Some(&(start, end)) = self.hunks.get(hi) else {
            return;
        };
        let slice = &self.diff.rows[start..=end];
        let new_start = slice.iter().find_map(|r| r.new_line).unwrap_or(0) as usize;
        if new_start == 0 {
            return;
        }
        let new_count = slice.iter().filter(|r| r.new_line.is_some()).count();
        let old_content: Vec<String> = slice
            .iter()
            .filter(|r| r.old_line.is_some())
            .map(|r| r.text.clone())
            .collect();
        cx.emit(DiffViewEvent::RevertHunk {
            path: self.abs_path.clone(),
            new_start,
            new_count,
            old_content,
        });
    }
}

/// diff 渲染行:hunk 头(@@,带 hunk 索引供回退)或一行 diff。
enum RenderRow<'a> {
    Hunk(usize, SharedString),
    Line(&'a DiffRow),
}

/// 计算每个 hunk 在 rows 里的 [start, end] 闭区间(与 build_render_rows 的窗口分组一致)。
fn hunk_ranges(rows: &[DiffRow]) -> Vec<(usize, usize)> {
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
    if changed.is_empty() {
        return Vec::new();
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
    windows
}

/// 把 core 的「全文扁平行」按变更点分组成 hunk:每个变更点上下保留 CONTEXT 行上下文,
/// 相邻 hunk 合并,远处未变更行折叠掉,并在每个 hunk 前合成标准 unified 头
/// `@@ -旧起,旧行数 +新起,新行数 @@`。git 的函数段标题来自 `git -p`,similar 内核拿不到,
/// 故只给标准行号头、不臆造段名(对齐 nib-git-diff.html 的 hunk 结构,不伪造数据)。
fn build_render_rows<'a>(rows: &'a [DiffRow], windows: &[(usize, usize)]) -> Vec<RenderRow<'a>> {
    if rows.is_empty() {
        return Vec::new();
    }
    // 全文无变更:原样渲染(不加 hunk 头)
    if windows.is_empty() {
        return rows.iter().map(RenderRow::Line).collect();
    }
    let mut out = Vec::new();
    for (hi, &(start, end)) in windows.iter().enumerate() {
        let slice = &rows[start..=end];
        let old_start = slice.iter().find_map(|r| r.old_line).unwrap_or(0);
        let new_start = slice.iter().find_map(|r| r.new_line).unwrap_or(0);
        let old_count = slice.iter().filter(|r| r.old_line.is_some()).count();
        let new_count = slice.iter().filter(|r| r.new_line.is_some()).count();
        out.push(RenderRow::Hunk(
            hi,
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
        let entity = cx.entity();
        let info = cx.theme().info;
        let mono = cx.theme().mono_font_family.clone();
        let rows: Vec<_> = build_render_rows(&self.diff.rows, &self.hunks)
            .iter()
            .map(|rr| {
                // hunk 头:整行蓝底,等宽,跨满宽 + 右侧「撤销此处」按钮(回退该 hunk)
                let row = match rr {
                    RenderRow::Hunk(hi, label) => {
                        let hi = *hi;
                        let e = entity.clone();
                        return h_flex()
                            .w_full()
                            .px_2()
                            .py(px(2.))
                            .gap_2()
                            .items_center()
                            .bg(hunk_bg)
                            .font_family(mono.clone())
                            .text_size(px(11.5))
                            .text_color(info)
                            .child(div().flex_1().min_w_0().child(label.clone()))
                            .child(
                                Button::new(("revert-hunk", hi))
                                    .ghost()
                                    .xsmall()
                                    .label("↶ 撤销此处")
                                    .on_click(move |_, _, app| {
                                        e.update(app, |this, cx| this.revert_hunk(hi, cx));
                                    }),
                            )
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
