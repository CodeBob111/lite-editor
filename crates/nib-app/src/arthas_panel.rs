// Arthas 诊断面板(底部 dock,镜像 terminal_panel 的挂载与流式拉取)。
// 对齐 nib-arthas.html:头部(连接态 + 5 命令 tab)+ 左参数构造表单 + 右命令预览/控制台。
// 左表单按当前 tab 实时拼命令(真实 Arthas 3.x 语法:watch 观察表达式/时机/-x/-n、
// trace -x/-n、stack -n、monitor -c/-n、tt -t/-x/-n)→ 选目标 Java 进程(jps) →
// 运行:nib_core::arthas_run 把命令 attach 到该 JVM,后台线程流式回显真实输出。
// 找不到 arthas-boot.jar 时显式提示,不伪造。光标定位的默认命令仍走 nib_core::arthas(剪贴板同源)。

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::arthas::ArthasCommand;
use nib_core::arthas_run::{list_java_processes, locate_arthas_boot, ArthasRun, JavaProc};

/// 底部面板高度。左表单 + 右控制台并排需要比纯堆栈略高。
pub const PANEL_HEIGHT: f32 = 340.;

const TABS: [(ArthasCommand, &str); 5] = [
    (ArthasCommand::Watch, "Watch"),
    (ArthasCommand::Trace, "Trace"),
    (ArthasCommand::Stack, "Stack"),
    (ArthasCommand::Monitor, "Monitor"),
    (ArthasCommand::Tt, "TimeTunnel"),
];

fn cmd_blurb(cmd: ArthasCommand) -> &'static str {
    match cmd {
        ArthasCommand::Watch => "观察方法入参 / 返回值 / 异常,无需改代码重启",
        ArthasCommand::Trace => "渲染方法内部调用路径与每段耗时,定位慢在哪一层",
        ArthasCommand::Stack => "输出方法被调用的完整调用栈,回答谁调起来的",
        ArthasCommand::Monitor => "按周期统计调用次数 / 成功失败 / 平均最大耗时",
        ArthasCommand::Tt => "记录每次调用快照进时间隧道,事后可重放回看",
    }
}

/// watch 的观察时机(-b/-s/-e/-f;Default=不带,默认所有阶段)
#[derive(Clone, Copy, PartialEq)]
enum WatchWhen {
    Default,
    Before,
    Success,
    Exception,
    Finish,
}

/// 数值参数标识(左表单 −/+ 步进器统一走 adjust)
#[derive(Clone, Copy)]
enum Param {
    Count,
    Depth,
    Interval,
}

/// watch 观察表达式标识(复选)
#[derive(Clone, Copy)]
enum WatchExpr {
    Params,
    Ret,
    Throw,
    Target,
    Cost,
}

pub struct ArthasPanel {
    fqn: Option<String>,
    method: Option<String>,
    active: ArthasCommand,
    processes: Vec<JavaProc>,
    selected_pid: Option<String>,
    boot: Option<PathBuf>,
    run: Option<Arc<ArthasRun>>,
    running: bool,
    output: Vec<String>,
    status: SharedString,
    // —— 命令参数(左表单驱动)——
    count: u32,    // -n
    depth: u32,    // -x
    interval: u32, // -c(monitor)
    w_params: bool,
    w_ret: bool,
    w_throw: bool,
    w_target: bool,
    w_cost: bool,
    w_when: WatchWhen,
}

