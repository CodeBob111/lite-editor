// 设置页(对齐 settings.html):从旧 4 字段浮层扩成 3 栏完整页——
// 左导航 212(编辑器=真分类 + 提案分类标灰)| 中内容(4 真字段:slider/分段/switch)| 右预览 392。
// 4 个真字段(fontSize/tabSize/wordWrap/folding)实时生效(每次改动即 emit Apply,父级热应用)。
// 提案分类(外观/Java/终端/Git/Arthas/Astore)无后端 → 标「提案」徽标、不可选,守 handoff 约束。

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    slider::{Slider, SliderEvent, SliderState},
    switch::Switch,
    v_flex, ActiveTheme,
};

use crate::session::EditorSettings;

pub enum SettingsEvent {
    Apply(EditorSettings),
}

/// 提案分类(无后端;仅展示,不可选)
const PROPOSAL_CATS: [&str; 5] = ["外观", "终端", "Git", "Arthas", "Astore"];

#[derive(Clone, Copy, PartialEq)]
enum Cat {
    Editor,
    Maven,
}

pub struct SettingsView {
    cat: Cat,
    font_slider: Entity<SliderState>,
    tab_size: u32,
    word_wrap: bool,
    folding: bool,
    maven_home: Entity<InputState>,
    maven_settings: Entity<InputState>,
    maven_repo: Entity<InputState>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(settings: EditorSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let font_slider = cx.new(|_| {
            SliderState::new()
                .min(11.)
                .max(22.)
                .step(1.)
                .default_value(settings.font_size)
        });
        cx.subscribe(&font_slider, |this, _, _event: &SliderEvent, cx| {
            this.apply(cx);
            cx.notify();
        })
        .detach();

        // Maven 三个路径文本输入,改即实时生效(订阅 Change)
        let mk = |val: &str, window: &mut Window, cx: &mut Context<Self>| {
            let st = cx.new(|cx| InputState::new(window, cx).default_value(val));
            cx.subscribe(&st, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.apply(cx);
                }
            })
            .detach();
            st
        };
        let maven_home = mk(&settings.maven_home, window, cx);
        let maven_settings = mk(&settings.maven_settings, window, cx);
        let maven_repo = mk(&settings.maven_repo, window, cx);

        Self {
            cat: Cat::Editor,
            font_slider,
            tab_size: settings.tab_size,
            word_wrap: settings.word_wrap,
            folding: settings.folding,
            maven_home,
            maven_settings,
            maven_repo,
        }
    }

    /// 切到 Maven 分类(供「检测到 Maven 工程」提醒的「去设置」按钮直达)。
    pub fn show_maven(&mut self, cx: &mut Context<Self>) {
        self.cat = Cat::Maven;
        cx.notify();
    }

    /// 全字段打包 emit,父级(Workbench)订阅后热应用 + 持久化。
    fn apply(&self, cx: &mut Context<Self>) {
        let font_size = self.font_slider.read(cx).value().start().round();
        cx.emit(SettingsEvent::Apply(EditorSettings {
            font_size,
            tab_size: self.tab_size,
            word_wrap: self.word_wrap,
            folding: self.folding,
            maven_home: self.maven_home.read(cx).value().to_string(),
            maven_settings: self.maven_settings.read(cx).value().to_string(),
            maven_repo: self.maven_repo.read(cx).value().to_string(),
        }));
    }
}

