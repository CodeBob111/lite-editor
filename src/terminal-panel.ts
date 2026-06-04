import { app } from "./state";
import { escapeHtml, showStatus } from "./utils";
import { appPrompt } from "./dialog";
import {
  spawnTerminal, writeTerminal, resizeTerminal, closeTerminal,
  getClaudeStatus,
  onTerminalOutput, onTerminalExit,
} from "./tauri-api";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";

interface TermInstance {
  id: number;
  xterm: Terminal;
  fitAddon: FitAddon;
  name: string;
  customName?: string;
  projectPath: string;
  wrapper: HTMLElement;
  claudeStatus: string | null;
}

const termInstances: TermInstance[] = [];
const activeTermByProject = new Map<string, number>();
let terminalResizeFrame: number | null = null;
let initialized = false;

async function renameTerminal(inst: TermInstance) {
  const newName = await appPrompt("Rename terminal", inst.customName || inst.name);
  if (newName !== null && newName.trim()) {
    inst.customName = newName.trim();
    renderTerminalTabs();
  }
}

function showContextMenu(inst: TermInstance, x: number, y: number) {
  document.querySelectorAll(".term-ctx-menu").forEach((m) => m.remove());
  const menu = document.createElement("div");
  menu.className = "term-ctx-menu";
  menu.style.cssText = `position:fixed;left:${x}px;top:${y}px;z-index:9999;`;
  menu.innerHTML = `<div class="term-ctx-item" data-action="rename">Rename</div>`;
  menu.addEventListener("click", (e) => {
    const action = (e.target as HTMLElement).dataset.action;
    menu.remove();
    if (action === "rename") renameTerminal(inst);
  });
  document.body.appendChild(menu);
  const dismiss = () => { menu.remove(); document.removeEventListener("click", dismiss); };
  setTimeout(() => document.addEventListener("click", dismiss), 0);
}

// Read the system clipboard and feed it to the terminal. Goes through
// xterm.paste() (not raw writeTerminal) so bracketed-paste mode in vim/shells is
// honored — a raw write would corrupt multi-line pastes there.
async function pasteIntoTerminal(xterm: Terminal) {
  try {
    const text = await navigator.clipboard.readText();
    if (text) xterm.paste(text);
  } catch {
    showStatus("Paste failed: clipboard unavailable", true);
  }
}

function showTerminalContextMenu(xterm: Terminal, x: number, y: number) {
  document.querySelectorAll(".term-ctx-menu").forEach((m) => m.remove());
  const menu = document.createElement("div");
  menu.className = "term-ctx-menu";
  menu.style.cssText = `position:fixed;left:${x}px;top:${y}px;z-index:9999;`;
  const item = document.createElement("div");
  item.className = "term-ctx-item";
  item.textContent = "Paste";
  item.addEventListener("click", () => {
    menu.remove();
    pasteIntoTerminal(xterm);
  });
  menu.appendChild(item);
  document.body.appendChild(menu);
  const dismiss = () => { menu.remove(); document.removeEventListener("click", dismiss); };
  setTimeout(() => document.addEventListener("click", dismiss), 0);
}

function currentTerminalProject() {
  return app.currentProjectPath || "__global__";
}

function currentProjectTerms() {
  return termInstances.filter((t) => t.projectPath === currentTerminalProject());
}

function activeTermIdForCurrentProject() {
  return activeTermByProject.get(currentTerminalProject()) ?? null;
}

function activeTermForCurrentProject() {
  const activeId = activeTermIdForCurrentProject();
  const terms = currentProjectTerms();
  return terms.find((t) => t.id === activeId) ?? terms[0] ?? null;
}

