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
    matcher: Matcher,
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

        let mut this = Self {
            input,
            files,
            rels: Arc::new(rels),
            matches: Vec::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            filter_seq: 0,
            _subscription: subscription,
        };
        this.refilter(cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// 输入防抖:连续击键 +seq,~40ms 静默后只有 seq 未变才真正过滤(大项目逐键全量匹配很贵)。
    fn schedule_refilter(&mut self, cx: &mut Context<Self>) {
        self.filter_seq += 1;
        let seq = self.filter_seq;
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(40))
                .await;
            let _ = weak.update(cx, |this, cx| {
                if this.filter_seq == seq {
                    this.refilter(cx);
                }
            });
        })
        .detach();
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        if query.trim().is_empty() {
            // 空查询:取最浅的一批文件。用 select_nth 部分选择 + 只排前 K,不全排序所有文件。
            let rels = self.rels.clone();
            let mut ixs: Vec<usize> = (0..self.files.len()).collect();
            if ixs.len() > MAX_RESULTS {
                ixs.select_nth_unstable_by_key(MAX_RESULTS - 1, |&i| rels[i].len());
                ixs.truncate(MAX_RESULTS);
            }
            ixs.sort_by_key(|&i| rels[i].len());
            self.matches = ixs;
        } else {
            let pattern = Pattern::parse(&query, CaseMatching::Smart, Normalization::Smart);
            let mut scored: Vec<(u32, usize)> = Vec::new();
            let mut buf = Vec::new();
            // 用预计算的 rels,不在循环里重复生成相对路径
            for (ix, rel) in self.rels.iter().enumerate() {
                let haystack = nucleo_matcher::Utf32Str::new(rel, &mut buf);
                if let Some(score) = pattern.score(haystack, &mut self.matcher) {
                    scored.push((score, ix));
                }
            }
            // Top-K:部分选择出得分最高的 MAX_RESULTS 个,只排这 K 个,不对全部命中全排序
            if scored.len() > MAX_RESULTS {
                scored.select_nth_unstable_by(MAX_RESULTS - 1, |a, b| b.0.cmp(&a.0));
                scored.truncate(MAX_RESULTS);
            }
            scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
            self.matches = scored.into_iter().map(|(_, ix)| ix).collect();
        }
        self.selected = 0;
        cx.notify();
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
