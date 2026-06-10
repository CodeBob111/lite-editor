// 3-way merge 浮层(M4,对齐旧版 merge-conflict 主链):解析冲突标记 →
// 逐块双栏(当前分支/并入分支)选边 → 全部选完才能应用(重组写回 + git add,
// 在 nib-core 完成)。整文件快捷方式走 git checkout --ours/--theirs。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use nib_core::diff::MergeSide;

/// 单侧最多渲染的行数(冲突块通常很小;超长块截断显示但选边/写回仍是全量)
const MAX_PANE_LINES: usize = 120;

pub enum MergeViewEvent {
    Resolved,
}

struct ChunkRow {
    ours: Vec<SharedString>,
    theirs: Vec<SharedString>,
}

pub struct MergeView {
    cwd: String,
    rel_path: String,
    chunks: Vec<ChunkRow>,
    choices: Vec<Option<MergeSide>>,
    loaded: bool,
    busy: bool,
    status: SharedString,
}

impl EventEmitter<MergeViewEvent> for MergeView {}

impl MergeView {
    pub fn new(cwd: String, rel_path: String, cx: &mut Context<Self>) -> Self {
        let abs = PathBuf::from(&cwd)
            .join(&rel_path)
            .to_string_lossy()
            .to_string();
        cx.spawn(async move |weak, cx| {
            let parsed = nib_core::git::parse_conflict_file(abs).await;
            let _ = weak.update(cx, |this: &mut MergeView, cx| {
                this.loaded = true;
                match parsed {
                    Ok(parsed) => {
                        this.choices = vec![None; parsed.chunks.len()];
                        this.chunks = parsed
                            .chunks
                            .into_iter()
                            .map(|c| ChunkRow {
                                ours: split_lines(&c.ours_text),
                                theirs: split_lines(&c.theirs_text),
                            })
                            .collect();
                        if this.chunks.is_empty() {
                            this.status = "文件里没有冲突标记".into();
                        }
                    }
                    Err(err) => this.status = format!("解析失败: {}", err).into(),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            cwd,
            rel_path,
            chunks: Vec::new(),
            choices: Vec::new(),
            loaded: false,
            busy: false,
            status: "".into(),
        }
    }

    fn choose(&mut self, ix: usize, side: MergeSide, cx: &mut Context<Self>) {
        if let Some(slot) = self.choices.get_mut(ix) {
            *slot = Some(side);
            cx.notify();
        }
    }

    fn choose_all(&mut self, side: MergeSide, cx: &mut Context<Self>) {
        for slot in &mut self.choices {
            *slot = Some(side);
        }
        cx.notify();
    }

    /// 逐块选边应用:全部选完才允许;重组写回 + git add 在 core runtime 完成
    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.choices.iter().any(|c| c.is_none()) {
            return;
        }
        let choices: Vec<MergeSide> = self.choices.iter().map(|c| c.unwrap()).collect();
        self.busy = true;
        self.status = "应用中…".into();
        cx.notify();
        let cwd = self.cwd.clone();
        let rel = self.rel_path.clone();
        cx.spawn(async move |weak, cx| {
            let result = nib_core::git::git_resolve_by_choices(cwd, rel, choices).await;
            let _ = weak.update(cx, |this: &mut MergeView, cx| {
                this.busy = false;
                match result {
                    Ok(_) => cx.emit(MergeViewEvent::Resolved),
                    Err(err) => {
                        this.status = format!("失败: {}", err).into();
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 整文件快捷方式:git checkout --ours/--theirs + git add
    fn take_whole(&mut self, side: MergeSide, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = "应用中…".into();
        cx.notify();
        let cwd = self.cwd.clone();
        let rel = self.rel_path.clone();
        let flag = match side {
            MergeSide::Ours => "ours",
            MergeSide::Theirs => "theirs",
        };
        cx.spawn(async move |weak, cx| {
            let result =
                nib_core::git::git_checkout_conflict_side(cwd, rel, flag.to_string()).await;
            let _ = weak.update(cx, |this: &mut MergeView, cx| {
                this.busy = false;
                match result {
                    Ok(_) => cx.emit(MergeViewEvent::Resolved),
                    Err(err) => {
                        this.status = format!("失败: {}", err).into();
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn render_pane(
        &self,
        ix: usize,
        side: MergeSide,
        lines: &[SharedString],
        cx: &mut Context<Self>,
    ) -> Div {
        let chosen = self.choices.get(ix).copied().flatten();
        let selected = chosen == Some(side);
        let rejected = chosen.is_some() && !selected;
        let (label, btn_id) = match side {
            MergeSide::Ours => ("当前分支 (ours)", ElementId::from(("take-ours", ix))),
            MergeSide::Theirs => ("并入分支 (theirs)", ElementId::from(("take-theirs", ix))),
        };
        let shown = lines.iter().take(MAX_PANE_LINES);
        let truncated = lines.len().saturating_sub(MAX_PANE_LINES);

        v_flex()
            .flex_1()
            .min_w_0()
            .border_1()
            .rounded(cx.theme().radius)
            .border_color(if selected {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .when(selected, |s| s.bg(cx.theme().list_active))
            .when(rejected, |s| s.opacity(0.45))
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(label),
                    )
                    .child(
                        Button::new(btn_id)
                            .xsmall()
                            .label(if selected { "已采用 ✓" } else { "采用" })
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose(ix, side, cx)
                            })),
                    ),
            )
            .child(
                v_flex()
                    .p_2()
                    .font_family("monospace")
                    .text_size(px(12.))
                    .when(lines.is_empty(), |s| {
                        s.child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("(空 — 该侧删除了这段)"),
                        )
                    })
                    .children(shown.map(|l| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(l.clone())
                    }))
                    .when(truncated > 0, |s| {
                        s.child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("…(还有 {} 行)", truncated)),
                        )
                    }),
            )
    }
}

fn split_lines(text: &str) -> Vec<SharedString> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|l| SharedString::from(l.trim_end_matches('\r').to_string()))
        .collect()
}

impl Render for MergeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.chunks.len();
        let chosen = self.choices.iter().filter(|c| c.is_some()).count();
        let all_chosen = total > 0 && chosen == total;

        let chunk_blocks: Vec<_> = self
            .chunks
            .iter()
            .enumerate()
            .map(|(ix, chunk)| {
                let ours_pane = self.render_pane(ix, MergeSide::Ours, &chunk.ours, cx);
                let theirs_pane = self.render_pane(ix, MergeSide::Theirs, &chunk.theirs, cx);
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("冲突 #{}", ix + 1)),
                    )
                    .child(h_flex().gap_2().items_start().child(ours_pane).child(theirs_pane))
            })
            .collect();

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
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .child(format!("合并冲突 — {}", self.rel_path)),
                    )
                    .child(
                        Button::new("merge-all-ours")
                            .ghost()
                            .xsmall()
                            .label("全用当前分支")
                            .disabled(self.busy || total == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choose_all(MergeSide::Ours, cx)
                            })),
                    )
                    .child(
                        Button::new("merge-all-theirs")
                            .ghost()
                            .xsmall()
                            .label("全用并入分支")
                            .disabled(self.busy || total == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choose_all(MergeSide::Theirs, cx)
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("merge-chunks")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .gap_3()
                    .when(!self.loaded, |s| {
                        s.child(
                            div()
                                .p_2()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .child("解析冲突标记…"),
                        )
                    })
                    .children(chunk_blocks),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{}/{} 块已选边", chosen, total)),
                    )
                    .when(!self.status.is_empty(), |s| {
                        s.child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().danger)
                                .child(self.status.clone()),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("merge-whole-ours")
                            .ghost()
                            .xsmall()
                            .label("整文件用 ours")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.take_whole(MergeSide::Ours, cx)
                            })),
                    )
                    .child(
                        Button::new("merge-whole-theirs")
                            .ghost()
                            .xsmall()
                            .label("整文件用 theirs")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.take_whole(MergeSide::Theirs, cx)
                            })),
                    )
                    .child(
                        Button::new("merge-apply")
                            .primary()
                            .xsmall()
                            .label("应用并标记已解决")
                            .disabled(self.busy || !all_chosen)
                            .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                    ),
            )
    }
}
