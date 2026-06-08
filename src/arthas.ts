import type { EditorView } from "@codemirror/view";
import { lspDocumentSymbols, lspGotoDefinition, readFile } from "./tauri-api";
import {
  findMethodAtPosition, methodNameFromDeclLine,
  parseJdtFqn, parsePackage, classNameFromFilePath, identifierAt, followedByParen,
  resolveCallFqnByText,
} from "./arthas-symbols";

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) => setTimeout(() => reject(new Error("lsp timeout")), ms)),
  ]);
}

interface JavaContext {
  packageName: string;
  className: string;
  methodName: string | null;
}

export function getJavaContext(view: EditorView, pos: number): JavaContext | null {
  const doc = view.state.doc;
  let packageName = "";
  let className = "";
  const classStack: string[] = [];

  for (let i = 1; i <= Math.min(doc.lines, 30); i++) {
    const text = doc.line(i).text.trim();
    if (text.startsWith("package ") && text.endsWith(";")) {
      packageName = text.slice(8, -1).trim();
      break;
    }
  }

  const classPattern = /\b(?:class|interface|enum)\s+(\w+)/;
  let braceDepth = 0;
  let currentLine = 0;

  for (let i = 1; i <= doc.lines; i++) {
    const text = doc.line(i).text;
    const classMatch = text.match(classPattern);
    if (classMatch && !text.trimStart().startsWith("//") && !text.trimStart().startsWith("*")) {
      const openBefore = (text.substring(0, text.indexOf(classMatch[0])).match(/{/g) || []).length;
      if (braceDepth - openBefore >= classStack.length) {
        classStack.push(classMatch[1]);
      } else {
        while (classStack.length > braceDepth) classStack.pop();
        classStack.push(classMatch[1]);
      }
    }

    for (const ch of text) {
      if (ch === "{") braceDepth++;
      else if (ch === "}") {
        braceDepth--;
        if (braceDepth < classStack.length && braceDepth >= 0) {
          classStack.length = braceDepth;
        }
      }
    }

    if (doc.line(i).to >= pos && currentLine === 0) {
      currentLine = i;
      className = classStack.join("$");
      break;
    }
  }

  if (!className) {
    for (let i = 1; i <= doc.lines; i++) {
      const m = doc.line(i).text.match(classPattern);
      if (m) { className = m[1]; break; }
    }
  }
  if (!className) return null;

  // 文本兜底:光标停在方法声明行上(右键方法名最常见)优先取该行方法名,否则按花括号回溯。
  const methodName = methodFromCurrentLine(view, pos) ?? findEnclosingMethod(view, pos);

  const fqn = packageName ? `${packageName}.${className}` : className;
  return { packageName: fqn, className, methodName };
}

// 结构化解析(像 IDEA 的 PSI):用 jdtls 的 documentSymbol 找「包含光标的最内层方法/构造器」。
// 稳妥处理「光标在方法体内」和「光标停在方法声明名上」两种情况;LSP 未就绪/出错时返回 null,
// 交给文本兜底。line/character 为 0-based(LSP 约定)。解析逻辑在 arthas-symbols.ts(可单测)。
export async function resolveMethodViaLsp(
  filePath: string, line: number, character: number,
): Promise<string | null> {
  try {
    // 加 JS 侧超时:jdtls 还在索引/繁忙时,documentSymbol 可能迟迟不返回(串行 IPC 会卡)。
    // 超时即放弃 LSP、回退文本解析,确保 Arthas 操作绝不卡死。
    const symbols = await withTimeout(lspDocumentSymbols(filePath), 2500);
    return symbols.length ? findMethodAtPosition(symbols, line, character) : null;
  } catch {
    return null;
  }
}

// 解析「光标所在的方法调用/声明」→ 被调方法的声明类 FQCN + 方法名(像 IDEA resolve 方法调用)。
// - 光标在跨类方法调用上(如 richReadClient.batchQuery(...))→ 返回 RichReadClient + batchQuery;
// - 光标在同类方法调用上 → 当前类 + 被调方法名;
// - 光标在方法声明名上(gotoDefinition 指回自身)→ 返回 null,交回「最内层方法 + 当前类」逻辑;
// - 光标不在 `name(` 这种方法 token 上 → 返回 null。
export async function resolveCallTarget(
  view: EditorView, pos: number, filePath: string,
): Promise<{ fqn: string; method: string } | null> {
  const lineObj = view.state.doc.lineAt(pos);
  const id = identifierAt(lineObj.text, pos - lineObj.from);
  if (!id || !followedByParen(lineObj.text, id.end)) return null;

  // 1) gotoDefinition:项目方法 / 有源码的库方法走得通(更精确,能处理继承)。
  let def: { uri: string; line: number } | null = null;
  try {
    def = await withTimeout(lspGotoDefinition(filePath, lineObj.number - 1, id.start), 3500);
  } catch { /* jdtls 未就绪/出错 → 落文本兜底 */ }

  if (def) {
    const defPath = def.uri.startsWith("file://") ? def.uri.slice(7) : def.uri;
    const selfDecl = defPath === filePath && def.line === lineObj.number - 1; // 光标在声明自身上
    if (!selfDecl) {
      let fqn: string | null = null;
      if (def.uri.startsWith("jdt://")) {
        fqn = parseJdtFqn(def.uri);
      } else if (defPath.endsWith(".java")) {
        try {
          const content = await readFile(defPath);
          const pkg = parsePackage(content);
          fqn = pkg ? `${pkg}.${classNameFromFilePath(defPath)}` : classNameFromFilePath(defPath);
        } catch { /* ignore */ }
      }
      if (fqn) return { fqn, method: id.word };
    }
  }

  // 2) gotoDefinition 没给出 FQN(库类无源码 → 返回空,或 jdtls 未起来)→ 纯文本兜底:
  //    有接收者 → 接收者类型 + import 推 FQCN;无接收者 → 同类方法调用 = 当前类。
  //    方法声明行无接收者 → 返回 null,交回「最内层方法」逻辑。
  const fileText = view.state.doc.toString();
  const pkg = parsePackage(fileText);
  const currentClassFqn = pkg ? `${pkg}.${classNameFromFilePath(filePath)}` : classNameFromFilePath(filePath);
  return resolveCallFqnByText(fileText, lineObj.text, id.start, id.word, pkg, currentClassFqn);
}

// 当前行的文本兜底:光标停在方法声明行上(右键方法名最常见)时取该行方法名。
// 判定逻辑在 arthas-symbols.ts 的 methodNameFromDeclLine(纯函数,可单测)。
function methodFromCurrentLine(view: EditorView, pos: number): string | null {
  return methodNameFromDeclLine(view.state.doc.lineAt(pos).text);
}

function findEnclosingMethod(view: EditorView, pos: number): string | null {
  const doc = view.state.doc;
  const curLine = doc.lineAt(pos).number;
  const methodSig = /\b(?:public|protected|private|static|abstract|synchronized|final|native|default)\b[\s\S]*?\b(\w+)\s*\(/;

  let braceCount = 0;
  for (let i = curLine; i >= 1; i--) {
    const text = doc.line(i).text;

    for (let j = Math.min(text.length - 1, i === curLine ? pos - doc.line(i).from : text.length - 1); j >= 0; j--) {
      if (text[j] === "}") braceCount++;
      else if (text[j] === "{") {
        if (braceCount > 0) { braceCount--; }
        else {
          for (let k = i; k >= Math.max(1, i - 3); k--) {
            let combined = "";
            for (let l = k; l <= i; l++) combined += doc.line(l).text + " ";
            const m = combined.match(methodSig);
            if (m && !combined.match(/\b(?:class|interface|enum)\s+\w+/)) {
              return m[1];
            }
          }
          return null;
        }
      }
    }
  }
  return null;
}

export type ArthasCommand = "watch" | "trace" | "stack" | "monitor" | "tt";

export function generateArthasCommand(ctx: JavaContext, cmd: ArthasCommand): string {
  const fqn = ctx.packageName;
  const method = ctx.methodName || "*";

  switch (cmd) {
    case "watch":
      return `watch ${fqn} ${method} '{params,returnObj,throwExp}' -n 5 -x 3`;
    case "trace":
      return `trace ${fqn} ${method} -n 5 --skipJDKMethod false`;
    case "stack":
      return `stack ${fqn} ${method} -n 5`;
    case "monitor":
      return `monitor ${fqn} ${method} -c 5 -n 10`;
    case "tt":
      return `tt -t ${fqn} ${method} -n 5`;
  }
}
