// Arthas 命令生成内核(自前端 arthas-symbols.ts 逐函数移植,语义对齐):
// 在 LSP 符号树/纯文本里解析「光标所在方法」与 FQCN,拼 watch/trace/stack/
// monitor/tt 命令。全部纯函数,便于单测;UI 层只负责取光标与展示。

use regex::Regex;
use std::sync::OnceLock;

const KIND_METHOD: u64 = 6;
const KIND_CONSTRUCTOR: u64 = 9;

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

/// 在 LSP documentSymbol 结果(层级 DocumentSymbol 或扁平 SymbolInformation)里
/// 找包含光标(0-based line/character)的最内层方法/构造器,返回裸方法名
/// (构造器返回 "<init>";jdtls 方法名可能带参数签名,截到 '(' 前)。
pub fn find_method_at_position(
    symbols: &serde_json::Value,
    line: u64,
    character: u64,
) -> Option<String> {
    fn range_of(node: &serde_json::Value) -> Option<&serde_json::Value> {
        node.get("range").or_else(|| node.get("location")?.get("range"))
    }
    fn pos_in_range(line: u64, ch: u64, r: &serde_json::Value) -> bool {
        let g = |p: &str, k: &str| r.get(p).and_then(|v| v.get(k)).and_then(|v| v.as_u64());
        let (Some(sl), Some(sc), Some(el), Some(ec)) = (
            g("start", "line"),
            g("start", "character"),
            g("end", "line"),
            g("end", "character"),
        ) else {
            return false;
        };
        if line < sl || line > el {
            return false;
        }
        if line == sl && ch < sc {
            return false;
        }
        if line == el && ch > ec {
            return false;
        }
        true
    }
    fn walk<'a>(nodes: &'a [serde_json::Value], line: u64, ch: u64, hits: &mut Vec<&'a serde_json::Value>) {
        for n in nodes {
            let Some(r) = range_of(n) else { continue };
            if !pos_in_range(line, ch, r) {
                continue;
            }
            if let Some(kind) = n.get("kind").and_then(|k| k.as_u64()) {
                if kind == KIND_METHOD || kind == KIND_CONSTRUCTOR {
                    hits.push(n);
                }
            }
            if let Some(children) = n.get("children").and_then(|c| c.as_array()) {
                walk(children, line, ch, hits);
            }
        }
    }

    let nodes = symbols.as_array()?;
    let mut hits = Vec::new();
    walk(nodes, line, character, &mut hits);
    // 取范围最小者 = 最内层(扁平列表无层级,按范围大小排序)
    hits.sort_by_key(|n| {
        let r = range_of(n).unwrap();
        let g = |p: &str, k: &str| r.get(p).and_then(|v| v.get(k)).and_then(|v| v.as_u64()).unwrap_or(0);
        (g("end", "line") - g("start", "line")) * 100000 + g("end", "character").saturating_sub(g("start", "character"))
    });
    let found = hits.first()?;
    if found.get("kind").and_then(|k| k.as_u64()) == Some(KIND_CONSTRUCTOR) {
        return Some("<init>".into());
    }
    let name = found.get("name")?.as_str()?;
    let bare = name.split('(').next().unwrap_or("").trim();
    if bare.is_empty() {
        None
    } else {
        Some(bare.to_string())
    }
}

