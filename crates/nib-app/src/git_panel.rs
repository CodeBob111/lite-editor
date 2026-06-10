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
    tab::{Tab, TabBar},
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use nib_core::git::{GitBranch, GitChange, GitCommit};

pub enum GitPanelEvent {
    OpenDiff { rel_path: String, abs_path: PathBuf },
}

#[derive(PartialEq, Clone, Copy)]
enum GitView {
    Changes,
    Branches,
    Log,
}

pub struct GitPanel {
    window_handle: AnyWindowHandle,
    project_root: PathBuf,
    view: GitView,
    branch: SharedString,
    changes: Vec<GitChange>,
    branches: Vec<GitBranch>,
    log: Vec<GitCommit>,
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
            view: GitView::Changes,
            branch: "".into(),
            changes: Vec::new(),
            branches: Vec::new(),
            log: Vec::new(),
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
        self.branches.clear();
        self.log.clear();
        self.branch = "".into();
        self.refresh(cx);
    }

    /// 拉取分支与变更(序号守卫:慢结果不覆盖新查询)
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_seq += 1;
        let seq = self.refresh_seq;
        let cwd = self.project_root.to_string_lossy().to_string();
        cx.spawn(async move |weak, cx| {
            let branch = nib_core::git::git_current_branch(cwd.clone())
                .await
                .unwrap_or_default();
            let changes = nib_core::git::git_status(cwd.clone()).await;
            let branches = nib_core::git::git_list_branches(cwd.clone()).await;
            let log = if branch.is_empty() {
                Ok(Vec::new())
            } else {
                nib_core::git::git_log(cwd, branch.clone(), Some(50)).await
            };
            let _ = weak.update(cx, |this, cx| {
                if this.refresh_seq != seq {
                    return;
                }
                this.branch = branch.into();
                this.changes = changes.unwrap_or_default();
                this.branches = branches.unwrap_or_default();
                this.log = log.unwrap_or_default();
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

    fn checkout(&mut self, branch: String, cx: &mut Context<Self>) {
        if self.busy || branch == self.branch.as_ref() {
            return;
        }
        self.busy = true;
        self.status = format!("切换到 {} …", branch).into();
        cx.notify();
        let cwd = self.project_root.to_string_lossy().to_string();
        cx.spawn(async move |weak, cx| {
            let result = nib_core::git::git_checkout(cwd, branch.clone(), None).await;
            let _ = weak.update(cx, |this: &mut GitPanel, cx| {
                this.busy = false;
                this.status = match &result {
                    Ok(_) => format!("已切换到 {} ✓", branch).into(),
                    Err(err) => format!("切换失败: {}", err).into(),
                };
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_branches(&self, cx: &mut Context<Self>) -> Vec<Div> {
        self.branches
            .iter()
            .filter(|b| !b.remote)
            .map(|b| {
                let name = b.name.clone();
                let current = b.current;
                let mut row = h_flex()
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .hover(|s| s.bg(cx.theme().accent));
                if current {
                    row = row
                        .bg(cx.theme().list_active)
                        .font_weight(FontWeight::BOLD);
                }
                let to_switch = name.clone();
                row.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut GitPanel, _, _, cx| {
                        this.checkout(to_switch.clone(), cx)
                    }),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().child(name))
                .when(b.ahead > 0 || b.behind > 0, |s| {
                    s.child(
                        div()
                            .text_size(px(10.))
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("↑{} ↓{}", b.ahead, b.behind)),
                    )
                })
            })
            .collect()
    }

    fn render_log(&self, cx: &mut Context<Self>) -> Vec<Div> {
        self.log
            .iter()
            .map(|c| {
                h_flex()
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_start()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .hover(|s| s.bg(cx.theme().accent))
                    .child(
                        div()
                            .text_color(cx.theme().info)
                            .font_family("monospace")
                            .child(c.short_hash.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(c.subject.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(c.date.clone()),
                    )
            })
            .collect()
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
                let rel = change.path.clone();
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
                            cx.emit(GitPanelEvent::OpenDiff {
                                rel_path: rel.clone(),
                                abs_path: abs.clone(),
                            });
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
                TabBar::new("git-views")
                    .w_full()
                    .underline()
                    .selected_index(match self.view {
                        GitView::Changes => 0,
                        GitView::Branches => 1,
                        GitView::Log => 2,
                    })
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.view = match ix {
                            1 => GitView::Branches,
                            2 => GitView::Log,
                            _ => GitView::Changes,
                        };
                        cx.notify();
                    }))
                    .child(Tab::new().label("变更"))
                    .child(Tab::new().label("分支"))
                    .child(Tab::new().label("历史")),
            )
            .child(
                v_flex()
                    .id("git-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .map(|body| match self.view {
                        GitView::Changes => body
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
                        GitView::Branches => body.children(self.render_branches(cx)),
                        GitView::Log => body.children(self.render_log(cx)),
                    }),
            )
            .when(self.view == GitView::Changes, |panel| panel.child(
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
            ))
    }
}