impl ArthasPanel {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        let processes = list_java_processes();
        let selected_pid = processes.first().map(|p| p.pid.clone());
        let boot = locate_arthas_boot();
        let status = if boot.is_none() {
            "未找到 arthas-boot.jar:设 ARTHAS_BOOT_JAR 或放到 ~/.arthas-boot.jar 后可一键执行".into()
        } else {
            "把光标放到 Java 方法上,菜单 Arthas → 选命令即可定位目标;选进程后可运行".into()
        };
        Self {
            fqn: None,
            method: None,
            active: ArthasCommand::Watch,
            processes,
            selected_pid,
            boot,
            run: None,
            running: false,
            output: Vec::new(),
            status,
            count: 5,
            depth: 3,
            interval: 5,
            w_params: true,
            w_ret: true,
            w_throw: true,
            w_target: false,
            w_cost: false,
            w_when: WatchWhen::Default,
        }
    }

    /// main.rs::arthas_command 解析出光标处目标后调用:设目标 + 切到对应命令 tab。
    pub fn set_target(
        &mut self,
        fqn: String,
        method: Option<String>,
        cmd: ArthasCommand,
        cx: &mut Context<Self>,
    ) {
        self.fqn = Some(fqn);
        self.method = method;
        self.active = cmd;
        self.status = "已定位目标方法".into();
        cx.notify();
    }

    /// 按当前 tab + 参数拼出真实 Arthas 命令(无目标返回 None)。
    fn build_command(&self) -> Option<String> {
        let fqn = self.fqn.as_ref()?;
        let method = self.method.as_deref().unwrap_or("*");
        let n = self.count;
        let x = self.depth;
        Some(match self.active {
            ArthasCommand::Watch => {
                let mut exprs = Vec::new();
                if self.w_params {
                    exprs.push("params");
                }
                if self.w_ret {
                    exprs.push("returnObj");
                }
                if self.w_throw {
                    exprs.push("throwExp");
                }
                if self.w_target {
                    exprs.push("target");
                }
                if self.w_cost {
                    exprs.push("#cost");
                }
                let exprs = if exprs.is_empty() {
                    "params".to_string()
                } else {
                    exprs.join(",")
                };
                let when = match self.w_when {
                    WatchWhen::Default => "",
                    WatchWhen::Before => "-b ",
                    WatchWhen::Success => "-s ",
                    WatchWhen::Exception => "-e ",
                    WatchWhen::Finish => "-f ",
                };
                format!("watch {fqn} {method} '{{{exprs}}}' {when}-n {n} -x {x}")
            }
            ArthasCommand::Trace => {
                format!("trace {fqn} {method} -n {n} -x {x} --skipJDKMethod false")
            }
            ArthasCommand::Stack => format!("stack {fqn} {method} -n {n}"),
            ArthasCommand::Monitor => {
                format!("monitor {fqn} {method} -c {} -n {n}", self.interval)
            }
            ArthasCommand::Tt => format!("tt -t {fqn} {method} -n {n} -x {x}"),
        })
    }

    fn select(&mut self, cmd: ArthasCommand, cx: &mut Context<Self>) {
        self.active = cmd;
        cx.notify();
    }

    /// 数值步进(−/+),按参数夹到合理区间。
    fn adjust(&mut self, p: Param, delta: i32, cx: &mut Context<Self>) {
        let bump = |v: u32, lo: u32, hi: u32| -> u32 {
            ((v as i32 + delta).clamp(lo as i32, hi as i32)) as u32
        };
        match p {
            Param::Count => self.count = bump(self.count, 1, 999),
            Param::Depth => self.depth = bump(self.depth, 1, 10),
            Param::Interval => self.interval = bump(self.interval, 1, 60),
        }
        cx.notify();
    }

    fn toggle_expr(&mut self, e: WatchExpr, cx: &mut Context<Self>) {
        match e {
            WatchExpr::Params => self.w_params = !self.w_params,
            WatchExpr::Ret => self.w_ret = !self.w_ret,
            WatchExpr::Throw => self.w_throw = !self.w_throw,
            WatchExpr::Target => self.w_target = !self.w_target,
            WatchExpr::Cost => self.w_cost = !self.w_cost,
        }
        cx.notify();
    }

    fn refresh_processes(&mut self, cx: &mut Context<Self>) {
        self.processes = list_java_processes();
        if self
            .selected_pid
            .as_ref()
            .is_none_or(|pid| !self.processes.iter().any(|p| &p.pid == pid))
        {
            self.selected_pid = self.processes.first().map(|p| p.pid.clone());
        }
        self.status = format!("已刷新:发现 {} 个 Java 进程", self.processes.len()).into();
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(cmd) = self.build_command() else {
            self.status = "尚无目标方法,无法生成命令".into();
            cx.notify();
            return;
        };
        self.status = match nib_core::clipboard::copy_text_to_clipboard(cmd) {
            Ok(()) => "命令已复制到剪贴板".into(),
            Err(err) => format!("复制失败: {}", err).into(),
        };
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.run {
            run.kill();
        }
        self.running = false;
        self.status = "已停止".into();
        cx.notify();
    }

    /// 把当前命令打到选中的 Java 进程,后台线程流式回显。
    fn run_command(&mut self, cx: &mut Context<Self>) {
        let Some(cmd) = self.build_command() else {
            self.status = "尚无目标方法,无法执行".into();
            cx.notify();
            return;
        };
        let Some(pid) = self.selected_pid.clone() else {
            self.status = "先选一个目标 Java 进程(下方进程行 / 刷新)".into();
            cx.notify();
            return;
        };
        let Some(boot) = self.boot.clone() else {
            self.status =
                "未找到 arthas-boot.jar:设 ARTHAS_BOOT_JAR 或放到 ~/.arthas-boot.jar".into();
            cx.notify();
            return;
        };

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.unbounded_send(());
        });

        match ArthasRun::spawn(&boot, &pid, &cmd, waker) {
            Ok(run) => {
                self.run = Some(Arc::new(run));
                self.running = true;
                self.output.clear();
                self.status = format!("正在 attach pid {} 执行: {}", pid, cmd).into();
                cx.spawn(async move |weak, cx| {
                    while rx.next().await.is_some() {
                        let keep = weak
                            .update(cx, |this: &mut ArthasPanel, cx| {
                                let Some(run) = this.run.clone() else { return false };
                                if run.take_dirty() {
                                    this.output = run.lines();
                                }
                                let finished = run.is_finished();
                                if finished {
                                    this.output = run.lines();
                                    this.running = false;
                                    this.status = "执行结束(命令已完成)".into();
                                }
                                cx.notify();
                                !finished
                            })
                            .unwrap_or(false);
                        if !keep {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(err) => {
                self.status = format!("启动失败: {}", err).into();
            }
        }
        cx.notify();
    }

    // —— 左表单控件 ——

    /// 数值步进器:label  [−] value [+]
    fn stepper(&self, p: Param, label: &str, value: u32, cx: &mut Context<Self>) -> impl IntoElement {
        let btn = |id: ElementId, glyph: &str, p: Param, delta: i32, cx: &mut Context<Self>| {
            div()
                .id(id)
                .w(px(22.))
                .h(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .border_1()
                .border_color(cx.theme().border)
                .text_color(cx.theme().foreground)
                .cursor_pointer()
                .hover(|s| s.bg(cx.theme().accent))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.adjust(p, delta, cx)),
                )
                .child(glyph.to_string())
        };
        h_flex()
            .items_center()
            .justify_between()
            .gap(px(10.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(label.to_string()),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .child(btn(
                        ElementId::Name(format!("dec-{label}").into()),
                        "−",
                        p,
                        -1,
                        cx,
                    ))
                    .child(
                        div()
                            .min_w(px(28.))
                            .text_center()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(13.))
                            .text_color(cx.theme().foreground)
                            .child(format!("{value}")),
                    )
                    .child(btn(
                        ElementId::Name(format!("inc-{label}").into()),
                        "+",
                        p,
                        1,
                        cx,
                    )),
            )
    }

    /// 复选片(watch 观察表达式)
    fn expr_chip(
        &self,
        e: WatchExpr,
        label: &'static str,
        on: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(ElementId::Name(format!("expr-{label}").into()))
            .px(px(9.))
            .py(px(4.))
            .rounded(px(6.))
            .border_1()
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(11.5))
            .cursor_pointer()
            .when(on, |s| {
                s.bg(cx.theme().list_active)
                    .border_color(cx.theme().primary)
                    .text_color(cx.theme().foreground)
            })
            .when(!on, |s| {
                s.border_color(cx.theme().border)
                    .text_color(cx.theme().muted_foreground)
            })
            .hover(|s| s.bg(cx.theme().accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.toggle_expr(e, cx)),
            )
            .child(label)
    }

    /// 单选片(watch 观察时机)
    fn when_chip(
        &self,
        when: WatchWhen,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let on = self.w_when == when;
        div()
            .id(ElementId::Name(format!("when-{label}").into()))
            .px(px(9.))
            .py(px(4.))
            .rounded(px(6.))
            .text_size(px(11.5))
            .cursor_pointer()
            .when(on, |s| s.bg(cx.theme().primary).text_color(cx.theme().background))
            .when(!on, |s| s.text_color(cx.theme().muted_foreground))
            .hover(|s| s.bg(cx.theme().accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.w_when = when;
                    cx.notify();
                }),
            )
            .child(label)
    }
}

