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

/// 文档内锚点导航 URL:`nibfile://nib-anchor/<encoded-slug>`。预览里点 `#章节` 锚点经 OS 路由
/// 回 app(on_open_urls → scroll_md_to_anchor 滚到该章节)。host 段 `nib-anchor` 与文件链接
/// (`nibfile:///abs`,host 空)区分开。
pub const ANCHOR_URL_PREFIX: &str = "nibfile://nib-anchor/";

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
    // 文档内锚点(#章节)→ 改写成路由回 Nib 的锚点 URL,点击经 OS 往返后滚到对应章节。
    // 不这么做的话裸 `#slug` 会被丢给 cx.open_url,OS 无法路由报 -50。
    if let Some(slug) = url.strip_prefix('#') {
        if slug.is_empty() {
            return None;
        }
        let encoded = percent_encoding::utf8_percent_encode(slug, crate::lsp::uri_path_set());
        return Some(format!("{ANCHOR_URL_PREFIX}{encoded}"));
    }
    // 协议相对(//host/path):OS 直接 open 同样会 -50,补成 https 让它能开。
    if let Some(rest) = url.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    // mailto: / 已带 scheme(http(s) 等):OS 能开,原样不动。
    if url.starts_with("mailto:") || url.contains("://") {
        return None;
    }
    // 路径段与可选 #fragment 分开(剥掉 fragment,路径仍要能打开)
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

/// 从 `nibfile://<abs>` URL 还原绝对路径(供 main.rs 的 on_open_urls 用)。锚点 URL 与非本
/// scheme 返回 None(调用方须先用 `anchor_slug_from_nibfile_url` 判锚点,再判文件)。
pub fn path_from_nibfile_url(url: &str) -> Option<std::path::PathBuf> {
    if url.starts_with(ANCHOR_URL_PREFIX) {
        return None;
    }
    let raw = url.strip_prefix(NIBFILE_SCHEME)?;
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    Some(std::path::PathBuf::from(decoded.into_owned()))
}

