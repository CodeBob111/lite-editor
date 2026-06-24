// Maven 面板:对齐 IDEA Maven 工具窗——层级树(根项目 → 嵌套子模块,每个模块可展开
// 出 Lifecycle〔clean/compile/test/package/install,可点击运行〕+ Dependencies〔懒加载
// mvn dependency:tree〕)+ 底部独立输出面板(展示 goal 运行日志)。解析全在 nib-core。

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use nib_core::maven::{MavenConfig, MavenDepTree, MavenModule};

/// IDEA 对齐的生命周期 goal(点击直接 `mvn <goal>`)。
const LIFECYCLE_GOALS: [&str; 5] = ["clean", "compile", "test", "package", "install"];

/// 输出面板渲染上限行数:mvn 日志可达数千行,只渲染尾部(BUILD SUCCESS/报错都在末尾),
/// 否则成千上万个 div 不虚拟化会卡。
const OUTPUT_TAIL_LINES: usize = 400;

pub struct MavenPanel {
    project_root: PathBuf,
    modules: Vec<MavenModule>,
    /// 树展开状态:节点 id 集合(m:/lc:/dep: 前缀 + pom_path)。
    expanded: HashSet<String>,
    /// 各模块依赖树缓存(key = pom_path),Dependencies 节点展开时懒加载。
    dep_trees: HashMap<String, MavenDepTree>,
    /// 正在跑 dependency:tree 的模块(key = pom_path)。
    dep_loading: HashSet<String>,
    /// 正在运行的 goal 描述(非 None = 输出面板显示「运行中」)。
    running_goal: Option<String>,
    /// 最近一次 goal 运行结果(输出面板内容)。
    output: Option<MavenOutput>,
    status: SharedString,
    seq: u64,
    /// 用户配置的 Maven 信息(home/settings/repo);从设置注入。
    config: MavenConfig,
}

/// 一次 goal 运行的输出面板内容。
struct MavenOutput {
    title: String,
    exit_code: i32,
    log: SharedString,
}

/// 树扁平化后的一行(递归收集 → 顺序渲染,避免深层嵌套闭包的借用问题)。
enum MavenRow {
    Module {
        ix: usize,
        depth: usize,
        expanded: bool,
    },
    Lifecycle {
        pom: String,
        depth: usize,
        expanded: bool,
    },
    Goal {
        pom: String,
        module_name: String,
        goal: &'static str,
        depth: usize,
    },
    Deps {
        pom: String,
        depth: usize,
        expanded: bool,
        loading: bool,
        loaded: bool,
        count: usize,
    },
    Dep {
        text: String,
        omitted: Option<String>,
        conflict: bool,
        depth: usize,
    },
}