/// jdt://contents/<jar>/<package>/<Class>.class?=... → package.Class
pub fn parse_jdt_fqn(uri: &str) -> Option<String> {
    if !uri.starts_with("jdt://") {
        return None;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let path = uri.split('?').next().unwrap_or(uri);
    let caps = re(
        &RE,
        r"/([A-Za-z_][A-Za-z0-9_.]*)/([A-Za-z_][A-Za-z0-9_$]*)\.class$",
    )
    .captures(path)?;
    Some(format!("{}.{}", &caps[1], &caps[2]))
}

/// 源码顶部 package 声明(前 60 行,遇 import/类声明即止)
pub fn parse_package(content: &str) -> String {
    for raw in content.lines().take(60) {
        let t = raw.trim();
        if let Some(rest) = t.strip_prefix("package ") {
            if let Some(pkg) = rest.strip_suffix(';') {
                return pkg.trim().to_string();
            }
        }
        if t.starts_with("import ")
            || t.starts_with("public ")
            || t.starts_with("class ")
            || t.starts_with("interface ")
            || t.starts_with("enum ")
        {
            break;
        }
    }
    String::new()
}

/// 文件名(去 .java)= Java 顶层公共类名(约定)
pub fn class_name_from_file_path(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".java")
        .to_string()
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// 取 col(字节列,行内)处所在标识符(含光标恰在词尾后一位)
pub fn identifier_at(text: &str, col: usize) -> Option<(String, usize, usize)> {
    if col > text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut s = col;
    let mut e = col;
    while s > 0 && is_ident(bytes[s - 1] as char) {
        s -= 1;
    }
    while e < bytes.len() && is_ident(bytes[e] as char) {
        e += 1;
    }
    if s == e {
        None
    } else {
        Some((text[s..e].to_string(), s, e))
    }
}

/// 标识符尾后(跳空白)紧跟 '(' → 方法调用/声明
pub fn followed_by_paren(text: &str, end: usize) -> bool {
    text[end.min(text.len())..]
        .chars()
        .find(|c| !c.is_whitespace())
        == Some('(')
}

/// `receiver.method(` 里 method 前的接收者(单段);无 '.' → None
pub fn receiver_before_dot(line_text: &str, id_start: usize) -> Option<String> {
    let bytes = line_text.as_bytes();
    let mut i = id_start;
    while i > 0 && (bytes[i - 1] as char).is_whitespace() {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] as char != '.' {
        return None;
    }
    i -= 1;
    while i > 0 && (bytes[i - 1] as char).is_whitespace() {
        i -= 1;
    }
    let e = i;
    let mut s = i;
    while s > 0 && is_ident(bytes[s - 1] as char) {
        s -= 1;
    }
    if s < e && matches!(bytes[s] as char, 'A'..='Z' | 'a'..='z' | '_' | '$') {
        Some(line_text[s..e].to_string())
    } else {
        None
    }
}

/// 全文找 `Type receiver`(Type 大写开头)推局部变量/字段类型
pub fn type_of_local_var(file_text: &str, receiver: &str) -> Option<String> {
    let pattern = format!(r"\b([A-Z][A-Za-z0-9_]*)\s+{}\b", regex::escape(receiver));
    Regex::new(&pattern).ok()?.captures(file_text).map(|c| c[1].to_string())
}

/// import 解析简单类名 → FQCN
pub fn fqn_from_imports(file_text: &str, type_name: &str) -> Option<String> {
    let pattern = format!(
        r"(?m)^\s*import\s+(?:static\s+)?([\w.]+\.{})\s*;",
        regex::escape(type_name)
    );
    Regex::new(&pattern).ok()?.captures(file_text).map(|c| c[1].to_string())
}

/// 语句关键字:`name(` 的 name 是这些则非方法声明
const STMT_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "catch", "synchronized", "return", "new", "throw", "else",
    "do", "try", "finally", "assert", "instanceof",
];

/// 单行文本是否方法声明,是则返回方法名(带修饰符或以 { 收尾才认定,防误判调用)
pub fn method_name_from_decl_line(text: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let caps = re(&RE, r"\b([a-zA-Z_]\w*)\s*\(").captures(text)?;
    let name = caps[1].to_string();
    if STMT_KEYWORDS.contains(&name.as_str()) {
        return None;
    }
    static MOD_RE: OnceLock<Regex> = OnceLock::new();
    let has_modifier = re(
        &MOD_RE,
        r"\b(?:public|protected|private|static|final|abstract|native)\b",
    )
    .is_match(text);
    static BRACE_RE: OnceLock<Regex> = OnceLock::new();
    let ends_with_brace = re(&BRACE_RE, r"\{\s*$").is_match(text);
    if has_modifier || ends_with_brace {
        Some(name)
    } else {
        None
    }
}