async function createNewTerminal() {
  const projectPath = currentTerminalProject();
  const cwd = app.currentProjectPath || ".";
  const container = document.getElementById("terminal-container")!;

  const xterm = new Terminal({
    fontSize: 13,
    fontFamily: "'SF Mono', 'Fira Code', 'Cascadia Code', monospace",
    theme: {
      background: "#101314",
      foreground: "#d7dee3",
      cursor: "#66b7bd",
      selectionBackground: "#2e4147",
      black: "#2b3336",
      red: "#df6b73",
      green: "#8fbc8f",
      yellow: "#d6a457",
      blue: "#7faedb",
      magenta: "#b997d2",
      cyan: "#66b7bd",
      white: "#dde3e7",
    },
    cursorBlink: true,
    allowProposedApi: true,
  });

  const fitAddon = new FitAddon();
  xterm.loadAddon(fitAddon);
  const unicode11 = new Unicode11Addon();
  xterm.loadAddon(unicode11);
  xterm.unicode.activeVersion = "11";

  const wrapper = document.createElement("div");
  wrapper.className = "terminal-wrapper";
  wrapper.dataset.projectPath = projectPath;
  wrapper.style.cssText = "position:absolute;inset:0;display:none;";
  container.appendChild(wrapper);
  xterm.open(wrapper);

  // Primary paste path: ride the native paste: → DOM `paste` event. Capture-phase
  // + stopPropagation runs us BEFORE xterm's own textarea handler, so we paste
  // exactly once regardless of whether xterm's built-in handler also fires.
  wrapper.addEventListener("paste", (e) => {
    const text = e.clipboardData?.getData("text/plain");
    if (text) {
      xterm.paste(text);
      e.preventDefault();
      e.stopPropagation();
    }
  }, true);

  // Fallback paste path: right-click → Paste, a guaranteed user-gesture route
  // through the Clipboard API in case the OS routes Cmd+V where no paste event fires.
  wrapper.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    showTerminalContextMenu(xterm, e.clientX, e.clientY);
  });

  await new Promise((r) => setTimeout(r, 50));
  fitAddon.fit();

  try {
    const termId = await spawnTerminal(cwd, xterm.cols, xterm.rows);
    const name = `${cwd.split("/").pop() || "Local"} (${currentProjectTerms().length + 1})`;
    const inst: TermInstance = { id: termId, xterm, fitAddon, name, projectPath, wrapper, claudeStatus: null };
    termInstances.push(inst);

    xterm.onData((data) => writeTerminal(termId, data).catch(() => {}));

    renderTerminalTabs();
    switchTerminal(currentProjectTerms().length - 1);
    showStatus(`Terminal opened: ${name}`);
  } catch (e) {
    wrapper.remove();
    xterm.dispose();
    showStatus(`Failed to open terminal: ${e}`, true);
  }
}

function renderTerminalTabs() {
  const tabsEl = document.getElementById("terminal-tabs")!;
  tabsEl.innerHTML = "";
  const visibleTerms = currentProjectTerms();
  const activeId = activeTermIdForCurrentProject();
  visibleTerms.forEach((inst, i) => {
    const tab = document.createElement("div");
    tab.className = `terminal-tab${inst.id === activeId ? " active" : ""}`;
    const badgeHtml = inst.claudeStatus ? `<span class="cc-badge cc-${inst.claudeStatus}">${inst.claudeStatus === "work" ? "W" : "I"}</span>` : "";
    tab.innerHTML = `<span class="terminal-tab-name">${escapeHtml(inst.customName || inst.name)}</span>${badgeHtml}<button class="terminal-tab-close">&times;</button>`;
    tab.querySelector(".terminal-tab-name")!.addEventListener("click", () => switchTerminal(i));
    tab.querySelector(".terminal-tab-name")!.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const me = e as MouseEvent;
      showContextMenu(inst, me.clientX, me.clientY);
    });
    tab.querySelector(".terminal-tab-close")!.addEventListener("click", (e) => {
      e.stopPropagation();
      closeTerminalTab(i);
    });
    tabsEl.appendChild(tab);
  });
}