/// 词法规范化路径(处理 `<module>../foo</module>` 这类相对路径),不碰文件系统。
fn normalize_path(p: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl MavenPanel {
    pub fn new(project_root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            project_root,
            modules: Vec::new(),
            expanded: HashSet::new(),
            dep_trees: HashMap::new(),
            dep_loading: HashSet::new(),
            running_goal: None,
            output: None,
            status: "".into(),
            seq: 0,
            config: MavenConfig::default(),
        };
        this.refresh_modules(cx);
        this
    }

    /// 注入/更新 Maven 配置(设置变更时调用)。变了就重刷模块树。
    pub fn set_config(
        &mut self,
        home: String,
        settings: String,
        repo: String,
        cx: &mut Context<Self>,
    ) {
        let changed = self.config.home != home
            || self.config.settings != settings
            || self.config.repo != repo;
        self.config = MavenConfig {
            home,
            settings,
            repo,
        };
        if changed {
            self.refresh_modules(cx);
        }
    }

    pub fn set_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.project_root = root;
        self.modules.clear();
        self.expanded.clear();
        self.dep_trees.clear();
        self.dep_loading.clear();
        self.running_goal = None;
        self.output = None;
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
                this.dep_trees.clear();
                this.dep_loading.clear();
                this.status = if this.modules.is_empty() {
                    "未发现 pom.xml".into()
                } else {
                    format!("{} 个模块", this.modules.len()).into()
                };
                // 默认展开根模块,树不至于全收起。
                let (roots, _) = this.build_hierarchy();
                for r in roots {
                    this.expanded
                        .insert(format!("m:{}", this.modules[r].pom_path));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 从 flat 模块列表 + 各自 `<module>` 名拼出层级:返回 (根模块下标, 每个模块的子模块下标)。
    /// 子模块 = 目录为「本模块目录 / <module> 名(规范化后)」的模块。无父者即为根。
    fn build_hierarchy(&self) -> (Vec<usize>, Vec<Vec<usize>>) {
        let mut dir_to_ix: HashMap<PathBuf, usize> = HashMap::new();
        for (ix, m) in self.modules.iter().enumerate() {
            if let Some(dir) = Path::new(&m.pom_path).parent() {
                dir_to_ix.insert(dir.to_path_buf(), ix);
            }
        }
        let mut children = vec![Vec::new(); self.modules.len()];
        let mut is_child = vec![false; self.modules.len()];
        for (ix, m) in self.modules.iter().enumerate() {
            let Some(dir) = Path::new(&m.pom_path).parent().map(|p| p.to_path_buf()) else {
                continue;
            };
            for name in &m.modules {
                let child_dir = normalize_path(dir.join(name));
                if let Some(&cix) = dir_to_ix.get(&child_dir) {
                    if cix != ix {
                        children[ix].push(cix);
                        is_child[cix] = true;
                    }
                }
            }
        }
        let roots: Vec<usize> = (0..self.modules.len())
            .filter(|&i| !is_child[i])
            .collect();
        (roots, children)
    }

    /// 递归把某模块子树展平成可见行(只下钻已展开的节点)。
    fn collect_rows(
        &self,
        ix: usize,
        depth: usize,
        children: &[Vec<usize>],
        visited: &mut HashSet<usize>,
        out: &mut Vec<MavenRow>,
    ) {
        if !visited.insert(ix) {
            return; // 防环
        }
        let m = &self.modules[ix];
        let pom = m.pom_path.clone();
        let m_expanded = self.expanded.contains(&format!("m:{}", pom));
        out.push(MavenRow::Module {
            ix,
            depth,
            expanded: m_expanded,
        });
        if !m_expanded {
            return;
        }

        // Lifecycle 节点
        let lc_expanded = self.expanded.contains(&format!("lc:{}", pom));
        out.push(MavenRow::Lifecycle {
            pom: pom.clone(),
            depth: depth + 1,
            expanded: lc_expanded,
        });
        if lc_expanded {
            for goal in LIFECYCLE_GOALS {
                out.push(MavenRow::Goal {
                    pom: pom.clone(),
                    module_name: m.name.clone(),
                    goal,
                    depth: depth + 2,
                });
            }
        }

        // Dependencies 节点(懒加载)
        let dep_expanded = self.expanded.contains(&format!("dep:{}", pom));
        let loading = self.dep_loading.contains(&pom);
        let loaded = self.dep_trees.contains_key(&pom);
        let count = self
            .dep_trees
            .get(&pom)
            .map(|t| t.flat.len())
            .unwrap_or(0);
        out.push(MavenRow::Deps {
            pom: pom.clone(),
            depth: depth + 1,
            expanded: dep_expanded,
            loading,
            loaded,
            count,
        });
        if dep_expanded {
            if let Some(tree) = self.dep_trees.get(&pom) {
                for d in &tree.flat {
                    out.push(MavenRow::Dep {
                        text: format!("{} : {}", d.artifact_id, d.version),
                        omitted: d.omitted_for.clone(),
                        conflict: d.is_conflict,
                        depth: depth + 2,
                    });
                }
            }
        }

        // 子模块
        for &cix in &children[ix] {
            self.collect_rows(cix, depth + 1, children, visited, out);
        }
    }

    fn toggle(&mut self, id: String) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
    }

    /// 展开/收起 Dependencies 节点;首次展开触发懒加载。
    fn on_deps_click(&mut self, pom: String, cx: &mut Context<Self>) {
        let id = format!("dep:{}", pom);
        let now_open = !self.expanded.remove(&id);
        if now_open {
            self.expanded.insert(id);
            if !self.dep_trees.contains_key(&pom) && !self.dep_loading.contains(&pom) {
                self.load_deps_for(pom, cx);
            }
        }
        cx.notify();
    }

    /// 跑 `mvn dependency:tree`,结果按 pom_path 缓存(不 seq 守卫——按 key 存,迟到也无害)。
    fn load_deps_for(&mut self, pom: String, cx: &mut Context<Self>) {
        let module_dir = Path::new(&pom)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| self.project_root.to_string_lossy().to_string());
        self.dep_loading.insert(pom.clone());
        let cfg = self.config.clone();
        cx.spawn(async move |weak, cx| {
            let tree = nib_core::maven::maven_dependency_tree(module_dir, cfg).await;
            let _ = weak.update(cx, |this, cx| {
                this.dep_loading.remove(&pom);
                match tree {
                    Ok(tree) => {
                        this.dep_trees.insert(pom.clone(), tree);
                    }
                    Err(err) => {
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

    /// 运行某模块的生命周期 goal,日志进底部输出面板。
    fn run_goal(
        &mut self,
        pom: String,
        module_name: String,
        goal: &'static str,
        cx: &mut Context<Self>,
    ) {
        let module_dir = Path::new(&pom)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| self.project_root.to_string_lossy().to_string());
        self.running_goal = Some(format!("mvn {} — {}", goal, module_name));
        self.output = None;
        self.status = format!("mvn {} — {} 运行中…", goal, module_name).into();
        cx.notify();

        self.seq += 1;
        let seq = self.seq;
        let cfg = self.config.clone();
        let title = format!("{}  ›  mvn {}", module_name, goal);
        cx.spawn(async move |weak, cx| {
            let res = nib_core::maven::maven_run_goal(module_dir, goal.to_string(), cfg).await;
            let _ = weak.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.running_goal = None;
                match res {
                    Ok(r) => {
                        this.status = if r.exit_code == 0 {
                            format!("mvn {} 成功", goal).into()
                        } else {
                            format!("mvn {} 失败(exit {})", goal, r.exit_code).into()
                        };
                        this.output = Some(MavenOutput {
                            title,
                            exit_code: r.exit_code,
                            log: r.log.into(),
                        });
                    }
                    Err(err) => {
                        this.status = if err.contains("No such file")
                            || err.contains("Failed to run mvn")
                        {
                            "未找到 mvn —— 请到 设置(⌘,)→ Maven 配置 Maven home(如 amaven)".into()
                        } else {
                            format!("运行失败: {}", err).into()
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_row(&self, row: MavenRow, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let indent = |depth: usize| px(8. + depth as f32 * 16.);
        let caret = |expanded: bool| if expanded { "▾" } else { "▸" };

        match row {
            MavenRow::Module {
                ix,
                depth,
                expanded,
            } => {
                let m = &self.modules[ix];
                let id_str = format!("m:{}", m.pom_path);
                let name = m.name.clone();
                let version = m.version.clone();
                h_flex()
                    .id(SharedString::from(format!("row-m-{}", ix)))
                    .pl(indent(depth))
                    .pr_2()
                    .py_0p5()
                    .gap_1()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle(id_str.clone());
                            cx.notify();
                        }),
                    )
                    .child(div().w(px(12.)).text_color(muted).child(caret(expanded)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(name),
                    )
                    .child(div().text_size(px(10.)).text_color(muted).child(version))
                    .into_any_element()
            }
            MavenRow::Lifecycle {
                pom,
                depth,
                expanded,
            } => {
                let id_str = format!("lc:{}", pom);
                h_flex()
                    .id(SharedString::from(format!("row-lc-{}", pom)))
                    .pl(indent(depth))
                    .pr_2()
                    .py_0p5()
                    .gap_1()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .text_color(muted)
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle(id_str.clone());
                            cx.notify();
                        }),
                    )
                    .child(div().w(px(12.)).child(caret(expanded)))
                    .child("Lifecycle")
                    .into_any_element()
            }
            MavenRow::Goal {
                pom,
                module_name,
                goal,
                depth,
            } => h_flex()
                .id(SharedString::from(format!("row-g-{}-{}", pom, goal)))
                .pl(indent(depth))
                .pr_2()
                .py_0p5()
                .gap_1()
                .rounded(cx.theme().radius)
                .text_size(px(12.))
                .hover(|s| s.bg(cx.theme().accent))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.run_goal(pom.clone(), module_name.clone(), goal, cx);
                    }),
                )
                .child(div().w(px(12.)).text_color(rgb(0x6aab73)).child("▶"))
                .child(goal)
                .into_any_element(),
            MavenRow::Deps {
                pom,
                depth,
                expanded,
                loading,
                loaded,
                count,
            } => {
                let suffix = if loading {
                    " (加载中…)".to_string()
                } else if loaded {
                    format!(" ({})", count)
                } else {
                    String::new()
                };
                h_flex()
                    .id(SharedString::from(format!("row-d-{}", pom)))
                    .pl(indent(depth))
                    .pr_2()
                    .py_0p5()
                    .gap_1()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .text_color(muted)
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.on_deps_click(pom.clone(), cx);
                        }),
                    )
                    .child(div().w(px(12.)).child(caret(expanded)))
                    .child(format!("Dependencies{}", suffix))
                    .into_any_element()
            }
            MavenRow::Dep {
                text,
                omitted,
                conflict,
                depth,
            } => h_flex()
                .pl(indent(depth))
                .pr_2()
                .gap_2()
                .text_size(px(12.))
                .when(conflict, |s| s.text_color(cx.theme().danger))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(text),
                )
                .when_some(omitted, |s, v| {
                    s.child(
                        div()
                            .text_size(px(10.))
                            .text_color(muted)
                            .child(format!("omitted→{}", v)),
                    )
                })
                .into_any_element(),
        }
    }
}

