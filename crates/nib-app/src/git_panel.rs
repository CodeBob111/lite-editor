// Git 面板(M2a,对齐旧版 changes-panel 核心流):当前分支 + 变更列表 +
// commit message + Commit / Commit&Push。数据全走 nib-core git 模块
// (core runtime 上跑),刷新带序号守卫;watcher 的 FileChanged 也会触发刷新。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use nib_core::git::GitChange;

pub enum GitPanelEvent {
    OpenFile(PathBuf),
}

pub struct GitPanel {
    window_handle: AnyWindowHandle,
    project_root: PathBuf,
    branch: SharedString,
    changes: Vec<GitChange>,
    message_input: Entity<InputState>,
    busy: bool,
    status: SharedString,
    refresh_seq: u64,
}

impl EventEmitter<GitPanelEvent> for GitPanel {}

impl GitPanel {
    pub fn new(project_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder("Commit message…")
        });
        let mut this = Self {
            window_handle: window.window_handle(),
            project_root,
            branch: "".into(),
            changes: Vec::new(),
            message_input,
            busy: false,
            status: "".into(),
            refresh_seq: 0,
        };
        this.refresh(cx);
        this
    }

    pub fn set_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.project_root = root;
        self.changes.clear();
        self.branch = "".into();
        self.refresh(cx);
    }

    /// 拉取分支与变更(序号守卫:慢结果不覆盖新查询)
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_seq += 1;
        let seq = self.refresh_seq;
        let cwd = self.project_root.to_string_lossy().to_string();
        cx.spawn(async move |weak, cx| {
            let branch = nib_core::git::git_current_branch(cwd.clone()).await;
            let changes = nib_core::git::git_status(cwd).await;
            let _ = weak.update(cx, |this, cx| {
                if this.refresh_seq != seq {
                    return;
                }
                this.branch = branch.unwrap_or_default().into();
                this.changes = changes.unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
    }

    fn commit(&mut self, and_push: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let message = self.message_input.read(cx).value().to_string();
        if message.trim().is_empty() {
            self.status = "commit message 不能为空".into();
            cx.notify();
            return;
        }
        if self.changes.is_empty() {
            self.status = "没有可提交的变更".into();
            cx.notify();
            return;
        }
        let files: Vec<String> = self.changes.iter().map(|c| c.path.clone()).collect();
        let cwd = self.project_root.to_string_lossy().to_string();
        let branch = self.branch.to_string();
        self.busy = true;
        self.status = if and_push { "提交并推送中…" } else { "提交中…" }.into();
        cx.notify();

        let input = self.message_input.clone();
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            let mut result = nib_core::git::git_commit(cwd.clone(), files, message).await;
            if result.is_ok() && and_push {
                result = nib_core::git::git_push(cwd, branch).await;
            }
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut GitPanel, cx| {
                    this.busy = false;
                    this.status = match &result {
                        Ok(_) => {
                            input.update(cx, |state, cx| state.set_value("", window, cx));
                            if and_push { "已提交并推送 ✓" } else { "已提交 ✓" }.into()
                        }
                        Err(err) => format!("失败: {}", err).into(),
                    };
                    this.refresh(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn status_color(status: &str, cx: &App) -> Hsla {
        match status {
            "Modified" => cx.theme().warning,
            "Added" | "Untracked" => cx.theme().success,
            "Deleted" => cx.theme().danger,
            _ => cx.theme().info,
        }
    }
}

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.project_root.clone();
        let rows: Vec<_> = self
            .changes
            .iter()
            .enumerate()
            .map(|(ix, change)| {
                let abs = root.join(&change.path);
                let color = Self::status_color(&change.status, cx);
                let mark: SharedString = change
                    .status
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_default()
                    .into();
                h_flex()
                    .id(ix)
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.emit(GitPanelEvent::OpenFile(abs.clone()));
                            let _ = this;
                        }),
                    )
                    .child(
                        div()
                            .w(px(14.))
                            .text_color(color)
                            .font_weight(FontWeight::BOLD)
                            .child(mark),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .child(change.path.clone()),
                    )
                    .when(change.staged, |s| {
                        s.child(
                            div()
                                .text_size(px(10.))
                                .text_color(cx.theme().muted_foreground)
                                .child("staged"),
                        )
                    })
            })
            .collect();

        v_flex()
            .size_full()
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
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child("分支"),
                    )
                    .child(div().text_size(px(12.)).child(self.branch.clone()))
                    .child(div().flex_1())
                    .child(
                        Button::new("refresh")
                            .ghost()
                            .xsmall()
                            .label("刷新")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("git-changes")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .when(self.changes.is_empty(), |s| {
                        s.child(
                            div()
                                .p_2()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .child("工作区干净"),
                        )
                    })
                    .children(rows),
            )
            .child(
                v_flex()
                    .p_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .h(px(56.))
                            .child(Input::new(&self.message_input).size_full()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("commit")
                                    .primary()
                                    .xsmall()
                                    .label("Commit")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.commit(false, cx))),
                            )
                            .child(
                                Button::new("commit-push")
                                    .xsmall()
                                    .label("Commit & Push")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.commit(true, cx))),
                            ),
                    )
                    .when(!self.status.is_empty(), |s| {
                        s.child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child(self.status.clone()),
                        )
                    }),
            )
    }
}