/// 纯文本解析「光标所在方法调用」的目标 FQCN+方法名(LSP 失效兜底)
pub fn resolve_call_fqn_by_text(
    file_text: &str,
    line_text: &str,
    id_start: usize,
    method: &str,
    current_package: &str,
    current_class_fqn: &str,
) -> Option<(String, String)> {
    match receiver_before_dot(line_text, id_start) {
        None => {
            if method_name_from_decl_line(line_text).is_some() {
                return None;
            }
            if current_class_fqn.is_empty() {
                None
            } else {
                Some((current_class_fqn.to_string(), method.to_string()))
            }
        }
        Some(receiver) => {
            let ty = if receiver.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                Some(receiver.clone())
            } else {
                type_of_local_var(file_text, &receiver)
            }?;
            if ty.contains('.') {
                return Some((ty, method.to_string()));
            }
            let fqn = fqn_from_imports(file_text, &ty).unwrap_or_else(|| {
                if current_package.is_empty() {
                    ty.clone()
                } else {
                    format!("{}.{}", current_package, ty)
                }
            });
            Some((fqn, method.to_string()))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArthasCommand {
    Watch,
    Trace,
    Stack,
    Monitor,
    Tt,
}

/// 拼 Arthas 命令(与旧版 generateArthasCommand 逐字对齐)
pub fn generate_arthas_command(fqn: &str, method: Option<&str>, cmd: ArthasCommand) -> String {
    let method = method.unwrap_or("*");
    match cmd {
        ArthasCommand::Watch => {
            format!("watch {} {} '{{params,returnObj,throwExp}}' -n 5 -x 3", fqn, method)
        }
        ArthasCommand::Trace => format!("trace {} {} -n 5 --skipJDKMethod false", fqn, method),
        ArthasCommand::Stack => format!("stack {} {} -n 5", fqn, method),
        ArthasCommand::Monitor => format!("monitor {} {} -c 5 -n 10", fqn, method),
        ArthasCommand::Tt => format!("tt -t {} {} -n 5", fqn, method),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_innermost_method_in_symbol_tree() {
        let symbols = json!([{
            "name": "Foo",
            "kind": 5,
            "range": {"start":{"line":0,"character":0},"end":{"line":20,"character":1}},
            "children": [{
                "name": "bar(List<Long>)",
                "kind": 6,
                "range": {"start":{"line":2,"character":2},"end":{"line":8,"character":3}}
            }, {
                "name": "Foo",
                "kind": 9,
                "range": {"start":{"line":10,"character":2},"end":{"line":12,"character":3}}
            }]
        }]);
        assert_eq!(find_method_at_position(&symbols, 3, 5), Some("bar".into()));
        assert_eq!(find_method_at_position(&symbols, 11, 4), Some("<init>".into()));
        assert_eq!(find_method_at_position(&symbols, 15, 0), None, "类内方法外");
    }

    #[test]
    fn parses_jdt_uri_and_package() {
        assert_eq!(
            parse_jdt_fqn("jdt://contents/rt.jar/java.util/List.class?=proj"),
            Some("java.util.List".into())
        );
        assert_eq!(parse_jdt_fqn("file:///a/B.java"), None);
        assert_eq!(parse_package("// hi\npackage com.a.b;\nimport x.Y;"), "com.a.b");
        assert_eq!(parse_package("import x.Y;\npackage com.a;"), "");
        assert_eq!(class_name_from_file_path("/x/y/UserService.java"), "UserService");
    }

    #[test]
    fn identifier_and_receiver_parsing() {
        let line = "  richReadClient.batchQuery(ids);";
        let (word, s, e) = identifier_at(line, 20).unwrap();
        assert_eq!(word, "batchQuery");
        assert!(followed_by_paren(line, e));
        assert_eq!(receiver_before_dot(line, s), Some("richReadClient".into()));
        assert_eq!(receiver_before_dot("  doThing(x);", 2), None);
    }

    #[test]
    fn resolves_call_fqn_via_imports_and_package() {
        let file = "package com.app;\nimport com.lib.RichReadClient;\nclass S { RichReadClient richReadClient; void go(){ richReadClient.batchQuery(1); } }";
        let line = "richReadClient.batchQuery(1);";
        let (word, s, _) = identifier_at(line, 17).unwrap();
        assert_eq!(word, "batchQuery");
        let got = resolve_call_fqn_by_text(file, line, s, &word, "com.app", "com.app.S").unwrap();
        assert_eq!(got, ("com.lib.RichReadClient".into(), "batchQuery".into()));
        // 无接收者的同类调用
        let got2 = resolve_call_fqn_by_text(file, "  helper(2);", 2, "helper", "com.app", "com.app.S").unwrap();
        assert_eq!(got2, ("com.app.S".into(), "helper".into()));
        // 声明行交回外层
        assert!(resolve_call_fqn_by_text(file, "public void helper(int x) {", 12, "helper", "com.app", "com.app.S").is_none());
    }

    #[test]
    fn decl_line_detection_is_conservative() {
        assert_eq!(method_name_from_decl_line("public void doIt(int a) {"), Some("doIt".into()));
        assert_eq!(method_name_from_decl_line("doIt(a);"), None, "调用不算声明");
        assert_eq!(method_name_from_decl_line("if (x) {"), None, "语句关键字排除");
    }

    #[test]
    fn arthas_command_strings_match_legacy() {
        assert_eq!(
            generate_arthas_command("com.a.B", Some("m"), ArthasCommand::Trace),
            "trace com.a.B m -n 5 --skipJDKMethod false"
        );
        assert_eq!(
            generate_arthas_command("com.a.B", None, ArthasCommand::Watch),
            "watch com.a.B * '{params,returnObj,throwExp}' -n 5 -x 3"
        );
    }
}