/// 从锚点 URL(`nibfile://nib-anchor/<encoded-slug>`)还原原始 slug;非锚点 URL 返回 None。
pub fn anchor_slug_from_nibfile_url(url: &str) -> Option<String> {
    let raw = url.strip_prefix(ANCHOR_URL_PREFIX)?;
    Some(
        percent_encoding::percent_decode_str(raw)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

/// 把锚点 slug / 标题文本归一化成「只留字母数字+CJK、小写、无分隔符」的 key,用来跨
/// GitHub/GitLab/手写 slug 的差异做匹配——它们主要差在空格/连字符/标点的处理,字母数字+CJK
/// 主体一致。例:`内置-claude-code-配置claude` 与标题「内置 Claude Code 配置(.claude/)」都
/// 归一成 `内置claudecode配置claude`。
pub fn normalize_anchor(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// markdown 预览按 ATX 标题切出的一段(标题行 + 其后内容,到下一标题为止)。
pub struct MdSection {
    /// 该段原始 markdown(含标题行)
    pub text: String,
    /// 该段标题归一化后的锚点 key;首个标题前的前导段为 None
    pub anchor_key: Option<String>,
}

/// 把 markdown 按 ATX 标题(`#`..`######` + 空白,围栏代码块外)切成若干段:供预览分段渲染 +
/// 锚点滚动定位。渲染与点击两处调同一函数,保证段序一致(滚动 index 才对得上)。
pub fn split_into_sections(md: &str) -> Vec<MdSection> {
    let mut sections = Vec::new();
    let mut cur = String::new();
    let mut cur_key: Option<String> = None;
    let mut in_fence = false;
    for line in md.split_inclusive('\n') {
        let trimmed = line.trim_start();
        // 围栏代码块开合(``` 或 ~~~):块内的 `# xx` 不是标题,不切段
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            cur.push_str(line);
            continue;
        }
        if !in_fence {
            if let Some(title) = atx_heading_title(line) {
                if !cur.is_empty() || cur_key.is_some() {
                    sections.push(MdSection {
                        text: std::mem::take(&mut cur),
                        anchor_key: cur_key.take(),
                    });
                }
                cur_key = Some(normalize_anchor(title));
            }
        }
        cur.push_str(line);
    }
    if !cur.is_empty() || cur_key.is_some() {
        sections.push(MdSection {
            text: cur,
            anchor_key: cur_key,
        });
    }
    if sections.is_empty() {
        sections.push(MdSection {
            text: String::new(),
            anchor_key: None,
        });
    }
    sections
}

/// 为锚点 slug 找对应章节段的下标。分级匹配,容忍手写锚点与标题的 slug 风格差异:
/// ① 精确(归一化相等);② 标题归一化「以」slug 归一化「为前缀」——兜住作者省掉标题尾部的
/// 括注/代码片段(如标题「…配置(`.claude/`)」归一含尾部 `claude`,而锚点只写「…配置」);
/// ③ 反向(slug 以标题为前缀)。多段命中取分最高、同分取第一个。slug / 标题归一化 <2 字不做
/// 前缀匹配(避免太短乱跳)。str::starts_with 对 &str 天然按字符边界,多字节安全。
pub fn find_section_for_anchor(sections: &[MdSection], slug: &str) -> Option<usize> {
    let key = normalize_anchor(slug);
    if key.is_empty() {
        return None;
    }
    let key_ok = key.chars().count() >= 2;
    let mut best: Option<(u8, usize)> = None;
    for (ix, sec) in sections.iter().enumerate() {
        let Some(hk) = sec.anchor_key.as_deref() else {
            continue;
        };
        let tier = if hk == key {
            3
        } else if key_ok && hk.starts_with(&key) {
            2
        } else if key_ok && hk.chars().count() >= 2 && key.starts_with(hk) {
            1
        } else {
            0
        };
        if tier > 0 && best.is_none_or(|(b, _)| tier > b) {
            best = Some((tier, ix));
        }
    }
    best.map(|(_, ix)| ix)
}

/// 若 `line` 是 ATX 标题(行首 1-6 个 `#` 后跟空白),返回标题文本(去 `#` 与首尾空白)。
fn atx_heading_title(line: &str) -> Option<&str> {
    let s = line.trim_start();
    let hashes = s.len() - s.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) {
        let rest = &s[hashes..];
        if rest.starts_with([' ', '\t']) {
            return Some(rest.trim());
        }
    }
    None
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
        // 相对文件 → nibfile://;路径段的 fragment 剥掉
        assert!(out.contains("[a](nibfile:///repo/docs/sub/x.md)"), "{out}");
        assert!(out.contains("[d](nibfile:///repo/docs/../y.md \"t\")"), "{out}");
        // 外链/mailto 不动;纯锚点 → 路由 URL(改成可跳章节,不再裸 #)
        assert!(out.contains("[b](https://e.com)"), "{out}");
        assert!(out.contains("[c](nibfile://nib-anchor/sec)"), "{out}");
        assert!(out.contains("[e](mailto:a@b.c)"), "{out}");
    }

    #[test]
    fn anchor_and_protocol_relative_links_are_rewritten() {
        use std::path::Path;
        let base = Path::new("/repo");
        let md = "[a](#内置-claude-code-配置claude) [b](//host.com/x) [c](sub/y.md) [d](https://e.com)";
        let out = resolve_relative_links(md, base);
        // 锚点 → 路由 URL
        assert!(out.contains("[a](nibfile://nib-anchor/"), "{out}");
        // 协议相对 → https(否则 OS open 报 -50)
        assert!(out.contains("[b](https://host.com/x)"), "{out}");
        // 相对文件 → nibfile:///abs;外链不动
        assert!(out.contains("[c](nibfile:///repo/sub/y.md)"), "{out}");
        assert!(out.contains("[d](https://e.com)"), "{out}");
    }

    #[test]
    fn anchor_slug_roundtrips_and_path_excludes_anchor() {
        let out = resolve_relative_links("[x](#内置-claude-code-配置claude)", std::path::Path::new("/r"));
        let url = out.trim_start_matches("[x](").trim_end_matches(')');
        let slug = anchor_slug_from_nibfile_url(url).unwrap();
        assert_eq!(slug, "内置-claude-code-配置claude");
        // 锚点 URL 不被当文件路径
        assert!(path_from_nibfile_url(url).is_none());
    }

    #[test]
    fn normalize_anchor_matches_across_slug_styles() {
        // 链接里的 slug 与标题文本归一化后应相等(跨连字符/空格/标点)
        let from_href = normalize_anchor("内置-claude-code-配置claude");
        let from_heading = normalize_anchor("内置 Claude Code 配置(.claude/)");
        assert_eq!(from_href, from_heading);
        assert_eq!(from_href, "内置claudecode配置claude");
    }

    #[test]
    fn anchor_matches_handwritten_slug_dropping_paren_suffix() {
        // rate-native README 真实数据:锚点 `#内置-claude-code-配置`,标题
        // 「内置 Claude Code 配置(`.claude/`)」。标题归一化含尾部 `.claude/` 的 "claude",
        // 锚点没有 → 精确不中,靠前缀匹配(标题以 slug 为前缀)兜住。
        let md = "前言\n\n## 配置详解（按需看）\nx\n### 内置 Claude Code 配置（`.claude/`）\n内容\n";
        let secs = split_into_sections(md);
        assert_eq!(find_section_for_anchor(&secs, "内置-claude-code-配置"), Some(2));
        // 作者写全的锚点 → 精确命中,且不被前缀误配到别的段
        let md2 = "## 推荐的个人 Claude Code 设置（可选）\na\n## 别的\nb\n";
        let secs2 = split_into_sections(md2);
        assert_eq!(
            find_section_for_anchor(&secs2, "推荐的个人-claude-code-设置可选"),
            Some(0)
        );
        // 不存在的锚点 → None
        assert_eq!(find_section_for_anchor(&secs2, "根本没有这个章节"), None);
    }

    #[test]
    fn split_sections_by_heading_and_skip_fenced_hash() {
        let md = "前言\n\n## 标题一\n内容1\n```\n# 这是代码不是标题\n```\n### 标题二\n内容2\n";
        let secs = split_into_sections(md);
        // 前导段 + 两个标题段
        assert_eq!(secs.len(), 3);
        assert_eq!(secs[0].anchor_key, None);
        assert_eq!(secs[1].anchor_key.as_deref(), Some("标题一"));
        assert_eq!(secs[2].anchor_key.as_deref(), Some("标题二"));
        // 围栏里的 `# 这是代码` 不切段 → 归在标题一段内
        assert!(secs[1].text.contains("这是代码不是标题"));
        // 锚点 href 能定位到对应段
        let target = normalize_anchor("标题二");
        let ix = secs.iter().position(|s| s.anchor_key.as_deref() == Some(target.as_str()));
        assert_eq!(ix, Some(2));
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
