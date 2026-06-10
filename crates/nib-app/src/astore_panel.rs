// Astore 面板(M4,对齐旧版 astore-panel 主链):登录 → 项目检测 → 环境切换 →
// 同步代码 → 发布 → 打开编辑器/发布单。reqwest 调用全部经 rt::run 落
// core runtime(与薄壳同纪律);busy 防重入,结果回显状态行。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use nib_core::astore::{AstoreSession, AstoreState};

pub struct AstorePanel {
    state: Arc<AstoreState>,
    project_root: PathBuf,
    session: AstoreSession,
    username_input: Entity<InputState>,
    password_input: Entity<InputState>,
    busy: bool,
    status: SharedString,
}

impl AstorePanel {
    pub fn new(project_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = Arc::new(AstoreState::new(
            crate::session::data_dirs().astore_session,
        ));
        let username_input = cx.new(|cx| InputState::new(window, cx).placeholder("域账号"));
        let password_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("密码").masked(true));
        let mut this = Self {
            state,
            project_root,
            session: AstoreSession::default(),
            username_input,
            password_input,
            busy: false,
            status: "".into(),
        };
        this.reload_session(cx);
        this
    }

    pub fn set_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.project_root = root;
        self.detect(cx);
    }

    fn reload_session(&mut self, cx: &mut Context<Self>) {
        let state = self.state.clone();
        cx.spawn(async move |weak, cx| {
            let session =
                nib_core::rt::run(
                    async move { nib_core::astore::astore_get_session(&state).await },
                )
                .await
                .unwrap_or_default();
            let _ = weak.update(cx, |this: &mut AstorePanel, cx| {
                this.session = session;
                cx.notify();
            });
        })
        .detach();
    }

    fn run_op<F, Fut>(&mut self, label: &str, op: F, cx: &mut Context<Self>)
    where
        F: FnOnce(Arc<AstoreState>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = format!("{}…", label).into();
        cx.notify();
        let state = self.state.clone();
        cx.spawn(async move |weak, cx| {
            let result = nib_core::rt::run(op(state.clone())).await;
            let session =
                nib_core::rt::run(
                    async move { nib_core::astore::astore_get_session(&state).await },
                )
                .await
                .unwrap_or_default();
            let _ = weak.update(cx, |this: &mut AstorePanel, cx| {
                this.busy = false;
                this.session = session;
                this.status = match result {
                    Ok(msg) => msg.into(),
                    Err(err) => format!("失败: {}", err).into(),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn login(&mut self, cx: &mut Context<Self>) {
        let username = self.username_input.read(cx).value().to_string();
        let password = self.password_input.read(cx).value().to_string();
        if username.trim().is_empty() || password.is_empty() {
            self.status = "请输入账号与密码".into();
            cx.notify();
            return;
        }
        self.run_op(
            "登录",
            move |state| async move {
                nib_core::astore::astore_login(&state, username, password)
                    .await
                    .map(|s| format!("已登录: {}", s.nick_name))
            },
            cx,
        );
    }

    fn detect(&mut self, cx: &mut Context<Self>) {
        if self.session.sso_token.is_empty() {
            return;
        }
        let root = self.project_root.to_string_lossy().to_string();
        self.run_op(
            "检测项目",
            move |state| async move {
                nib_core::astore::astore_detect_project(&state, root)
                    .await
                    .map(|s| format!("项目类型: {}", s.project_type))
            },
            cx,
        );
    }

    fn set_env(&mut self, env: &'static str, cx: &mut Context<Self>) {
        self.run_op(
            "切换环境",
            move |state| async move {
                nib_core::astore::astore_set_env(&state, env.to_string())
                    .await
                    .map(|_| format!("已切到{}", env))
            },
            cx,
        );
    }

    fn sync_code(&mut self, cx: &mut Context<Self>) {
        let cwd = self.project_root.to_string_lossy().to_string();
        self.run_op(
            "同步代码",
            move |state| async move {
                let url = nib_core::git::git_remote_url(cwd.clone(), None).await?;
                let branch = nib_core::git::git_current_branch(cwd).await?;
                nib_core::astore::astore_sync_code(&state, url, branch).await
            },
            cx,
        );
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.run_op(
            "发布",
            move |state| async move { nib_core::astore::astore_publish(&state).await },
            cx,
        );
    }

    fn open_url(&mut self, editor: bool, cx: &mut Context<Self>) {
        let state = self.state.clone();
        cx.spawn(async move |weak, cx| {
            let url = nib_core::rt::run(async move {
                if editor {
                    nib_core::astore::astore_get_editor_url(&state).await
                } else {
                    nib_core::astore::astore_get_record_url(&state).await
                }
            })
            .await;
            let _ = weak.update(cx, |this: &mut AstorePanel, cx| match url {
                Ok(url) => {
                    cx.open_url(&url);
                    this.status = "已在浏览器打开".into();
                    cx.notify();
                }
                Err(err) => {
                    this.status = format!("失败: {}", err).into();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        self.run_op(
            "登出",
            move |state| async move {
                nib_core::astore::astore_logout(&state)
                    .await
                    .map(|_| "已登出".to_string())
            },
            cx,
        );
    }
}

impl Render for AstorePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let logged_in = !self.session.sso_token.is_empty();
        let btn = |id: &'static str, label: &'static str| {
            Button::new(id).xsmall().label(label)
        };

        v_flex()
            .size_full()
            .p_2()
            .gap_2()
            .map(|panel| {
                if !logged_in {
                    panel
                        .child(div().text_size(px(12.)).child("Astore 登录"))
                        .child(Input::new(&self.username_input))
                        .child(Input::new(&self.password_input))
                        .child(
                            Button::new("login")
                                .primary()
                                .xsmall()
                                .label("登录")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.login(cx))),
                        )
                } else {
                    panel
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_size(px(12.))
                                .child(self.session.nick_name.clone())
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(self.session.current_env.clone()),
                                )
                                .child(div().flex_1())
                                .child(
                                    Button::new("logout")
                                        .ghost()
                                        .xsmall()
                                        .label("登出")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, _, cx| this.logout(cx))),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "项目: {} · 模板: {}",
                                    self.session.project_type,
                                    self.session
                                        .template_id
                                        .map(|v| v.to_string())
                                        .unwrap_or_else(|| "-".into())
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(btn("env-daily", "日常环境").disabled(self.busy).on_click(
                                    cx.listener(|this, _, _, cx| this.set_env("日常环境", cx)),
                                ))
                                .child(btn("env-pre", "预发环境").disabled(self.busy).on_click(
                                    cx.listener(|this, _, _, cx| this.set_env("预发环境", cx)),
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("sync")
                                        .primary()
                                        .xsmall()
                                        .label("同步代码")
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, _, cx| this.sync_code(cx))),
                                )
                                .child(btn("publish", "发布").disabled(self.busy).on_click(
                                    cx.listener(|this, _, _, cx| this.publish(cx)),
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(btn("open-editor", "打开编辑器").on_click(
                                    cx.listener(|this, _, _, cx| this.open_url(true, cx)),
                                ))
                                .child(btn("open-record", "打开发布单").on_click(
                                    cx.listener(|this, _, _, cx| this.open_url(false, cx)),
                                )),
                        )
                        .child(btn("detect", "重新检测项目").disabled(self.busy).on_click(
                            cx.listener(|this, _, _, cx| this.detect(cx)),
                        ))
                }
            })
            .child(div().flex_1())
            .when(!self.status.is_empty(), |s| {
                s.child(
                    div()
                        .text_size(px(11.))
                        .text_color(cx.theme().muted_foreground)
                        .child(self.status.clone()),
                )
            })
    }
}