impl Render for MavenPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 展平树
        let (roots, children) = self.build_hierarchy();
        let mut flat: Vec<MavenRow> = Vec::new();
        let mut visited = HashSet::new();
        for r in roots {
            self.collect_rows(r, 0, &children, &mut visited, &mut flat);
        }
        let rows: Vec<AnyElement> = flat.into_iter().map(|r| self.render_row(r, cx)).collect();

        // 输出面板:运行中或有结果时显示
        let show_output = self.running_goal.is_some() || self.output.is_some();

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("maven-tree")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .children(rows),
            )
            .when(show_output, |s| {
                s.child(
                    v_flex()
                        .h(px(220.))
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            // 输出面板标题栏
                            h_flex()
                                .px_2()
                                .py_1()
                                .gap_2()
                                .bg(cx.theme().muted)
                                .text_size(px(11.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .child(
                                            self.running_goal
                                                .clone()
                                                .or_else(|| {
                                                    self.output.as_ref().map(|o| o.title.clone())
                                                })
                                                .unwrap_or_default(),
                                        ),
                                )
                                .when_some(self.output.as_ref(), |s, o| {
                                    let (txt, col) = if o.exit_code == 0 {
                                        ("BUILD SUCCESS".to_string(), rgb(0x6aab73))
                                    } else {
                                        (format!("FAILED (exit {})", o.exit_code), rgb(0xe06c75))
                                    };
                                    s.child(div().text_color(col).child(txt))
                                }),
                        )
                        .child(
                            v_flex()
                                .id("maven-output")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .px_2()
                                .py_1()
                                .text_size(px(11.))
                                .when(self.running_goal.is_some(), |s| {
                                    s.child(
                                        div()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("运行中…(完成后显示完整日志)"),
                                    )
                                })
                                .when_some(self.output.as_ref(), |s, o| {
                                    let all: Vec<&str> = o.log.lines().collect();
                                    let total = all.len();
                                    let start = total.saturating_sub(OUTPUT_TAIL_LINES);
                                    let mut lines: Vec<AnyElement> = Vec::new();
                                    if start > 0 {
                                        lines.push(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "…(省略前 {} 行,仅显示末 {} 行)",
                                                    start, OUTPUT_TAIL_LINES
                                                ))
                                                .into_any_element(),
                                        );
                                    }
                                    for line in &all[start..] {
                                        // 不设 nowrap:长堆栈/报错行自动换行,不被面板裁掉。
                                        lines.push(div().child(line.to_string()).into_any_element());
                                    }
                                    s.children(lines)
                                }),
                        ),
                )
            })
            .child(
                // 状态行:div + w_full 让长状态按面板宽换行完整显示。
                div()
                    .w_full()
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
