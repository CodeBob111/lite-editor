// Nib 原生壳(M0b 起步):gpui 窗口 + gpui-component Root。
// 性能纪律(RFC v2 §5):主线程零阻塞 IO——一切重活经 nib-core 自持 runtime。

use gpui::*;
use gpui_component::Root;

struct Workbench;

impl Render for Workbench {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child("Nib (native) — M0b 骨架启动")
    }
}

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| Workbench);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
