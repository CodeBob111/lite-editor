import type { EditorView } from "@codemirror/view";

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

  const methodName = findEnclosingMethod(view, pos);

  const fqn = packageName ? `${packageName}.${className}` : className;
  return { packageName: fqn, className, methodName };
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
