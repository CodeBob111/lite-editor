// Git 面板(M2a,对齐旧版 changes-panel 核心流):当前分支 + 变更列表 +
// commit message + Commit / Commit&Push。数据全走 nib-core git 模块
// (core runtime 上跑),刷新带序号守卫;watcher 的 FileChanged 也会触发刷新。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    v_flex, ActiveTheme, Disableable as _, Sizable as _,
};
use nib_core::git::{GitBranch, GitChange, GitCommit, GitRepo};

const MAX_RENDERED_CHANGES: usize = 400;

pub enum GitPanelEvent {
    OpenDiff { repo: String, rel_path: String, abs_path: PathBuf },
    OpenMerge { repo: String, rel_path: String },
    /// 刷新拿到的 git status 结果(供 Workbench 据此建编辑器/树的改动标记,
    /// 不必再单独跑一次 git status——合并重复查询)。
    StatusUpdated(Vec<GitChange>),
}

/// 对齐旧版:Commit(变更+提交框)与 Git(分支+历史)是活动栏里两个独立视图,
/// 共享同一份数据/刷新逻辑,由 Workbench 切换 mode
#[derive(PartialEq, Clone, Copy)]
pub enum GitPanelMode {
    Commit,
    Branches,
}

pub struct GitPanel {
    window_handle: AnyWindowHandle,
    project_root: PathBuf,
    /// 多仓工作区:项目下发现的所有 git 仓(根仓 + 一级嵌套仓);单仓项目只有一个。
    /// 空 = 尚未发现或非 git 项目(refresh 时回退为 [project_root])。
    repos: Vec<GitRepo>,
    /// 「分支/历史」视图当前选中的仓索引(指向 repos);Commit 视图聚合所有仓不依赖它。
    active_repo: usize,
    mode: GitPanelMode,
    branch: SharedString,
    /// 所有仓的改动聚合(扁平;每条 GitChange.repo 标明所属仓,path 相对该仓)。
    changes: Vec<GitChange>,
    /// 冲突文件的**绝对**路径(跨仓;= repo.join(rel))。
    conflicts: Vec<String>,
    /// 当前选中的改动(用**绝对**路径唯一标识,跨仓不冲突)。
    selected_change: Option<String>,
    branches: Vec<GitBranch>,
    log: Vec<GitCommit>,
    /// 「历史」区当前展示的是哪个分支的提交(左键点分支只看历史、不切换)。空=当前分支。
    selected_branch: SharedString,
    message_input: Entity<InputState>,
    branch_filter: Entity<InputState>,
    /// 提交信息草稿按项目各自保留(内存级,App 重启丢弃):切项目时存当前项目、取目标项目的。
    /// 否则在 A 写一半的提交信息会跟着串到 B(切回 A 又丢了)。键=项目根。
    commit_drafts: HashMap<PathBuf, String>,
    busy: bool,
    status: SharedString,
    /// branch/status/conflicts(轻量字段)的序号:refresh 与 refresh_light 共用,最新一次生效。
    refresh_seq: u64,
    /// 分支列表 + 历史(重量字段)的独立序号:**只** refresh / select_branch 动它。
    /// refresh_light(watcher 文件事件)不 bump 它——否则 jdtls 导入写 .project/.classpath
    /// 触发的 refresh_light 风暴会把全量 refresh 的 branches+log 结果丢弃,分支列表与历史变空。
    log_seq: u64,
    /// 右键菜单的目标分支:右键某行时 context_menu 闭包把该行分支名写进来,菜单的
    /// 「checkout」action(走 Workbench 的 on_action)再读它去切换。元素级 on_mouse_down(Right)
    /// 对本机不触发,改用 gpui-component 的 .context_menu(window 级,与文件树同款,可靠)。
    ctx_branch: Rc<RefCell<Option<String>>>,
    _branch_filter_sub: Subscription,
}

impl EventEmitter<GitPanelEvent> for GitPanel {}

/// 改动文件的**绝对**路径(= 所属仓路径 join 仓内相对路径)。多仓下唯一标识一条改动。
fn change_abs(c: &GitChange) -> String {
    abs_in_repo(&c.repo, &c.path)
}

fn abs_in_repo(repo: &str, rel: &str) -> String {
    std::path::Path::new(repo)
        .join(rel)
        .to_string_lossy()
        .to_string()
}