function switchTerminal(index: number) {
  const visibleTerms = currentProjectTerms();
  const next = visibleTerms[index] ?? visibleTerms[0] ?? null;
  termInstances.forEach((t) => {
    t.wrapper.style.display = next && t.id === next.id ? "block" : "none";
  });
  if (next) activeTermByProject.set(currentTerminalProject(), next.id);
  else activeTermByProject.delete(currentTerminalProject());
  renderTerminalTabs();
  if (next) {
    setTimeout(() => {
      next.fitAddon.fit();
      resizeTerminal(next.id, next.xterm.cols, next.xterm.rows).catch(() => {});
      next.xterm.focus();
    }, 10);
  }
}

async function closeTerminalTab(index: number) {
  const inst = currentProjectTerms()[index];
  if (inst) await closeTerminalInstance(inst, index);
}

async function closeTerminalInstance(inst: TermInstance, visibleIndex = 0) {
  try { await closeTerminal(inst.id); } catch { /* ignore */ }
  inst.xterm.dispose();
  inst.wrapper.remove();
  const globalIndex = termInstances.findIndex((t) => t.id === inst.id);
  if (globalIndex >= 0) termInstances.splice(globalIndex, 1);

  const visibleTerms = currentProjectTerms();
  if (visibleTerms.length === 0) activeTermByProject.delete(currentTerminalProject());
  else switchTerminal(Math.min(visibleIndex, visibleTerms.length - 1));
  renderTerminalTabs();
}

export function syncTerminalProject() {
  const active = activeTermForCurrentProject();
  if (!active) {
    termInstances.forEach((t) => { t.wrapper.style.display = "none"; });
    renderTerminalTabs();
    return;
  }
  switchTerminal(Math.max(0, currentProjectTerms().findIndex((t) => t.id === active.id)));
}

export function initTerminalPanel() {
  if (initialized) return;
  initialized = true;

  onTerminalOutput((id, data) => {
    const inst = termInstances.find((t) => t.id === id);
    if (inst) inst.xterm.write(data);
  });

  onTerminalExit((id) => {
    const inst = termInstances.find((t) => t.id === id);
    if (inst) closeTerminalInstance(inst);
  });

  document.getElementById("terminal-add")!.addEventListener("click", () => createNewTerminal());

  document.querySelector('[data-panel="terminal"]')!.addEventListener("click", () => {
    const active = activeTermForCurrentProject();
    if (!active) createNewTerminal();
    else {
      syncTerminalProject();
      setTimeout(() => {
        active.fitAddon.fit();
        resizeTerminal(active.id, active.xterm.cols, active.xterm.rows).catch(() => {});
        active.xterm.focus();
      }, 50);
    }
  });

  const termResizeObserver = new ResizeObserver(() => {
    if (terminalResizeFrame !== null) return;
    terminalResizeFrame = requestAnimationFrame(() => {
      terminalResizeFrame = null;
      const inst = activeTermForCurrentProject();
      if (inst) {
        inst.fitAddon.fit();
        resizeTerminal(inst.id, inst.xterm.cols, inst.xterm.rows).catch(() => {});
      }
    });
  });
  termResizeObserver.observe(document.getElementById("terminal-container")!);

  setInterval(async () => {
    let changed = false;
    for (const inst of termInstances) {
      try {
        const status = await getClaudeStatus(inst.id);
        if (status !== inst.claudeStatus) {
          inst.claudeStatus = status;
          changed = true;
        }
      } catch { /* ignore */ }
    }
    if (changed) renderTerminalTabs();
  }, 2000);
}

export async function openTerminalPanel() {
  const active = activeTermForCurrentProject();
  if (!active) { await createNewTerminal(); return; }
  syncTerminalProject();
  setTimeout(() => {
    active.fitAddon.fit();
    resizeTerminal(active.id, active.xterm.cols, active.xterm.rows).catch(() => {});
    active.xterm.focus();
  }, 50);
}
