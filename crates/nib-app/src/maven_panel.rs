// Maven 面板(M3,对齐旧版 dep-analyzer 主链):模块列表 → 依赖扁平列表 +
// 冲突高亮/计数。解析全在 nib-core(maven_dependency_tree),带序号守卫。

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::maven::{MavenConfig, MavenDepTree, MavenModule};

pub struct MavenPanel {
    project_root: PathBuf,
    modules: Vec<MavenModule>,
    selected_module: Option<usize>,
    dep_tree: Option<MavenDepTree>,
    loading: bool,
    status: SharedString,
    seq: u64,
    /// 用户配置的 Maven 信息(home/settings/repo);从设置注入。
    config: MavenConfig,
}

impl MavenPanel {
    pub fn new(project_root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            project_root,
            modules: Vec::new(),
            selected_module: None,
            dep_tree: None,
            loading: false,
            status: "".into(),
            seq: 0,
            config: MavenConfig::default(),
        };
        this.refresh_modules(cx);
        this
    }

    /// 注入/更新 Maven 配置(设置变更时调用)。变了就重刷依赖树。
    pub fn set_config(&mut self, home: String, settings: String, repo: String, cx: &mut Context<Self>) {
        let changed = self.config.home != home
            || self.config.settings != settings
            || self.config.repo != repo;
        self.config = MavenConfig { home, settings, repo };
        if changed {
            self.refresh_modules(cx);
        }
    }

    pub fn set_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.project_root = root;
        self.modules.clear();
        self.selected_module = None;
        self.dep_tree = None;
        self.refresh_modules(cx);
    }

    pub fn refresh_modules(&mut self, cx: &mut Context<Self>) {
        self.seq += 1;
        let seq = self.seq;
        let root = self.project_root.to_string_lossy().to_string();
        cx.spawn(async move |weak, cx| {
            let modules = nib_core::maven::parse_maven_modules(root).await;
            let _ = weak.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.modules = modules.unwrap_or_default();
                this.status = if this.modules.is_empty() {
                    "未发现 pom.xml".into()
                } else {
                    format!("{} 个模块", this.modules.len()).into()
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn load_deps(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(module) = self.modules.get(ix) else {
            return;
        };
        self.selected_module = Some(ix);
        self.dep_tree = None;
        self.loading = true;
        self.status = format!("mvn dependency:tree — {} …", module.name).into();
        cx.notify();

        self.seq += 1;
        let seq = self.seq;
        let cfg = self.config.clone();
        let module_dir = PathBuf::from(&module.pom_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| self.project_root.to_string_lossy().to_string());
        cx.spawn(async move |weak, cx| {
            let tree = nib_core::maven::maven_dependency_tree(module_dir, cfg).await;
            let _ = weak.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.loading = false;
                match tree {
                    Ok(tree) => {
                        this.status = format!(
                            "{} 个依赖,{} 组冲突",
                            tree.flat.len(),
                            tree.conflicts.len()
                        )
                        .into();
                        this.dep_tree = Some(tree);
                    }
                    Err(err) => {
                        // mvn 找不到/跑不起来 → 提示去设置配置 Maven(尤其用 amaven 的内网工程)
                        this.status = if err.contains("No such file")
                            || err.contains("Failed to run mvn")
                        {
                            "未找到 mvn —— 请到 设置(⌘,)→ Maven 配置 Maven home(如 amaven)".into()
                        } else {
                            format!("解析失败: {}", err).into()
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for MavenPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let module_rows: Vec<_> = self
            .modules
            .iter()
            .enumerate()
            .map(|(ix, m)| {
                let selected = self.selected_module == Some(ix);
                h_flex()
                    .id(ix)
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .when(selected, |s| s.bg(cx.theme().list_active))
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.load_deps(ix, cx)),
                    )
                    .child(div().flex_1().min_w_0().overflow_hidden().child(m.name.clone()))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(cx.theme().muted_foreground)
                            .child(m.version.clone()),
                    )
            })
            .collect();

        let dep_rows: Vec<_> = self
            .dep_tree
            .as_ref()
            .map(|tree| {
                tree.flat
                    .iter()
                    .map(|d| {
                        h_flex()
                            .px_2()
                            .gap_2()
                            .text_size(px(12.))
                            .when(d.is_conflict, |s| s.text_color(cx.theme().danger))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(format!("{} : {}", d.artifact_id, d.version)),
                            )
                            .when_some(d.omitted_for.clone(), |s, v| {
                                s.child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("omitted→{}", v)),
                                )
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("maven-modules")
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .p_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(module_rows),
            )
            .child(
                v_flex()
                    .id("maven-deps")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .when(self.loading, |s| {
                        s.child(
                            div()
                                .p_2()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .child("mvn dependency:tree 运行中…"),
                        )
                    })
                    .children(dep_rows),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone()),
            )
    }
}
