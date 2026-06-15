//! 文本偏移换算:UTF-16 ↔ 字节。macOS 文本输入(IME)按 UTF-16 码元偏移传范围,而 Rust
//! String 按字节索引——终端 IME 改写合成中的预编辑串要在两者间转换,弄错会在退格/改字时错乱。

use std::ops::Range;

/// 字符串的 UTF-16 长度(码元数)。CJK 在 BMP 内各占 1 码元,非 BMP(emoji 等)占 2。
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// UTF-16 偏移 → 字节偏移。偏移落在字符内部或超界时,回退到下一字符起点 / 串尾(防越界 panic)。
pub fn utf16_to_byte(s: &str, utf16_off: usize) -> usize {
    let mut u = 0;
    for (b, c) in s.char_indices() {
        if u >= utf16_off {
            return b;
        }
        u += c.len_utf16();
    }
    s.len()
}

/// 字节范围 → UTF-16 范围(回填给 IME 的 adjusted_range)。
pub fn byte_range_to_utf16(s: &str, range: Range<usize>) -> Range<usize> {
    utf16_len(&s[..range.start])..utf16_len(&s[..range.end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_len_counts_code_units() {
        assert_eq!(utf16_len(""), 0);
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("你好"), 2); // CJK 各 1 码元(BMP)
        assert_eq!(utf16_len("a你b"), 3);
        assert_eq!(utf16_len("😀"), 2); // 非 BMP = 代理对 2 码元
    }

    #[test]
    fn utf16_to_byte_maps_cjk_offsets() {
        let s = "a你好"; // 字节 a(1)你(3)好(3);utf16 各 1
        assert_eq!(utf16_to_byte(s, 0), 0);
        assert_eq!(utf16_to_byte(s, 1), 1); // '你' 起点
        assert_eq!(utf16_to_byte(s, 2), 4); // '好' 起点
        assert_eq!(utf16_to_byte(s, 3), 7); // 串尾
        assert_eq!(utf16_to_byte(s, 99), 7); // 超界回退串尾
    }

    #[test]
    fn byte_range_roundtrips_for_cjk() {
        let s = "拼音ok"; // 拼(3)音(3)o(1)k(1)
        assert_eq!(byte_range_to_utf16(s, 0..6), 0..2);
        assert_eq!(byte_range_to_utf16(s, 6..8), 2..4);
        let b = utf16_to_byte(s, 2);
        assert_eq!(b, 6);
        assert_eq!(byte_range_to_utf16(s, 0..b), 0..2);
    }

    #[test]
    fn partial_edit_within_marked_buffer() {
        // 模拟合成中删一字:预编辑 "你好",删 utf16 [1,2) → 字节 [3,6)
        let mut marked = String::from("你好");
        let s = utf16_to_byte(&marked, 1);
        let e = utf16_to_byte(&marked, 2);
        marked.replace_range(s..e, "");
        assert_eq!(marked, "你");
    }
}
