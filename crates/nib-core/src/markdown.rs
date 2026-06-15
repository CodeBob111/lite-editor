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

/// 自定义 URL scheme:预览里点相对文件链接时,把它改写成 `nibfile://<abs>`,`cx.open_url`
/// 经 OS 路由回 Nib(bundle 注册了该 scheme),app 的 on_open_urls 在新标签打开。区别于直接
/// 用 file://(那会丢给系统默认 app)。详见 main.rs 的 url 处理。
pub const NIBFILE_SCHEME: &str = "nibfile://";

/// 把 markdown 里的**相对文件链接**改写成 `nibfile://<绝对路径>`(用 `base_dir`=当前 md 所在
/// 目录解析),好让预览里点击在 Nib 标签打开。http(s)/mailto/协议相对(`//`)/锚点(`#…`)/
/// 已带 scheme 的都不动。纯词法(不碰文件系统,可在渲染路径同步调用)。
pub fn resolve_relative_links(text: &str, base_dir: &std::path::Path) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    // markdown 行内链接 url 段:`](url)` 或 `](url "title")`,url 取到首个空白/`)` 为止。
    let re = RE.get_or_init(|| regex::Regex::new(r"\]\(\s*([^)\s]+)([^)]*)\)").unwrap());
    re.replace_all(text, |caps: &regex::Captures| {
        let url = &caps[1];
        let rest = &caps[2];
        match rewrite_relative_link(url, base_dir) {
            Some(rewritten) => format!("]({}{})", rewritten, rest),
            None => caps[0].to_string(),
        }
    })
    .into_owned()
}

fn rewrite_relative_link(url: &str, base_dir: &std::path::Path) -> Option<String> {
    if url.starts_with('#')
        || url.starts_with("//")
        || url.starts_with("mailto:")
        || url.contains("://")
    {
        return None;
    }
    // 路径段与可选 #fragment 分开(锚点暂不支持滚动,但路径仍要能打开,故剥掉 fragment)
    let path_part = url.split('#').next().unwrap_or(url);
    if path_part.is_empty() {
        return None;
    }
    let abs = base_dir.join(path_part);
    // percent-encode(空格/中文/特殊字符),保留 `/`;复用 lsp 的路径字符集。
    let abs_str = abs.to_string_lossy();
    let encoded = percent_encoding::utf8_percent_encode(&abs_str, crate::lsp::uri_path_set());
    Some(format!("{NIBFILE_SCHEME}{encoded}"))
}

/// 从 `nibfile://<abs>` URL 还原绝对路径(供 main.rs 的 on_open_urls 用)。非本 scheme 返回 None。
pub fn path_from_nibfile_url(url: &str) -> Option<std::path::PathBuf> {
    let raw = url.strip_prefix(NIBFILE_SCHEME)?;
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    Some(std::path::PathBuf::from(decoded.into_owned()))
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

    #[test]
    fn resolve_links_rewrites_only_relative_files() {
        use std::path::Path;
        let base = Path::new("/repo/docs");
        let md = "[a](sub/x.md) [b](https://e.com) [c](#sec) [d](../y.md \"t\") [e](mailto:a@b.c)";
        let out = resolve_relative_links(md, base);
        // 相对文件 → nibfile://;锚点 fragment 剥掉(暂不支持滚动,但路径要能开)
        assert!(out.contains("[a](nibfile:///repo/docs/sub/x.md)"), "{out}");
        assert!(out.contains("[d](nibfile:///repo/docs/../y.md \"t\")"), "{out}");
        // 外链/锚点/mailto 不动
        assert!(out.contains("[b](https://e.com)"), "{out}");
        assert!(out.contains("[c](#sec)"), "{out}");
        assert!(out.contains("[e](mailto:a@b.c)"), "{out}");
    }

    #[test]
    fn nibfile_url_roundtrips_with_spaces_and_unicode() {
        use std::path::Path;
        let base = Path::new("/repo");
        let out = resolve_relative_links("[x](子 目录/说明.md)", base);
        // 空格/中文被 percent-encode
        let url = out
            .trim_start_matches("[x](")
            .trim_end_matches(')')
            .to_string();
        assert!(url.starts_with("nibfile://"), "{url}");
        let back = path_from_nibfile_url(&url).unwrap();
        assert_eq!(back, Path::new("/repo/子 目录/说明.md"));
    }
}
