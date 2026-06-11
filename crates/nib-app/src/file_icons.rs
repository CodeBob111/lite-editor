// 文件图标元信息(字形+颜色+是否淡化)单一来源——自旧版 file-icons.ts 逐键移植。
// Explorer 树、编辑器标签、Commit 改动行共用,保证同一文件各处颜色一致。

use gpui::{rgb, Hsla};

pub struct FileIconMeta {
    pub glyph: &'static str,
    pub color: Hsla,
    /// git 忽略/lock 等噪音文件:标签淡化显示
    pub dim: bool,
}

fn meta(glyph: &'static str, color: u32, dim: bool) -> FileIconMeta {
    FileIconMeta {
        glyph,
        color: rgb(color).into(),
        dim,
    }
}

pub fn file_icon_meta(name: &str) -> FileIconMeta {
    let lower = name.to_lowercase();

    // 特殊文件名优先于扩展名
    if lower == "pom.xml" {
        return meta("m", 0x7faedb, false);
    }
    if lower == ".gitignore" || lower == ".gitattributes" || lower == ".gitmodules" {
        return meta("≡", 0x69737b, true);
    }
    if lower.ends_with(".lock") || lower.ends_with("-lock.json") || lower.ends_with("-lock.yaml") {
        return meta("≡", 0x69737b, true);
    }

    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "java" => meta("J", 0xd08a5c, false),
        "kt" | "kts" => meta("K", 0xb997d2, false),
        "ts" | "tsx" => meta("T", 0x74b8d6, false),
        "js" | "jsx" | "mjs" | "cjs" => meta("J", 0xd8bf6a, false),
        "json" => meta("{}", 0xd8bf6a, false),
        "xml" => meta("X", 0xb997d2, false),
        "html" | "htm" => meta("H", 0xd88964, false),
        "css" | "scss" | "sass" | "less" => meta("#", 0x66b7bd, false),
        "md" | "markdown" => meta("M", 0x7faedb, false),
        "py" => meta("P", 0x74b8d6, false),
        "rs" => meta("R", 0xd88964, false),
        "go" => meta("G", 0x66b7bd, false),
        "sh" | "bash" | "zsh" => meta("$", 0x8fbc8f, false),
        "yaml" | "yml" => meta("Y", 0xa7b0b6, false),
        "properties" => meta("P", 0xa7b0b6, false),
        "toml" | "ini" | "conf" | "cfg" | "editorconfig" | "classpath" | "project" => {
            meta("≡", 0xd6a457, false)
        }
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" => {
            meta("▣", 0x8fbc8f, false)
        }
        "txt" | "log" => meta("≡", 0x69737b, true),
        _ => meta("·", 0xa7b0b6, false),
    }
}