impl GitPanel {
    pub fn new(project_root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .placeholder("Commit message…")
        });
        let branch_filter = cx.new(|cx| InputState::new(window, cx).placeholder("搜索分支…"));
        let branch_filter_sub = cx.subscribe(
            &branch_filter,
            |_: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        let mut this = Self {
            window_handle: window.window_handle(),
            project_root,
            repos: Vec::new(),
            active_repo: 0,
            mode: GitPanelMode::Commit,
            branch: "".into(),
            changes: Vec::new(),
            conflicts: Vec::new(),
            selected_change: None,
            branches: Vec::new(),
            log: Vec::new(),
            selected_branch: "".into(),
            message_input,
            branch_filter,
            commit_drafts: HashMap::new(),
            busy: false,
            status: "".into(),
            refresh_seq: 0,
            log_seq: 0,
            ctx_branch: Rc::new(RefCell::new(None)),
            _branch_filter_sub: branch_filter_sub,
        };
        this.refresh(cx);
        this
    }

    pub fn set_mode(&mut self, mode: GitPanelMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
        }
    }

    pub fn branch(&self) -> SharedString {
        self.branch.clone()
    }

    /// 当前分支相对上游的领先/落后(状态栏 ↑↓ 用);无上游或未刷新为 (0,0)。
    pub fn ahead_behind(&self) -> (i32, i32) {
        self.branches
            .iter()
            .find(|b| b.current)
            .map(|b| (b.ahead, b.behind))
            .unwrap_or((0, 0))
    }

    /// 工作区改动文件数(状态栏 ● 用)。
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }

    pub fn set_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        // 提交信息草稿按项目各自保留:存档当前项目的、取出目标项目的(无则空)。
        let current = self.message_input.read(cx).value().to_string();
        if !self.project_root.as_os_str().is_empty() {
            if current.is_empty() {
                self.commit_drafts.remove(&self.project_root);
            } else {
                self.commit_drafts.insert(self.project_root.clone(), current);
            }
        }
        self.project_root = root.clone();
        let draft = self.commit_drafts.get(&root).cloned().unwrap_or_default();
        // set_value 需要 &mut Window(本方法无 window);切项目发生在 listener 的窗口更新里,
        // 同步取窗口会重入 panic → 沿用 commit 后清空提交框的写法,经 window_handle 延后应用。
        // 顺带清空分支搜索框(瞬态过滤,不跨项目保留)。
        let msg = self.message_input.clone();
        let filter = self.branch_filter.clone();
        let wh = self.window_handle;
        cx.spawn(async move |_, cx| {
            let _ = cx.update_window(wh, |_, window, cx| {
                msg.update(cx, |state, cx| state.set_value(draft, window, cx));
                filter.update(cx, |state, cx| state.set_value("", window, cx));
            });
        })
        .detach();
        self.changes.clear();
        self.conflicts.clear();
        self.selected_change = None;
        self.branches.clear();
        self.log.clear();
        self.branch = "".into();
        self.repos.clear();
        self.active_repo = 0;
        self.refresh(cx);
    }

    /// 拉取多仓的分支与变更(序号守卫:慢结果不覆盖新查询)。
    /// 多仓工作区:Commit 视图聚合**所有**仓的改动;分支/历史只针对**活跃仓**(active_repo)。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // 全量刷新(开面板/手动/提交/切换后)回到「看当前分支历史」。watcher 的 refresh_light
        // 不动 selected_branch,这样浏览别的分支历史时文件变更不会把视图跳回当前分支。
        self.selected_branch = "".into();
        self.refresh_seq += 1;
        self.log_seq += 1;
        let rseq = self.refresh_seq;
        let lseq = self.log_seq;
        let root = self.project_root.to_string_lossy().to_string();
        let active_repo = self.active_repo;
        cx.spawn(async move |weak, cx| {
            // 发现所有仓(根仓 + 一级嵌套);非 git 项目则回退为单一项目根。
            let mut repos = nib_core::git::git_discover_repos(root.clone()).await;
            if repos.is_empty() {
                repos.push(GitRepo {
                    name: std::path::Path::new(&root)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| root.clone()),
                    path: root.clone(),
                });
            }
            let repo_paths: Vec<String> = repos.iter().map(|r| r.path.clone()).collect();
            // 所有仓改动并行(rayon),所有仓冲突并行;冲突文件存绝对路径供任意仓改动行判定。
            let (batch, conflicts_per) = futures::join!(
                nib_core::git::git_status_batch(repo_paths.clone()),
                futures::future::join_all(repo_paths.iter().cloned().map(|p| async move {
                    let c = nib_core::git::git_merge_conflicts(p.clone()).await;
                    (p, c)
                })),
            );
            let mut changes = Vec::new();
            for b in batch {
                if let Some(cs) = b.result {
                    changes.extend(cs);
                }
            }
            let mut conflicts = Vec::new();
            for (repo_path, res) in conflicts_per {
                if let Ok(cs) = res {
                    for rel in cs {
                        conflicts.push(abs_in_repo(&repo_path, &rel));
                    }
                }
            }
            // 活跃仓的分支/历史(分支天然 per-repo)。
            let active_path = repos
                .get(active_repo)
                .or_else(|| repos.first())
                .map(|r| r.path.clone())
                .unwrap_or_else(|| root.clone());
            let (branch, branches) = futures::join!(
                nib_core::git::git_current_branch(active_path.clone()),
                nib_core::git::git_list_branches(active_path.clone()),
            );
            let branch = branch.unwrap_or_default();
            let log = if branch.is_empty() {
                Ok(Vec::new())
            } else {
                nib_core::git::git_log(active_path, branch.clone(), Some(50)).await
            };
            let _ = weak.update(cx, |this, cx| {
                if this.refresh_seq == rseq {
                    this.repos = repos;
                    this.branch = branch.into();
                    this.changes = changes;
                    this.conflicts = conflicts;
                    if let Some(sel) = &this.selected_change {
                        if !this.changes.iter().any(|c| change_abs(c) == *sel) {
                            this.selected_change = None;
                        }
                    }
                    cx.emit(GitPanelEvent::StatusUpdated(this.changes.clone()));
                }
                if this.log_seq == lseq {
                    this.branches = branches.unwrap_or_default();
                    this.log = log.unwrap_or_default();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 轻量刷新:只查 branch + status + conflicts(改动标记 + 变更列表所需),**不**拉
    /// 分支列表与 50 条 log。watcher 文件事件 / 保存走这条——分支和 log 重且不随每次文件
    /// 变更变化,只在 set_project / 手动刷新 / 提交后用全量 refresh 加载。与 refresh 共用
    /// refresh_seq,二者互不覆盖(最新一次生效)。
    pub fn refresh_light(&mut self, cx: &mut Context<Self>) {
        self.refresh_seq += 1;
        let seq = self.refresh_seq;
        let root = self.project_root.to_string_lossy().to_string();
        // 复用 refresh 已发现的仓;未发现则回退项目根。轻量刷新不重新发现仓(目录结构罕变)、
        // 也不动 branch/branches/log(只刷所有仓的改动 + 冲突,够改动标记与变更列表用)。
        let repo_paths: Vec<String> = if self.repos.is_empty() {
            vec![root]
        } else {
            self.repos.iter().map(|r| r.path.clone()).collect()
        };
        cx.spawn(async move |weak, cx| {
            let (batch, conflicts_per) = futures::join!(
                nib_core::git::git_status_batch(repo_paths.clone()),
                futures::future::join_all(repo_paths.iter().cloned().map(|p| async move {
                    let c = nib_core::git::git_merge_conflicts(p.clone()).await;
                    (p, c)
                })),
            );
            let mut changes = Vec::new();
            for b in batch {
                if let Some(cs) = b.result {
                    changes.extend(cs);
                }
            }
            let mut conflicts = Vec::new();
            for (repo_path, res) in conflicts_per {
                if let Ok(cs) = res {
                    for rel in cs {
                        conflicts.push(abs_in_repo(&repo_path, &rel));
                    }
                }
            }
            let _ = weak.update(cx, |this, cx| {
                if this.refresh_seq != seq {
                    return;
                }
                this.changes = changes;
                this.conflicts = conflicts;
                if let Some(sel) = &this.selected_change {
                    if !this.changes.iter().any(|c| change_abs(c) == *sel) {
                        this.selected_change = None;
                    }
                }
                cx.emit(GitPanelEvent::StatusUpdated(this.changes.clone()));
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
        // 多仓:按仓分组待提交文件(各仓相对路径),每个仓单独 git_commit(共用同一条 message)。
        let mut by_repo: HashMap<String, Vec<String>> = HashMap::new();
        for c in &self.changes {
            by_repo.entry(c.repo.clone()).or_default().push(c.path.clone());
        }
        let groups: Vec<(String, Vec<String>)> = by_repo.into_iter().collect();
        self.busy = true;
        self.status = if and_push { "提交并推送中…" } else { "提交中…" }.into();
        cx.notify();

        let input = self.message_input.clone();
        let window_handle = self.window_handle;
        cx.spawn(async move |weak, cx| {
            let mut ok = 0usize;
            let mut err: Option<String> = None;
            for (repo, files) in groups {
                match nib_core::git::git_commit(repo.clone(), files, message.clone()).await {
                    Ok(_) => {
                        ok += 1;
                        // 推送各仓自己的当前分支(多仓各分支不同,逐仓查)。
                        if and_push {
                            let branch = nib_core::git::git_current_branch(repo.clone())
                                .await
                                .unwrap_or_default();
                            if !branch.is_empty() {
                                if let Err(e) = nib_core::git::git_push(repo, branch).await {
                                    err.get_or_insert(e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        err.get_or_insert(e);
                    }
                }
            }
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let _ = weak.update(cx, |this: &mut GitPanel, cx| {
                    this.busy = false;
                    this.status = match &err {
                        None => {
                            input.update(cx, |state, cx| state.set_value("", window, cx));
                            if and_push {
                                format!("已提交 {} 个仓并推送 ✓", ok)
                            } else {
                                format!("已提交 {} 个仓 ✓", ok)
                            }
                            .into()
                        }
                        Some(e) => format!("部分失败: {}", e).into(),
                    };
                    this.refresh(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn sync_remote(&mut self, push: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = if push { "推送中…" } else { "拉取中…" }.into();
        cx.notify();
        // Pull/Push 针对「分支/历史」视图当前选中的活跃仓。
        let cwd = self.active_repo_path();
        let branch = self.branch.to_string();
        cx.spawn(async move |weak, cx| {
            let result = if push {
                nib_core::git::git_push(cwd, branch).await
            } else {
                nib_core::git::git_pull(cwd, None, None).await
            };
            let _ = weak.update(cx, |this: &mut GitPanel, cx| {
                this.busy = false;
                this.status = match &result {
                    Ok(out) => {
                        let head = out.lines().next().unwrap_or("完成").to_string();
                        format!("{} ✓", if head.is_empty() { "完成".into() } else { head }).into()
                    }
                    Err(err) => format!("失败: {}", err).into(),
                };
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 活跃仓的路径(分支/历史/Pull/Push/checkout 针对它);越界则回退项目根。
    fn active_repo_path(&self) -> String {
        self.repos
            .get(self.active_repo)
            .map(|r| r.path.clone())
            .unwrap_or_else(|| self.project_root.to_string_lossy().to_string())
    }

    fn selected_change(&self) -> Option<GitChange> {
        self.selected_change
            .as_ref()
            .and_then(|abs| self.changes.iter().find(|c| change_abs(c) == *abs))
            .cloned()
    }

    fn rollback_selected(&mut self, cx: &mut Context<Self>) {
        let Some(change) = self.selected_change() else {
            self.status = "先选择一个变更".into();
            cx.notify();
            return;
        };
        self.rollback_change(change, cx);
    }

    fn rollback_change(&mut self, change: GitChange, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.status = format!("Rollback {} …", change.path).into();
        cx.notify();
        // rollback 作用于该改动所属的仓(多仓:每条改动带 repo)。
        let cwd = change.repo.clone();
        cx.spawn(async move |weak, cx| {
            let result =
                nib_core::git::git_discard_changes(cwd, change.path.clone(), change.status).await;
            let _ = weak.update(cx, |this: &mut GitPanel, cx| {
                this.busy = false;
                this.status = match &result {
                    Ok(_) => format!("已 rollback {}", change.path).into(),
                    Err(err) => format!("Rollback 失败: {}", err).into(),
                };
                this.refresh_light(cx);
                cx.notify();
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
        let cwd = self.active_repo_path();
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

    /// 右键菜单「checkout 切换分支」触发:切到右键的那个分支(render_branches 的 context_menu
    /// 闭包已把该行分支名写进 ctx_branch)。由 Workbench 的 on_action(CheckoutBranch) 调用。
    pub fn checkout_context(&mut self, cx: &mut Context<Self>) {
        let branch = self.ctx_branch.borrow_mut().take();
        if let Some(branch) = branch {
            self.checkout(branch, cx);
        }
    }

    /// 点仓库选择器:切换「分支/历史/Pull/Push/checkout」针对的活跃仓。**只**重取该仓的
    /// branch/branches/log(轻量,不重走所有仓 status——Commit 聚合视图不随活跃仓变,故无需
    /// 重刷改动)。用 log_seq 守卫;先清空旧分支/历史 + notify 让 chip 立即高亮(消除「点了没反应」)。
    fn select_repo(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.repos.len() || i == self.active_repo {
            return;
        }
        self.active_repo = i;
        self.selected_branch = "".into();
        self.branch = "".into();
        self.branches.clear();
        self.log.clear();
        self.log_seq += 1;
        let lseq = self.log_seq;
        let path = self.repos[i].path.clone();
        cx.notify();
        cx.spawn(async move |weak, cx| {
            let (branch, branches) = futures::join!(
                nib_core::git::git_current_branch(path.clone()),
                nib_core::git::git_list_branches(path.clone()),
            );
            let branch = branch.unwrap_or_default();
            let log = if branch.is_empty() {
                Ok(Vec::new())
            } else {
                nib_core::git::git_log(path, branch.clone(), Some(50)).await
            };
            let _ = weak.update(cx, |this: &mut GitPanel, cx| {
                if this.log_seq != lseq {
                    return;
                }
                this.branch = branch.into();
                this.branches = branches.unwrap_or_default();
                this.log = log.unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
    }

    /// 左键点分支:只把该分支的提交历史加载到「历史」区,**不切换分支**(切换用右键)。
    /// 复用 refresh_seq 守卫:期间发生 refresh / 又点别的分支则丢弃本次慢结果。
    fn select_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        self.selected_branch = branch.clone().into();
        // 只动历史 → 用 log_seq(与 refresh / 别的 select 互不覆盖,且不受 refresh_light 影响)。
        self.log_seq += 1;
        let seq = self.log_seq;
        let cwd = self.active_repo_path();
        cx.notify();
        cx.spawn(async move |weak, cx| {
            let log = nib_core::git::git_log(cwd, branch, Some(50)).await;
            let _ = weak.update(cx, |this: &mut GitPanel, cx| {
                if this.log_seq != seq {
                    return;
                }
                this.log = log.unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
    }

    fn render_branches(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // 「正在看历史」的分支(左键选中);空=看当前分支。供高亮用,先取出避免在 map 里借 self。
        let selected = self.selected_branch.clone();
        let ctx_branch = self.ctx_branch.clone();
        let query = self.branch_filter.read(cx).value().trim().to_lowercase();
        self.branches
            .iter()
            .filter(|b| !b.remote && (query.is_empty() || b.name.to_lowercase().contains(&query)))
            .map(|b| {
                let name = b.name.clone();
                let current = b.current;
                // 当前在看其历史的分支(左键点的那个)。current 分支默认即在看(selected 为空时)。
                let viewing = if selected.is_empty() {
                    current
                } else {
                    selected.as_ref() == name.as_str()
                };
                let mut row = h_flex()
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .hover(|s| s.bg(cx.theme().accent));
                // 当前分支加粗;正在看历史的分支(含当前)给底色高亮。
                if current {
                    row = row.font_weight(FontWeight::BOLD);
                }
                if viewing {
                    row = row.bg(cx.theme().list_active);
                }
                let to_select = name.clone();
                let cb = ctx_branch.clone();
                let menu_branch = name.clone();
                row
                    // 左键:只看该分支提交历史,不切换
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut GitPanel, _, _, cx| {
                            this.select_branch(to_select.clone(), cx)
                        }),
                    )
                    // 长分支名单行截断(末尾溢出隐藏),不再换行成多行
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(name),
                    )
                    .when(b.ahead > 0 || b.behind > 0, |s| {
                        s.child(
                            div()
                                .text_size(px(10.))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("↑{} ↓{}", b.ahead, b.behind)),
                        )
                    })
                    // 右键菜单:checkout 切换到该分支(元素级 on_mouse_down(Right) 本机不触发,
                    // 改用 window 级的 .context_menu)。闭包先把该行分支名写进 ctx_branch,菜单的
                    // CheckoutBranch action 经 Workbench 的 on_action 读它去切换。
                    .context_menu(move |menu, _window, _cx| {
                        *cb.borrow_mut() = Some(menu_branch.to_string());
                        menu.menu("checkout 切换分支", Box::new(crate::CheckoutBranch))
                    })
                    .into_any_element()
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
        let render_group = |title: SharedString,
                            total: usize,
                            changes: Vec<GitChange>,
                            cx: &mut Context<Self>|
         -> Vec<AnyElement> {
            if total == 0 {
                return Vec::new();
            }
            let hidden = total.saturating_sub(changes.len());
            let mut rows = vec![h_flex()
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .rounded(cx.theme().radius)
                .bg(cx.theme().list_active)
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title)
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} files", total)),
                )
                .into_any_element()];
            rows.extend(changes.into_iter().enumerate().map(|(ix, change)| {
                // 绝对路径 = 所属仓 join 仓内相对路径(多仓);conflicts/selected_change 均按绝对路径比对。
                let abs = std::path::Path::new(&change.repo).join(&change.path);
                let abs_str = abs.to_string_lossy().to_string();
                let rel = change.path.clone();
                let repo = change.repo.clone();
                let sel = abs_str.clone();
                // 两段式显示(对齐 IDEA):文件名(前景亮) + 相对目录(muted 灰)
                let p = std::path::Path::new(&change.path);
                let filename: SharedString = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| change.path.clone())
                    .into();
                let dir: SharedString = p
                    .parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default()
                    .into();
                let conflicted = self.conflicts.iter().any(|c| c == &abs_str);
                let selected = self.selected_change.as_deref() == Some(abs_str.as_str());
                let color = if conflicted {
                    cx.theme().danger
                } else {
                    Self::status_color(&change.status, cx)
                };
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
                    .when(selected, |s| s.bg(cx.theme().list_active))
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_change = Some(sel.clone());
                            if conflicted {
                                cx.emit(GitPanelEvent::OpenMerge {
                                    repo: repo.clone(),
                                    rel_path: rel.clone(),
                                });
                            } else {
                                cx.emit(GitPanelEvent::OpenDiff {
                                    repo: repo.clone(),
                                    rel_path: rel.clone(),
                                    abs_path: abs.clone(),
                                });
                            }
                            let _ = this;
                            cx.notify();
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
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .gap_1p5()
                            .text_size(px(12.))
                            .child(div().flex_none().child(filename))
                            .when(!dir.is_empty(), |s| {
                                s.child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(dir),
                                )
                            }),
                    )
                    .when(conflicted, |s| {
                        s.child(
                            div()
                                .text_size(px(10.))
                                .text_color(cx.theme().danger)
                                .child("冲突"),
                        )
                    })
                    .when(change.staged && !conflicted, |s| {
                        s.child(
                            div()
                                .text_size(px(10.))
                                .text_color(cx.theme().muted_foreground)
                                .child("staged"),
                        )
                    })
                    .into_any_element()
            }));
            if hidden > 0 {
                rows.push(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(11.))
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("还有 {} 个变更未展示,请用搜索/忽略规则收窄工作区", hidden))
                        .into_any_element(),
                );
            }
            rows
        };
        // 多仓:按仓分组聚合——每个有改动的仓一个分组(标题=仓名,内含该仓全部改动)。
        // 单仓时就一个分组。MAX_RENDERED_CHANGES 跨所有仓共享上限。
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut remaining = MAX_RENDERED_CHANGES;
        for repo in &self.repos {
            let total = self.changes.iter().filter(|c| c.repo == repo.path).count();
            if total == 0 {
                continue;
            }
            let group: Vec<GitChange> = self
                .changes
                .iter()
                .filter(|c| c.repo == repo.path)
                .take(remaining)
                .cloned()
                .collect();
            remaining = remaining.saturating_sub(group.len());
            rows.extend(render_group(repo.name.clone().into(), total, group, cx));
        }
        let can_rollback = !self.busy && self.selected_change().is_some();
        let busy = self.busy;

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
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child("分支"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(12.))
                            .child(self.branch.clone()),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .child(
                                div()
                                    .rounded(cx.theme().radius)
                                    .when(!busy, |w| w.hover(|s| s.bg(cx.theme().list_active)))
                                    .child(
                                        Button::new("pull")
                                            .ghost()
                                            .xsmall()
                                            .label("Pull")
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.sync_remote(false, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .rounded(cx.theme().radius)
                                    .when(!busy, |w| w.hover(|s| s.bg(cx.theme().list_active)))
                                    .child(
                                        Button::new("push")
                                            .ghost()
                                            .xsmall()
                                            .label("Push")
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.sync_remote(true, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .rounded(cx.theme().radius)
                                    .hover(|s| s.bg(cx.theme().list_active))
                                    .child(
                                        Button::new("refresh")
                                            .ghost()
                                            .xsmall()
                                            .label("刷新")
                                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                                    ),
                            ),
                    ),
            )
            // 多仓选择器(仅 repos>1 时):选中仓决定「分支/历史/Pull/Push/checkout」针对哪个仓;
            // Commit 变更视图聚合所有仓,不依赖它。横向可滚动,容纳多仓。
            .when(self.repos.len() > 1, |panel| {
                let active_repo = self.active_repo;
                panel.child(
                    h_flex()
                        .id("repo-selector")
                        .px_2()
                        .py_1()
                        .gap_1p5()
                        .items_center()
                        .overflow_x_scroll()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .children(self.repos.iter().enumerate().map(|(i, r)| {
                            let active = i == active_repo;
                            div()
                                .id(("repo-chip", i))
                                .px_2()
                                .py_0p5()
                                .flex_none()
                                .rounded(cx.theme().radius)
                                .text_size(px(11.))
                                .whitespace_nowrap()
                                .when(active, |s| {
                                    s.bg(cx.theme().list_active).text_color(cx.theme().foreground)
                                })
                                .when(!active, |s| {
                                    s.text_color(cx.theme().muted_foreground)
                                        .hover(|s| s.bg(cx.theme().accent))
                                })
                                .child(r.name.clone())
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut GitPanel, _, _, cx| {
                                        this.select_repo(i, cx);
                                    }),
                                )
                        })),
                )
            })
            .when(self.mode == GitPanelMode::Commit, |panel| {
                panel.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .rounded(cx.theme().radius)
                                .when(can_rollback, |w| w.hover(|s| s.bg(cx.theme().list_active)))
                                .child(
                                    Button::new("rollback")
                                        .ghost()
                                        .xsmall()
                                        .label("Rollback")
                                        .disabled(!can_rollback)
                                        .on_click(cx.listener(|this, _, _, cx| this.rollback_selected(cx))),
                                ),
                        )
                        .when(self.selected_change.is_some(), |s| {
                            s.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(self.selected_change.clone().unwrap_or_default()),
                            )
                        }),
                )
            })
            .when(self.mode == GitPanelMode::Branches, |panel| {
                panel.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(div().flex_1().min_w_0().child(Input::new(&self.branch_filter))),
                )
            })
            .child(
                v_flex()
                    .id("git-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .map(|body| match self.mode {
                        GitPanelMode::Commit => body
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
                        GitPanelMode::Branches => {
                            let branch_rows = self.render_branches(cx);
                            body
                                .when(branch_rows.is_empty(), |s| {
                                    s.child(
                                        div()
                                            .p_2()
                                            .text_size(px(12.))
                                            .text_color(cx.theme().muted_foreground)
                                            .child("没有匹配的分支"),
                                    )
                                })
                                .children(branch_rows)
                                .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .mt_1()
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "历史 · {}",
                                        if self.selected_branch.is_empty() {
                                            self.branch.clone()
                                        } else {
                                            self.selected_branch.clone()
                                        }
                                    )),
                            )
                                .children(self.render_log(cx))
                        }
                    }),
            )
            .when(self.mode == GitPanelMode::Commit, |panel| panel.child(
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
