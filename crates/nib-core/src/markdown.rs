// Markdown 渲染(自 commands.rs 迁入;comrak,GFM 扩展对齐原 marked gfm:true;不开硬换行)

use crate::rt::on_worker;

pub fn markdown_to_html(text: &str) -> String {
    let mut options = comrak::Options::default();
    options.extension.autolink = true;
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    // 沿用 marked 的原始 HTML 透传现状(本地文件预览,信任模型不变)
    options.render.r#unsafe = true;
    comrak::markdown_to_html(text, &options)
}

pub async fn render_markdown(text: String) -> Result<String, String> {
    on_worker(move || Ok(markdown_to_html(&text))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_render_matches_marked_gfm_behavior() {
        // fenced code:语言 class + HTML 转义(对齐原 marked 自定义 renderer)
        let html = markdown_to_html("```rust\nlet a = 1 < 2;\n```\n");
        assert!(html.contains("<pre><code class=\"language-rust\">"), "html: {}", html);
        assert!(html.contains("1 &lt; 2"));
        // GFM autolink:裸 URL 可点
        let html = markdown_to_html("see https://example.com/x\n");
        assert!(html.contains("<a href=\"https://example.com/x\""), "html: {}", html);
        // GFM 表格
        let html = markdown_to_html("| a | b |\n| - | - |\n| 1 | 2 |\n");
        assert!(html.contains("<table>"));
        // 原始 HTML 透传(marked 默认不消毒,行为保持)
        let html = markdown_to_html("<div class=\"x\">hi</div>\n");
        assert!(html.contains("<div class=\"x\">hi</div>"));
        // 软换行不转 <br>(对齐 breaks:false)
        let html = markdown_to_html("line1\nline2\n");
        assert!(!html.contains("<br"));
    }
}
