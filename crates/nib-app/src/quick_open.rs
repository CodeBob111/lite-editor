// Quick Open 面板(对齐旧版 双击Shift/quick-open):模糊搜全项目文件,
// nucleo 匹配(Helix 同款,30 万条目无感延迟),↑↓ 选择,Enter 打开,Esc 关闭。
// 文件清单由 Workbench 预载缓存(list_all_files 在 core runtime 跑),
// 每击键只做内存匹配——不触盘,主线程开销毫秒级。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme, Icon, IconName, Sizable as _,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

const MAX_RESULTS: usize = 60;

/// 绝对路径 → 项目相对路径(去根前缀 + 前导 /)。预计算一次,不在每次击键里重复生成。
fn rel_of(root: &str, abs: &str) -> String {
    abs.strip_prefix(root)
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| abs.to_string())
}

/// 模糊匹配:对预计算的相对路径打分,Top-K(部分选择 + 只排前 MAX_RESULTS,不全排序)返回
/// 命中的下标。纯计算、无 IO/无 self,可丢到后台线程跑。空查询 = 按路径长度取最浅的一批。
fn match_files(query: &str, rels: &[String]) -> Vec<usize> {
    if query.trim().is_empty() {
        let mut ixs: Vec<usize> = (0..rels.len()).collect();
        if ixs.len() > MAX_RESULTS {
            ixs.select_nth_unstable_by_key(MAX_RESULTS - 1, |&i| rels[i].len());
            ixs.truncate(MAX_RESULTS);
        }
        ixs.sort_by_key(|&i| rels[i].len());
        return ixs;
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut scored: Vec<(u32, usize)> = Vec::new();
    let mut buf = Vec::new();
    for (ix, rel) in rels.iter().enumerate() {
        let haystack = nucleo_matcher::Utf32Str::new(rel, &mut buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            scored.push((score, ix));
        }
    }
    if scored.len() > MAX_RESULTS {
        scored.select_nth_unstable_by(MAX_RESULTS - 1, |a, b| b.0.cmp(&a.0));
        scored.truncate(MAX_RESULTS);
    }
    scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(_, ix)| ix).collect()
}

pub enum QuickOpenEvent {
    Open(PathBuf),
}

pub struct QuickOpen {
    input: Entity<InputState>,
    /// 绝对路径全量清单(Workbench 预载)
    files: Arc<Vec<String>>,
    /// 与 files 等长、一一对应的相对路径,**开面板时预计算一次**——避免每次击键对全部文件
    /// 重复生成相对路径 String(原实现的逐键堆分配)。
    rels: Arc<Vec<String>>,
    /// 命中的 files 下标,按 nucleo 得分降序
    matches: Vec<usize>,
    selected: usize,
    /// 输入防抖序号:连续击键只让最后一次过滤生效。
    filter_seq: u64,
    _subscription: Subscription,
}

impl EventEmitter<QuickOpenEvent> for QuickOpen {}

impl QuickOpen {
    pub fn new(
        project_root: PathBuf,
        files: Arc<Vec<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("输入文件名(模糊匹配)…"));
        let subscription = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            match event {
                InputEvent::Change => this.schedule_refilter(cx),
                InputEvent::PressEnter { .. } => this.confirm(cx),
                _ => {}
            }
        });

        let root = project_root.to_string_lossy();
        let rels: Vec<String> = files.iter().map(|p| rel_of(&root, p)).collect();
        drop(root);

        let rels = Arc::new(rels);
        // 初始(空查询)立即出一批,无需上后台/防抖(空查询只按长度选,廉价)
        let matches = match_files("", &rels);
        Self {
            input,
            files,
            rels,
            matches,
            selected: 0,
            filter_seq: 0,
            _subscription: subscription,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// 输入防抖 + 后台匹配:连续击键 +seq,40ms 静默后读 query,把模糊匹配放到**后台执行器线程**
    /// 跑(大项目逐键全量匹配很贵,绝不占主线程),结果回主线程更新;两端都按 seq 丢弃过期结果。
    fn schedule_refilter(&mut self, cx: &mut Context<Self>) {
        self.filter_seq += 1;
        let seq = self.filter_seq;
        let rels = self.rels.clone();
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(40))
                .await;
            // 读最新 query(主线程)+ 校验防抖序号
            let query = weak
                .update(cx, |this, cx| {
                    if this.filter_seq != seq {
                        None
                    } else {
                        Some(this.input.read(cx).value().to_string())
                    }
                })
                .ok()
                .flatten();
            let Some(query) = query else {
                return;
            };
            // 后台线程做匹配
            let matches = cx
                .background_executor()
                .spawn(async move { match_files(&query, &rels) })
                .await;
            let _ = weak.update(cx, |this, cx| {
                if this.filter_seq != seq {
                    return; // 期间又敲了 → 丢弃这次结果
                }
                this.matches = matches;
                this.selected = 0;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as i32;
        let next = (self.selected as i32 + delta).rem_euclid(len);
        self.selected = next as usize;
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(&ix) = self.matches.get(self.selected) {
            cx.emit(QuickOpenEvent::Open(PathBuf::from(self.files[ix].clone())));
        }
    }

    fn select_and_open(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        self.confirm(cx);
    }
}

impl Render for QuickOpen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .matches
            .iter()
            .enumerate()
            .map(|(row, &file_ix)| {
                let rel = self.rels[file_ix].clone();
                let (dir, name) = match rel.rsplit_once('/') {
                    Some((d, n)) => (Some(d.to_string()), n.to_string()),
                    None => (None, rel.clone()),
                };
                let selected = row == self.selected;
                h_flex()
                    .id(row)
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .when(selected, |s| s.bg(cx.theme().list_active))
                    .hover(|s| s.bg(cx.theme().accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.select_and_open(row, cx)),
                    )
                    .child(Icon::new(IconName::File).small())
                    .child(div().child(name))
                    .when_some(dir, |s, dir| {
                        s.child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .text_size(px(11.))
                                .child(dir),
                        )
                    })
            })
            .collect();

        v_flex()
            .w(px(560.))
            .max_h(px(420.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .child(div().p_2().child(Input::new(&self.input)))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .px_1()
                    .pb_1()
                    .children(rows),
            )
    }
}
