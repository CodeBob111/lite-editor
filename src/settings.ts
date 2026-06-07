import { Compartment, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { indentUnit, bracketMatching, foldGutter } from "@codemirror/language";
import { app } from "./state";
import { loadSettingsData, saveSettingsData } from "./tauri-api";

// 偏好设置:单一真源在内存 `current`,持久化到 app_data_dir/settings.json
// (后端 save_settings/load_settings)。控件读 getSetting、改 setSetting,
// setSetting 立即落盘并应用到编辑器 —— 满足「控件绑定真实读写」硬约束。
//
// 应用方式分两类:
//   - 字体族/字号:写 CSS 变量(--editor-font-*),全部视图即时生效;
//   - 制表符宽度/自动换行/括号匹配/代码折叠:CodeMirror Compartment,
//     新开文件读当前值,改动时 reconfigure 当前活动视图即时生效。
// (minimap 本轮按用户决定不做,故不在表内。)

export interface SettingsSchemaItem {
  key: string;
  label: string;
  desc: string;
  type: "text" | "segment" | "select" | "toggle";
  options?: (number | string)[];
}

export const EDITOR_SETTINGS: SettingsSchemaItem[] = [
  { key: "editor.fontFamily", label: "Font Family", desc: "Monospace family used in the editor and terminal.", type: "text" },
  { key: "editor.fontSize", label: "Font Size", desc: "Editor font size in pixels.", type: "segment", options: [12, 13, 14, 16] },
  { key: "editor.tabSize", label: "Tab Size", desc: "Number of spaces a tab is equal to.", type: "select", options: [2, 4, 8] },
  { key: "editor.wordWrap", label: "Word Wrap", desc: "Wrap long lines at the viewport edge.", type: "toggle" },
  { key: "editor.matchBrackets", label: "Bracket Matching", desc: "Highlight the matching bracket pair.", type: "toggle" },
  { key: "editor.folding", label: "Code Folding", desc: "Show fold controls in the gutter.", type: "toggle" },
];

const DEFAULT_FONT = 'ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Code", Menlo, Consolas, monospace';

export const SETTINGS_DEFAULTS: Record<string, unknown> = {
  "editor.fontFamily": DEFAULT_FONT,
  "editor.fontSize": 13,
  "editor.tabSize": 4,
  "editor.wordWrap": true,
  "editor.matchBrackets": true,
  "editor.folding": true,
};

let current: Record<string, unknown> = { ...SETTINGS_DEFAULTS };

export function getSetting<T = unknown>(key: string): T {
  return (key in current ? current[key] : SETTINGS_DEFAULTS[key]) as T;
}

export function allSettings(): Record<string, unknown> {
  return { ...current };
}

// 启动时调用:读盘 → 合并默认值 → 应用字体 CSS 变量。Compartment 由 createEditorState
// 在首个视图创建时读取当前值,无需在此 reconfigure。
export async function initSettings(): Promise<void> {
  try {
    const raw = await loadSettingsData();
    if (raw) {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === "object") {
        current = { ...SETTINGS_DEFAULTS, ...parsed };
      }
    }
  } catch {
    current = { ...SETTINGS_DEFAULTS };
  }
  applyFontVars();
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;
function persist() {
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    saveSettingsData(JSON.stringify(current, null, 2)).catch(() => {});
  }, 250);
}

export function setSetting(key: string, value: unknown): void {
  current[key] = value;
  persist();
  if (key === "editor.fontFamily" || key === "editor.fontSize") applyFontVars();
  else reconfigureActiveEditor();
}

// 用整段 JSON 覆盖(settings.json 原始编辑标签用):非法 JSON 直接忽略,返回是否成功。
export function replaceAllSettings(json: string): boolean {
  try {
    const parsed = JSON.parse(json);
    if (!parsed || typeof parsed !== "object") return false;
    current = { ...SETTINGS_DEFAULTS, ...parsed };
    persist();
    applyFontVars();
    reconfigureActiveEditor();
    return true;
  } catch {
    return false;
  }
}

function applyFontVars() {
  const root = document.documentElement;
  root.style.setProperty("--editor-font-family", String(getSetting("editor.fontFamily")));
  root.style.setProperty("--editor-font-size", `${getSetting<number>("editor.fontSize")}px`);
}

// ---- CodeMirror compartments ----
const tabComp = new Compartment();
const wrapComp = new Compartment();
const bracketComp = new Compartment();
const foldComp = new Compartment();

function tabExt() {
  const n = getSetting<number>("editor.tabSize");
  return [EditorState.tabSize.of(n), indentUnit.of(" ".repeat(n))];
}
function wrapExt() {
  return getSetting<boolean>("editor.wordWrap") ? EditorView.lineWrapping : [];
}
function bracketExt() {
  return getSetting<boolean>("editor.matchBrackets") ? bracketMatching() : [];
}
function foldExt() {
  return getSetting<boolean>("editor.folding") ? foldGutter() : [];
}

// 供 createEditorState 注入:把四个可变设置包进 compartment,初值取当前设置。
export function editorSettingsExtensions() {
  return [
    tabComp.of(tabExt()),
    wrapComp.of(wrapExt()),
    bracketComp.of(bracketExt()),
    foldComp.of(foldExt()),
  ];
}

// 设置变更后即时刷新当前活动视图(后台缓存视图在下次打开时读新值)。
function reconfigureActiveEditor() {
  const view = app.editorView;
  if (!view) return;
  view.dispatch({
    effects: [
      tabComp.reconfigure(tabExt()),
      wrapComp.reconfigure(wrapExt()),
      bracketComp.reconfigure(bracketExt()),
      foldComp.reconfigure(foldExt()),
    ],
  });
}