impl Render for ArthasPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let mono = cx.theme().mono_font_family.clone();

        // —— 5 命令 tab ——
        let tabs: Vec<_> = TABS
            .iter()
            .map(|(cmd, label)| {
                let cmd = *cmd;
                let selected = self.active == cmd;
                div()
                    .id(*label)
                    .px_3()
                    .py_1()
                    .text_size(px(12.))
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .when(selected, |s| {
                        s.bg(cx.theme().list_active).text_color(fg)
                    })
                    .when(!selected, |s| s.text_color(muted))
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.select(cmd, cx)),
                    )
                    .child(*label)
            })
            .collect();

        let target = match (&self.fqn, &self.method) {
            (Some(fqn), Some(m)) => format!("{} # {}", fqn, m),
            (Some(fqn), None) => format!("{} # *", fqn),
            _ => "(未定位目标方法)".to_string(),
        };
        let command = self.build_command().unwrap_or_else(|| "—".to_string());
        let can_run = self.boot.is_some() && self.selected_pid.is_some() && self.fqn.is_some();

        // —— 连接态(头部右侧):进程数 / 选中 pid / boot ——
        let conn = if self.boot.is_some() {
            format!(
                "{} · pid {} · {} 进程",
                "arthas-boot ✓",
                self.selected_pid.as_deref().unwrap_or("—"),
                self.processes.len()
            )
        } else {
            "arthas-boot ✗".to_string()
        };

        // —— 左表单:目标 + 说明 + 按 tab 的参数 ——
        let mut form = v_flex()
            .id("arthas-form")
            .w(px(360.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(border)
            .px(px(14.))
            .py(px(12.))
            .gap(px(12.))
            .child(
                v_flex()
                    .gap(px(3.))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(12.))
                            .text_color(fg)
                            .child(target),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child(cmd_blurb(self.active)),
                    ),
            );

        // watch:观察表达式 + 时机 + -x;其余 tab:按需 -x/-c;公共 -n
        if matches!(self.active, ArthasCommand::Watch) {
            form = form
                .child(
                    v_flex()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(muted)
                                .child("观察表达式"),
                        )
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap(px(6.))
                                .child(self.expr_chip(WatchExpr::Params, "params", self.w_params, cx))
                                .child(self.expr_chip(WatchExpr::Ret, "returnObj", self.w_ret, cx))
                                .child(self.expr_chip(WatchExpr::Throw, "throwExp", self.w_throw, cx))
                                .child(self.expr_chip(WatchExpr::Target, "target", self.w_target, cx))
                                .child(self.expr_chip(WatchExpr::Cost, "#cost", self.w_cost, cx)),
                        ),
                )
                .child(
                    v_flex()
                        .gap(px(6.))
                        .child(div().text_size(px(11.)).text_color(muted).child("观察时机"))
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap(px(4.))
                                .child(self.when_chip(WatchWhen::Default, "默认", cx))
                                .child(self.when_chip(WatchWhen::Before, "前 -b", cx))
                                .child(self.when_chip(WatchWhen::Success, "成功 -s", cx))
                                .child(self.when_chip(WatchWhen::Exception, "异常 -e", cx))
                                .child(self.when_chip(WatchWhen::Finish, "完成 -f", cx)),
                        ),
                )
                .child(self.stepper(Param::Depth, "遍历深度 -x", self.depth, cx));
        }
        if matches!(self.active, ArthasCommand::Trace | ArthasCommand::Tt) {
            form = form.child(self.stepper(Param::Depth, "遍历深度 -x", self.depth, cx));
        }
        if matches!(self.active, ArthasCommand::Monitor) {
            form = form.child(self.stepper(Param::Interval, "统计周期 -c(秒)", self.interval, cx));
        }
        form = form.child(self.stepper(Param::Count, "执行次数 -n", self.count, cx));

        // —— 右侧:命令预览 + 复制/运行 + 控制台 ——
        let run_btn = if self.running {
            div()
                .id("arthas-stop")
                .px_3()
                .py_1()
                .text_size(px(12.))
                .rounded(cx.theme().radius)
                .bg(cx.theme().danger)
                .text_color(cx.theme().background)
                .cursor_pointer()
                .hover(|s| s.opacity(0.9))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.stop(cx)))
                .child("停止")
        } else {
            div()
                .id("arthas-run")
                .px_3()
                .py_1()
                .text_size(px(12.))
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .when(can_run, |s| {
                    s.bg(cx.theme().primary).text_color(cx.theme().background)
                })
                .when(!can_run, |s| {
                    s.bg(cx.theme().secondary).text_color(muted)
                })
                .hover(|s| s.opacity(0.9))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.run_command(cx)),
                )
                .child("运行")
        };

        let console_rows: Vec<_> = self
            .output
            .iter()
            .map(|line| {
                let is_log = line.starts_with('[') || line.starts_with("Press ");
                div()
                    .px_2()
                    .text_size(px(11.5))
                    .font_family(mono.clone())
                    .whitespace_nowrap()
                    .when(is_log, |s| s.text_color(muted))
                    .when(!is_log, |s| s.text_color(fg))
                    .child(line.clone())
            })
            .collect();

        let right = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            // 命令预览 + 复制 + 运行
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(12.5))
                            .font_family(mono.clone())
                            .text_color(fg)
                            .child(format!("$ {}", command)),
                    )
                    .child(
                        div()
                            .id("arthas-copy")
                            .px_3()
                            .py_1()
                            .text_size(px(12.))
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(border)
                            .text_color(fg)
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().accent))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.copy(cx)),
                            )
                            .child("复制"),
                    )
                    .child(run_btn),
            )
            // 目标进程行
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .flex_wrap()
                    .child(div().text_size(px(11.)).text_color(muted).child("目标进程"))
                    .when(self.processes.is_empty(), |s| {
                        s.child(
                            div()
                                .text_size(px(11.))
                                .text_color(muted)
                                .child("未发现 Java 进程,先启动应用再点 ⟳"),
                        )
                    })
                    .children(self.processes.iter().map(|p| {
                        let pid = p.pid.clone();
                        let selected = self.selected_pid.as_deref() == Some(pid.as_str());
                        div()
                            .id(SharedString::from(format!("proc-{}", pid)))
                            .px_2()
                            .py_0p5()
                            .text_size(px(11.))
                            .rounded(cx.theme().radius)
                            .border_1()
                            .font_family(mono.clone())
                            .cursor_pointer()
                            .when(selected, |s| {
                                s.border_color(cx.theme().primary).text_color(cx.theme().primary)
                            })
                            .when(!selected, |s| s.border_color(border).text_color(muted))
                            .hover(|s| s.bg(cx.theme().accent))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.selected_pid = Some(pid.clone());
                                    cx.notify();
                                }),
                            )
                            .child(format!("{} · {}", p.name, p.pid))
                    })),
            )
            // boot 缺失提示
            .when(self.boot.is_none(), |s| {
                s.child(
                    div()
                        .mx_3()
                        .my_1()
                        .px_3()
                        .py_1()
                        .text_size(px(11.))
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().secondary)
                        .text_color(cx.theme().warning)
                        .child(
                            "未找到 arthas-boot.jar —— 复制命令可手动执行;设 ARTHAS_BOOT_JAR 后可一键运行",
                        ),
                )
            })
            // 控制台
            .child(
                v_flex()
                    .id("arthas-console")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .when(self.output.is_empty(), |s| {
                        s.child(
                            div()
                                .px_2()
                                .text_size(px(11.5))
                                .text_color(muted)
                                .child("控制台:运行后这里流式显示目标 JVM 的真实回显"),
                        )
                    })
                    .children(console_rows),
            );

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            // 头:标题 + 5 tab + 连接态 + 刷新
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(34.))
                    .flex_none()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child("ARTHAS 诊断"),
                    )
                    .children(tabs)
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(10.5))
                            .text_color(muted)
                            .child(conn),
                    )
                    .child(
                        div()
                            .id("arthas-refresh")
                            .px_2()
                            .py_0p5()
                            .text_size(px(11.))
                            .rounded(cx.theme().radius)
                            .text_color(muted)
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().accent).text_color(fg))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.refresh_processes(cx)),
                            )
                            .child("⟳"),
                    ),
            )
            // 体:左表单 | 右控制台
            .child(h_flex().flex_1().min_h_0().child(form).child(right))
            // 状态栏
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .flex_none()
                    .border_t_1()
                    .border_color(border)
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(self.status.clone()),
            )
    }
}
