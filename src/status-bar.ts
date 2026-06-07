import { app } from "./state";

// 状态栏(底部)对齐设计稿 A8:活跃时整条变 accent 蓝,左侧 Git 分支 + 诊断计数,
// 右侧 行列 / 缩进 / 编码 / 行尾 / 语言 / LSP 点。数据各有来源:
//   - 行列:编辑器 updateListener(见 editor-setup.ts)每次选区变化调 updateStatusCursor
//   - 语言:活动文件扩展名
//   - 分支:git-panel 解析出当前 HEAD 时回灌(setStatusBranch)
//   - 诊断计数:LSP 应用诊断时回灌(setStatusDiagnostics)
//   - 缩进/编码/行尾:当前为固定值(编辑器统一 Spaces:4 / UTF-8 / LF)

// 扩展名 → 语言标签(比 utils.getLanguageId 覆盖更全,纯展示用)。
const LANG_LABEL: Record<string, string> = {
  java: "Java", kt: "Kotlin", py: "Python", ts: "TypeScript", tsx: "TypeScript",
  js: "JavaScript", jsx: "JavaScript", json: "JSON", xml: "XML", html: "HTML",
  css: "CSS", md: "Markdown", yml: "YAML", yaml: "YAML", sh: "Shell",
  rs: "Rust", go: "Go", sql: "SQL", properties: "Properties", txt: "Plain Text",
};

function langLabel(file: string): string {
  const ext = (file.split(".").pop() || "").toLowerCase();
  return LANG_LABEL[ext] ?? "Plain Text";
}

export function updateStatusCursor() {
  const el = document.getElementById("status-cursor");
  if (!el) return;
  const view = app.editorView;
  if (!view) { el.textContent = ""; return; }
  const head = view.state.selection.main.head;
  const line = view.state.doc.lineAt(head);
  el.textContent = `Ln ${line.number}, Col ${head - line.from + 1}`;
}

// 活动文件变化时刷新整条状态栏:无文件(或 diff 标签)→ 收起右侧信息、回到非活跃灰底。
export function updateStatusBar() {
  const bar = document.getElementById("status-bar");
  const right = document.getElementById("status-right");
  const file = app.currentFilePath;
  if (!file || file.startsWith("diff:") || file === "welcome.ts") {
    bar?.classList.remove("active");
    right?.classList.add("hidden");
    updateBreadcrumb();
    return;
  }
  bar?.classList.add("active");
  right?.classList.remove("hidden");
  const langEl = document.getElementById("status-lang");
  if (langEl) langEl.textContent = langLabel(file);
  updateStatusCursor();
  updateBreadcrumb();
}

// 面包屑(设计稿 A4):项目 › src › main › … › 文件名,分隔符用 --faint。
// 无文件 / diff / 欢迎页时隐藏。
export function updateBreadcrumb() {
  const bar = document.getElementById("breadcrumb");
  if (!bar) return;
  const file = app.currentFilePath;
  if (!file || file.startsWith("diff:") || file === "welcome.ts") {
    bar.classList.add("hidden");
    return;
  }
  const proj = app.currentProjectPath;
  let rel = file;
  if (proj && file.startsWith(proj + "/")) rel = file.slice(proj.length + 1);
  const projName = proj ? proj.split("/").pop() || "" : "";
  const parts = (projName ? [projName, ...rel.split("/")] : rel.split("/")).filter(Boolean);
  bar.replaceChildren();
  parts.forEach((p, i) => {
    if (i > 0) {
      const sep = document.createElement("span");
      sep.className = "crumb-sep";
      sep.textContent = "›";
      bar.appendChild(sep);
    }
    const c = document.createElement("span");
    c.className = "crumb";
    c.textContent = p;
    bar.appendChild(c);
  });
  bar.classList.remove("hidden");
}

// 由 git-panel 在解析出当前 HEAD 时回灌。空字符串则隐藏分支段。
export function setStatusBranch(branch: string | null) {
  const seg = document.getElementById("status-branch");
  const name = document.getElementById("status-branch-name");
  if (!seg || !name) return;
  if (branch) {
    name.textContent = branch;
    seg.classList.remove("hidden");
  } else {
    seg.classList.add("hidden");
  }
}

// 由 LSP 在应用诊断时回灌当前文件的错误/警告数。两者皆 0 则隐藏诊断段。
export function setStatusDiagnostics(errors: number, warnings: number) {
  const seg = document.getElementById("status-diags");
  const errEl = document.getElementById("status-diags-err");
  const warnEl = document.getElementById("status-diags-warn");
  if (!seg || !errEl || !warnEl) return;
  if (errors === 0 && warnings === 0) {
    seg.classList.add("hidden");
    return;
  }
  errEl.textContent = String(errors);
  warnEl.textContent = String(warnings);
  seg.classList.remove("hidden");
}