/// 「已实现 / 提案」徽标
fn badge(text: &'static str, color: Hsla, border: Hsla) -> impl IntoElement {
    div()
        .text_size(px(10.))
        .text_color(color)
        .border_1()
        .border_color(border)
        .rounded(px(5.))
        .px(px(7.))
        .py(px(2.))
        .child(text)
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let sidebar = cx.theme().sidebar;
        let bg = cx.theme().background;
        let success = cx.theme().success;
        let mono = cx.theme().mono_font_family.clone();
        let font_size = self.font_slider.read(cx).value().start().round();
        let entity = cx.entity();
        let cur_cat = self.cat;
        let list_active = cx.theme().list_active;
        let accent = cx.theme().accent;

        // 真分类导航项(可点切换;选中=高亮底+左条)
        let cat_item = |label: &'static str, c: Cat| {
            let on = cur_cat == c;
            let e = entity.clone();
            h_flex()
                .relative()
                .mx(px(8.))
                .px(px(9.))
                .py(px(7.))
                .gap(px(9.))
                .items_center()
                .rounded(px(7.))
                .cursor_pointer()
                .when(on, |s| s.bg(list_active).text_color(fg))
                .when(!on, |s| s.text_color(muted).hover(|h| h.bg(accent)))
                .when(on, |s| {
                    s.child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .top(px(6.))
                            .bottom(px(6.))
                            .w(px(2.))
                            .rounded(px(2.))
                            .bg(primary),
                    )
                })
                .on_mouse_down(MouseButton::Left, move |_, _, app| {
                    e.update(app, |this, cx| {
                        this.cat = c;
                        cx.notify();
                    });
                })
                .child(div().text_size(px(13.)).child(label))
        };

        // —— 左导航 ——
        let nav = v_flex()
            .w(px(212.))
            .h_full()
            .flex_none()
            .bg(sidebar)
            .border_r_1()
            .border_color(border)
            .child(
                div()
                    .px(px(12.))
                    .pt(px(14.))
                    .pb(px(5.))
                    .text_size(px(10.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(muted)
                    .child("常规"),
            )
            // 真分类:编辑器 / Maven(可切换)
            .child(cat_item("编辑器", Cat::Editor))
            .child(cat_item("Maven", Cat::Maven))
            .child(
                div()
                    .px(px(12.))
                    .pt(px(14.))
                    .pb(px(5.))
                    .text_size(px(10.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(muted)
                    .child("提案"),
            )
            // 提案分类:灰 + 「提案」徽标,不可选
            .children(PROPOSAL_CATS.iter().map(|name| {
                h_flex()
                    .mx(px(8.))
                    .px(px(9.))
                    .py(px(7.))
                    .gap(px(9.))
                    .items_center()
                    .rounded(px(7.))
                    .text_color(muted)
                    .child(div().flex_1().text_size(px(13.)).child(*name))
                    .child(badge("提案", muted, border))
            }));

        // —— 字段行 helper(name/key/desc | 控件 + 徽标) ——
        let field = |name: &'static str, key: &'static str, desc: &'static str, control: AnyElement| {
            h_flex()
                .py(px(18.))
                .gap(px(30.))
                .items_start()
                .border_b_1()
                .border_color(border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .max_w(px(430.))
                        .child(
                            h_flex()
                                .gap(px(9.))
                                .items_center()
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(fg)
                                        .child(name),
                                )
                                .child(badge("已实现", success, border)),
                        )
                        .child(
                            div()
                                .mt(px(4.))
                                .font_family(mono.clone())
                                .text_size(px(11.))
                                .text_color(muted)
                                .child(key),
                        )
                        .child(
                            div()
                                .mt(px(7.))
                                .text_size(px(12.5))
                                .text_color(muted)
                                .child(desc),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .min_w(px(200.))
                        .justify_end()
                        .items_center()
                        .gap(px(12.))
                        .child(control),
                )
        };

        // 字号:slider + 数值
        let font_ctrl = h_flex()
            .gap(px(12.))
            .items_center()
            .child(div().w(px(150.)).child(Slider::new(&self.font_slider)))
            .child(
                div()
                    .font_family(mono.clone())
                    .text_size(px(13.))
                    .text_color(fg)
                    .border_1()
                    .border_color(border)
                    .rounded(px(6.))
                    .px(px(8.))
                    .py(px(3.))
                    .min_w(px(46.))
                    .text_right()
                    .child(format!("{}", font_size as i32)),
            )
            .into_any_element();

        // Tab 宽度:分段 2/4/8
        let seg = |val: u32| {
            let on = self.tab_size == val;
            let e = entity.clone();
            div()
                .id(("tab", val as usize))
                .px(px(11.))
                .py(px(5.))
                .rounded(px(5.))
                .cursor_pointer()
                .font_family(mono.clone())
                .text_size(px(12.))
                .when(on, |s| s.bg(primary).text_color(cx.theme().primary_foreground))
                .when(!on, |s| s.text_color(muted))
                .on_mouse_down(
                    MouseButton::Left,
                    move |_, window, app| {
                        e.update(app, |this, cx| {
                            this.tab_size = val;
                            this.apply(cx);
                            cx.notify();
                        });
                        let _ = window;
                    },
                )
                .child(format!("{val}"))
        };
        let tab_ctrl = h_flex()
            .bg(bg)
            .border_1()
            .border_color(border)
            .rounded(px(7.))
            .p(px(2.))
            .gap(px(2.))
            .child(seg(2))
            .child(seg(4))
            .child(seg(8))
            .into_any_element();

        // 开关 helper
        let switch_ctrl = |id: &'static str, value: bool, set: fn(&mut SettingsView, bool)| {
            let e = entity.clone();
            Switch::new(id)
                .checked(value)
                .on_click(move |checked, _window, app| {
                    let v = *checked;
                    e.update(app, |this, cx| {
                        set(this, v);
                        this.apply(cx);
                        cx.notify();
                    });
                })
                .into_any_element()
        };

        // 内容区标题
        let header = |title: &'static str, desc: &'static str| {
            v_flex()
                .mb(px(10.))
                .child(
                    div()
                        .text_size(px(21.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(fg)
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(5.))
                        .max_w(px(540.))
                        .text_size(px(13.))
                        .text_color(muted)
                        .child(desc),
                )
        };
        // Maven 路径字段:路径需要整行宽度,纵向堆叠(标题/key/说明/全宽输入)。
        // 不用编辑器那种「左说明+右窄控件」横排——内容列仅 ~288px,塞不下宽输入框,
        // 会把左说明列压成 0 宽 → 说明文字每字一行的竖排乱码。
        let mfield = |name: &'static str, key: &'static str, desc: &'static str, st: &Entity<InputState>| {
            v_flex()
                .py(px(16.))
                .gap(px(6.))
                .border_b_1()
                .border_color(border)
                .child(
                    h_flex()
                        .gap(px(9.))
                        .items_center()
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(fg)
                                .child(name),
                        )
                        .child(badge("已实现", success, border)),
                )
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_size(px(11.))
                        .text_color(muted)
                        .child(key),
                )
                .child(div().text_size(px(12.5)).text_color(muted).child(desc))
                .child(
                    div()
                        .mt(px(4.))
                        .w_full()
                        .child(Input::new(st).font_family(mono.clone()).text_size(px(12.5))),
                )
        };
        let content = v_flex()
            .id("settings-content")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px(px(34.))
            .py(px(26.))
            .map(|c| match self.cat {
                Cat::Editor => c
                    .child(header(
                        "编辑器",
                        "字体、缩进与换行。改动立即作用于已打开的编辑器(Tab 宽度对新开标签生效)。",
                    ))
                    .child(field("字号", "editor.fontSize", "编辑器正文字号(11–22)。", font_ctrl))
                    .child(field("Tab 宽度", "editor.tabSize", "一个缩进等于多少空格。", tab_ctrl))
                    .child(field(
                        "自动换行",
                        "editor.wordWrap",
                        "超出视宽的长行是否折行显示。",
                        switch_ctrl("set-wrap", self.word_wrap, |s, v| s.word_wrap = v),
                    ))
                    .child(field(
                        "代码折叠",
                        "editor.folding",
                        "显示折叠槽,可折叠函数/块。",
                        switch_ctrl("set-folding", self.folding, |s, v| s.folding = v),
                    )),
                Cat::Maven => c
                    .child(header(
                        "Maven",
                        "像 IDEA 一样指定 Maven home / settings.xml / 本地仓库。留空则用 PATH 里的 mvn 与默认 ~/.m2。改动即时生效,Maven 面板会按新配置重新解析。",
                    ))
                    .child(mfield(
                        "Maven home",
                        "maven.home",
                        "自定义 Maven 目录(含 bin/mvn),如 ~/amaven-3.5.0。留空=用 PATH 的 mvn。",
                        &self.maven_home,
                    ))
                    .child(mfield(
                        "settings.xml",
                        "maven.settings",
                        "内网仓库等配置文件路径。留空=mvn 默认 ~/.m2/settings.xml。",
                        &self.maven_settings,
                    ))
                    .child(mfield(
                        "本地仓库",
                        "maven.repo",
                        "依赖本地缓存目录。留空=mvn 默认 ~/.m2/repository。",
                        &self.maven_repo,
                    )),
            });

        // —— 右侧实时预览 ——
        let sample: [(&str, Hsla); 6] = [
            ("public class Demo {", cx.theme().foreground),
            ("    // 实时预览:字号/缩进/换行随设置变化", cx.theme().muted_foreground),
            ("    private final String name = \"nib\";", fg),
            ("    public String greet(String who) {", fg),
            ("        return \"Hello, \" + who + \" from \" + name;", fg),
            ("    }", fg),
        ];
        let indent = " ".repeat(self.tab_size as usize);
        let preview = v_flex()
            .w(px(392.))
            .flex_none()
            .h_full()
            .bg(bg)
            .border_l_1()
            .border_color(border)
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_none()
                    .px(px(14.))
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .text_size(px(11.))
                    .text_color(muted)
                    .child("实时预览"),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p(px(14.))
                    .font_family(mono.clone())
                    .text_size(px(font_size))
                    .children(sample.iter().map(|(line, color)| {
                        // tabSize:把样例里的 4 空格缩进按当前 tab_size 重排
                        let shown = line.replace("    ", &indent);
                        div()
                            .when(!self.word_wrap, |d| d.whitespace_nowrap().overflow_hidden())
                            .text_color(*color)
                            .child(shown)
                    })),
            );

        // —— 外壳:标题条 + 三栏 ——
        v_flex()
            .w(px(960.))
            .h(px(620.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(38.))
                    .flex_none()
                    .px(px(14.))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(border)
                    .bg(cx.theme().title_bar)
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child("设置"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child("Esc 关闭 · 改动实时生效"),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(nav)
                    .child(content)
                    .child(preview),
            )
    }
}
