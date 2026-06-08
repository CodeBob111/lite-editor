import type { LspDocumentSymbol, LspRange } from "./tauri-api";

// LSP SymbolKind
const KIND_METHOD = 6;
const KIND_CONSTRUCTOR = 9;

function posInRange(line: number, ch: number, r: LspRange): boolean {
  if (line < r.start.line || line > r.end.line) return false;
  if (line === r.start.line && ch < r.start.character) return false;
  if (line === r.end.line && ch > r.end.character) return false;
  return true;
}

// 纯函数:在 LSP 符号树里找「包含光标(line/character,0-based)的最内层方法/构造器」,
// 返回裸方法名(构造器返回 <init>)。兼容层级化 DocumentSymbol(range/children)与
// 扁平 SymbolInformation(location.range,无 children)。无 Tauri 依赖,便于单测。
export function findMethodAtPosition(
  symbols: LspDocumentSymbol[], line: number, character: number,
): string | null {
  const rangeOf = (n: LspDocumentSymbol): LspRange | undefined => n.range ?? n.location?.range;
  const hits: LspDocumentSymbol[] = [];
  const walk = (nodes: LspDocumentSymbol[]) => {
    for (const n of nodes) {
      const r = rangeOf(n);
      if (!r || !posInRange(line, character, r)) continue;
      if (n.kind === KIND_METHOD || n.kind === KIND_CONSTRUCTOR) hits.push(n);
      if (n.children && n.children.length) walk(n.children);
    }
  };
  walk(symbols);
  // 命中里取范围最小者 = 最内层(扁平列表无 children、未必有序,故按范围排序)。
  hits.sort((a, b) => {
    const ra = rangeOf(a)!, rb = rangeOf(b)!;
    const sa = (ra.end.line - ra.start.line) * 100000 + (ra.end.character - ra.start.character);
    const sb = (rb.end.line - rb.start.line) * 100000 + (rb.end.character - rb.start.character);
    return sa - sb;
  });
  const found = hits[0];
  if (!found) return null;
  if (found.kind === KIND_CONSTRUCTOR) return "<init>";
  // jdtls 的方法 name 可能带参数签名,如 "buildUserTagVOListNew(List<Long>)";Arthas 只要裸方法名。
  return found.name.replace(/\(.*$/, "").trim() || null;
}

// 从 jdt:// URI(jdtls 对库类返回的)解析出 FQCN。
// 形如 jdt://contents/<jar>/<package.dotted>/<ClassName>.class?=...  →  package.ClassName
export function parseJdtFqn(uri: string): string | null {
  if (!uri.startsWith("jdt://")) return null;
  const path = uri.split("?")[0];
  const m = path.match(/\/([A-Za-z_][A-Za-z0-9_.]*)\/([A-Za-z_][A-Za-z0-9_$]*)\.class$/);
  return m ? `${m[1]}.${m[2]}` : null;
}

// 解析源码顶部的 package 声明(纯文本,前 60 行,遇到 import/类声明即止)。
export function parsePackage(content: string): string {
  for (const raw of content.split("\n").slice(0, 60)) {
    const t = raw.trim();
    if (t.startsWith("package ") && t.endsWith(";")) return t.slice(8, -1).trim();
    if (t.startsWith("import ") || t.startsWith("public ") || /^(class|interface|enum)\b/.test(t)) break;
  }
  return "";
}

// 文件名(去掉 .java)= Java 顶层公共类名(约定)。
export function classNameFromFilePath(path: string): string {
  return (path.split("/").pop() || path).replace(/\.java$/, "");
}

// 取 col 处所在的标识符(含光标恰好在标识符尾后一位的情况)。
export function identifierAt(text: string, col: number): { word: string; start: number; end: number } | null {
  if (col < 0 || col > text.length) return null;
  const isId = (c: string) => /[A-Za-z0-9_$]/.test(c);
  let s = col, e = col;
  while (s > 0 && isId(text[s - 1])) s--;
  while (e < text.length && isId(text[e])) e++;
  return s === e ? null : { word: text.slice(s, e), start: s, end: e };
}

// 标识符结束位置之后(跳过空白)紧跟 '(' → 这是方法调用/声明。
export function followedByParen(text: string, end: number): boolean {
  let k = end;
  while (k < text.length && /\s/.test(text[k])) k++;
  return text[k] === "(";
}

// 取 `receiver.method(` 里 method 前面的接收者标识符(单段,如 richReadClient)。
// 没有 `.`(如直接 method() 或链式 foo().bar())→ null。idStart 为 method 标识符起点。
export function receiverBeforeDot(lineText: string, idStart: number): string | null {
  let i = idStart - 1;
  while (i >= 0 && /\s/.test(lineText[i])) i--;
  if (lineText[i] !== ".") return null;
  i--;
  while (i >= 0 && /\s/.test(lineText[i])) i--;
  const e = i + 1;
  let s = i;
  while (s >= 0 && /[A-Za-z0-9_$]/.test(lineText[s])) s--;
  s++;
  return s < e && /[A-Za-z_$]/.test(lineText[s]) ? lineText.slice(s, e) : null;
}

// 在整份源码里找局部变量/字段 receiver 的声明类型:`Type receiver`(Type 以大写开头)。
export function typeOfLocalVar(fileText: string, receiver: string): string | null {
  const m = fileText.match(new RegExp(`\\b([A-Z][A-Za-z0-9_]*)\\s+${receiver}\\b`));
  return m ? m[1] : null;
}

// 用文件顶部的 import 把简单类名解析成 FQCN:`import a.b.Type;` → a.b.Type。
export function fqnFromImports(fileText: string, typeName: string): string | null {
  const m = fileText.match(new RegExp(`^\\s*import\\s+(?:static\\s+)?([\\w.]+\\.${typeName})\\s*;`, "m"));
  return m ? m[1] : null;
}

// 纯文本解析「光标所在方法调用」的目标类 FQCN + 方法名(gotoDefinition 失效时的兜底)。
// - 有接收者 obj.method:obj 大写视作类名(静态调用),否则查其声明类型;再经 import/同包推 FQCN。
// - 无接收者 method():声明行 → null(交回外层);否则是同类方法调用 → 当前类 currentClassFqn。
export function resolveCallFqnByText(
  fileText: string, lineText: string, idStart: number, method: string,
  currentPackage: string, currentClassFqn: string,
): { fqn: string; method: string } | null {
  const receiver = receiverBeforeDot(lineText, idStart);
  if (!receiver) {
    // 无接收者:若该行像方法声明 → 交回「最内层方法」逻辑;否则当作同类方法调用。
    if (methodNameFromDeclLine(lineText)) return null;
    return currentClassFqn ? { fqn: currentClassFqn, method } : null;
  }
  const type = /^[A-Z]/.test(receiver) ? receiver : typeOfLocalVar(fileText, receiver);
  if (!type) return null;
  if (type.includes(".")) return { fqn: type, method };           // 已是限定名
  const fqn = fqnFromImports(fileText, type)                       // import 解析
    ?? (currentPackage ? `${currentPackage}.${type}` : type);     // 否则按同包
  return { fqn, method };
}

// 语句关键字:`name(` 里的 name 若是这些,则不是方法声明(if/for/synchronized 等)。
const STMT_KEYWORDS = new Set([
  "if", "for", "while", "switch", "catch", "synchronized", "return", "new",
  "throw", "else", "do", "try", "finally", "assert", "instanceof",
]);

// 纯函数(可单测):从「单行文本」判断它是不是方法声明,是则返回方法名,否则 null。
// 保守:要么该行带访问修饰符(public/private/…),要么以 `{` 收尾(方法体起始),
// 才认定是声明 —— 避免把方法调用(如 `obj.doThing(a);`)误判成声明。
// 这是 LSP(documentSymbol)未就绪时的文本兜底,专门覆盖「光标停在方法声明行」这一最常见场景。
export function methodNameFromDeclLine(text: string): string | null {
  const m = text.match(/\b([a-zA-Z_]\w*)\s*\(/);
  if (!m) return null;
  const name = m[1];
  if (STMT_KEYWORDS.has(name)) return null;
  const hasModifier = /\b(?:public|protected|private|static|final|abstract|native)\b/.test(text);
  const endsWithBrace = /\{\s*$/.test(text);
  return hasModifier || endsWithBrace ? name : null;
}
