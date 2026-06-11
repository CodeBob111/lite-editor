// Arthas 诊断面板(底部 dock,镜像 terminal_panel 的挂载与流式拉取):
// 命令构造器 + 真实执行。选命令 tab(Watch/Trace/Stack/Monitor/TimeTunnel)+
// 目标方法(编辑区光标经 nib-core::arthas 解析,main.rs::arthas_command 注入)→
// 实时拼出命令(全部出自 nib_core::arthas::generate_arthas_command,与剪贴板同源)→
// 选目标 Java 进程(jps) → 运行:nib_core::arthas_run 把命令打到该 JVM,后台线程
// 流式回显真实输出(params/returnObj/cost)。找不到 arthas-boot.jar 时显式提示,不伪造。

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::arthas::{generate_arthas_command, ArthasCommand};
use nib_core::arthas_run::{list_java_processes, locate_arthas_boot, ArthasRun, JavaProc};

/// 底部面板高度,与 terminal_panel::PANEL_HEIGHT 同量级。
pub const PANEL_HEIGHT: f32 = 300.;

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

    fn current_command(&self) -> Option<String> {
        let fqn = self.fqn.as_ref()?;
        Some(generate_arthas_command(fqn, self.method.as_deref(), self.active))
    }

    fn select(&mut self, cmd: ArthasCommand, cx: &mut Context<Self>) {
        self.active = cmd;
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
        let Some(cmd) = self.current_command() else {
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
        let Some(cmd) = self.current_command() else {
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
                // 拉取循环:waker 每来一批新行就 take_dirty→快照→notify;进程结束则收尾退出。
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
}

impl Render for ArthasPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .when(selected, |s| {
                        s.bg(cx.theme().list_active).text_color(cx.theme().foreground)
                    })
                    .when(!selected, |s| s.text_color(cx.theme().muted_foreground))
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
        let command = self.current_command().unwrap_or_else(|| "—".to_string());
        let can_run = self.boot.is_some() && self.selected_pid.is_some() && self.fqn.is_some();

        // 目标进程 chips(jps)
        let proc_chips: Vec<_> = self
            .processes
            .iter()
            .map(|p| {
                let pid = p.pid.clone();
                let selected = self.selected_pid.as_deref() == Some(pid.as_str());
                div()
                    .id(SharedString::from(format!("proc-{}", pid)))
                    .px_2()
                    .py_0p5()
                    .text_size(px(11.))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .font_family(cx.theme().mono_font_family.clone())
                    .when(selected, |s| {
                        s.border_color(cx.theme().primary)
                            .text_color(cx.theme().primary)
                    })
                    .when(!selected, |s| {
                        s.border_color(cx.theme().border)
                            .text_color(cx.theme().muted_foreground)
                    })
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_pid = Some(pid.clone());
                            cx.notify();
                        }),
                    )
                    .child(format!("{} · {}", p.name, p.pid))
            })
            .collect();

        // 控制台输出行(真实 arthas 回显;[INFO]/[ERROR] 等日志行弱化)
        let console_rows: Vec<_> = self
            .output
            .iter()
            .map(|line| {
                let is_log = line.starts_with('[') || line.starts_with("Press ");
                div()
                    .px_2()
                    .text_size(px(11.5))
                    .font_family(cx.theme().mono_font_family.clone())
                    .whitespace_nowrap()
                    .when(is_log, |s| s.text_color(cx.theme().muted_foreground))
                    .when(!is_log, |s| s.text_color(cx.theme().foreground))
                    .child(line.clone())
            })
            .collect();

        let run_btn = if self.running {
            div()
                .id("arthas-stop")
                .px_3()
                .py_1()
                .text_size(px(12.))
                .rounded(cx.theme().radius)
                .bg(cx.theme().danger)
                .text_color(cx.theme().background)
                .hover(|s| s.opacity(0.9))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.stop(cx)),
                )
                .child("停止")
        } else {
            div()
                .id("arthas-run")
                .px_3()
                .py_1()
                .text_size(px(12.))
                .rounded(cx.theme().radius)
                .when(can_run, |s| {
                    s.bg(cx.theme().primary).text_color(cx.theme().background)
                })
                .when(!can_run, |s| {
                    s.bg(cx.theme().secondary)
                        .text_color(cx.theme().muted_foreground)
                })
                .hover(|s| s.opacity(0.9))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.run_command(cx)),
                )
                .child("运行")
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            // 头:标题 + 命令 tab + 刷新进程
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(34.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child("ARTHAS 诊断"),
                    )
                    .children(tabs)
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("arthas-refresh")
                            .px_2()
                            .py_0p5()
                            .text_size(px(11.))
                            .rounded(cx.theme().radius)
                            .text_color(cx.theme().muted_foreground)
                            .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.refresh_processes(cx)),
                            )
                            .child("⟳ 刷新进程"),
                    ),
            )
            // 目标 + 说明
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().foreground)
                            .child(target),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(cmd_blurb(self.active)),
                    ),
            )
            // 生成的命令 + 复制 + 运行/停止
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(12.5))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().foreground)
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
                            .border_color(cx.theme().border)
                            .text_color(cx.theme().foreground)
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
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child("目标进程"),
                    )
                    .when(self.processes.is_empty(), |s| {
                        s.child(
                            div()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground)
                                .child("未发现 Java 进程,先启动你的应用,再点 ⟳ 刷新"),
                        )
                    })
                    .children(proc_chips),
            )
            // arthas-boot 缺失提示(显式,不伪造执行)
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
                            "未找到 arthas-boot.jar —— 复制命令可手动执行;设 ARTHAS_BOOT_JAR 或放到 ~/.arthas-boot.jar 后即可一键运行",
                        ),
                )
            })
            // 控制台输出(真实 arthas 回显)
            .child(
                v_flex()
                    .id("arthas-console")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .when(self.output.is_empty(), |s| {
                        s.child(
                            div()
                                .px_2()
                                .text_size(px(11.5))
                                .text_color(cx.theme().muted_foreground)
                                .child("控制台:运行后这里流式显示目标 JVM 的真实回显"),
                        )
                    })
                    .children(console_rows),
            )
            // 状态栏
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone()),
            )
    }
}
