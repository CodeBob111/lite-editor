import { EditorView } from "@codemirror/view";
import { app } from "./state";
import { positionContextMenu, showStatus } from "./utils";
import { isBlameActive, toggleBlame } from "./git-blame-gutter";
import { copyTextToClipboard } from "./tauri-api";

// 上一次打开的菜单的清理函数。再次打开时先执行它:否则上一次注册的 document
// `contextmenu` dismiss 会在本次右键事件冒泡到 document 时把刚显示的菜单立刻藏掉
// (表现为「连续两次右键,第二次菜单不出来」),监听器也会残留。
let activeCleanup: (() => void) | null = null;

export function showEditorContextMenu(view: EditorView, x: number, y: number) {
  activeCleanup?.();
  const menu = document.getElementById("editor-context-menu")!;
  const blameItem = menu.querySelector<HTMLElement>('[data-action="toggle-blame"]')!;
  blameItem.textContent = isBlameActive(view) ? "Close Git Blame" : "Annotate with Git Blame";

  const arthasSection = document.getElementById("arthas-menu-section")!;
  const isJava = app.currentFilePath?.endsWith(".java") ?? false;
  arthasSection.style.display = isJava ? "" : "none";

  positionContextMenu(menu, x, y);

  const arthasSub = document.getElementById("arthas-submenu")!;
  const arthasTrigger = document.getElementById("arthas-submenu-trigger")!;
  arthasSub.style.display = "none";

  const showSub = () => {
    // 先显示再量尺寸(visibility 隐藏避免闪一下),据视口决定开在右侧还是翻到左侧。
    arthasSub.style.visibility = "hidden";
    arthasSub.style.display = "block";
    const t = arthasTrigger.getBoundingClientRect();
    const w = arthasSub.offsetWidth;
    const h = arthasSub.offsetHeight;
    const pad = 8;
    // 默认开在触发项右侧;右侧放不下(被遮挡/超出视口)就翻到左侧。
    let left = t.right;
    if (left + w + pad > window.innerWidth) {
      left = t.left - w;
      if (left < pad) left = Math.max(pad, window.innerWidth - w - pad);
    }
    // 纵向:超出底部就上移贴住视口。
    let top = t.top;
    if (top + h + pad > window.innerHeight) top = Math.max(pad, window.innerHeight - h - pad);
    arthasSub.style.left = `${left}px`;
    arthasSub.style.top = `${top}px`;
    arthasSub.style.visibility = "";
  };
  const hideSub = (e: MouseEvent) => {
    const related = e.relatedTarget as Node | null;
    if (related && (arthasTrigger.contains(related) || arthasSub.contains(related))) return;
    arthasSub.style.display = "none";
  };
  const toggleSub = (e: MouseEvent) => {
    e.stopPropagation();
    if (arthasSub.style.display === "block") {
      arthasSub.style.display = "none";
    } else {
      showSub();
    }
  };

  arthasTrigger.addEventListener("mouseenter", showSub);
  arthasTrigger.addEventListener("mouseleave", hideSub);
  arthasSub.addEventListener("mouseleave", hideSub);
  arthasTrigger.addEventListener("click", toggleSub);

  const cleanup = () => {
    if (activeCleanup === cleanup) activeCleanup = null;
    arthasSub.style.display = "none";
    arthasTrigger.removeEventListener("mouseenter", showSub);
    arthasTrigger.removeEventListener("mouseleave", hideSub);
    arthasSub.removeEventListener("mouseleave", hideSub);
    arthasTrigger.removeEventListener("click", toggleSub);
    document.removeEventListener("click", dismiss);
    document.removeEventListener("contextmenu", dismiss);
    document.removeEventListener("wheel", dismiss);
  };
  activeCleanup = cleanup;
  const handler = (e: MouseEvent) => {
    const action = (e.target as HTMLElement).dataset.action;
    if (!action) return;
    menu.classList.add("hidden");
    cleanup();
    if (action === "toggle-blame" && app.currentProjectPath && app.currentFilePath) {
      toggleBlame(view, app.currentProjectPath, app.currentFilePath);
    } else if (action.startsWith("arthas-")) {
      handleArthasAction(view, action);
    }
  };
  const dismiss = () => {
    menu.classList.add("hidden");
    cleanup();
  };

  menu.onclick = handler;
  setTimeout(() => {
    document.addEventListener("click", dismiss);
    document.addEventListener("contextmenu", dismiss);
    // 滚动(滚轮)时关闭菜单 —— 否则菜单是 fixed 浮层,代码滚走了它还钉在原地。
    document.addEventListener("wheel", dismiss, { passive: true });
  }, 0);
}

async function handleArthasAction(view: EditorView, action: string) {
  const { getJavaContext, generateArthasCommand, resolveMethodViaLsp, resolveCallTarget } = await import("./arthas");
  const pos = view.state.selection.main.head;
  const ctx = getJavaContext(view, pos);
  if (!ctx) {
    showStatus("Cannot determine Java class context", true);
    return;
  }
  // 像 IDEA:光标在方法调用上 → 解析被调方法的类 + 方法名;否则用「包含光标的最内层方法」。
  if (app.currentFilePath) {
    try {
      const target = await resolveCallTarget(view, pos, app.currentFilePath);
      if (target) {
        ctx.packageName = target.fqn;   // 被调方法的声明类 FQCN(getJavaContext 里 packageName 即 FQN)
        ctx.methodName = target.method;
      } else {
        const lineObj = view.state.doc.lineAt(pos);
        const m = await resolveMethodViaLsp(app.currentFilePath, lineObj.number - 1, pos - lineObj.from);
        if (m) ctx.methodName = m;
      }
    } catch { /* LSP 不可用则保留文本兜底 */ }
  }
  const cmdMap: Record<string, "watch" | "trace" | "stack" | "monitor" | "tt"> = {
    "arthas-watch": "watch",
    "arthas-trace": "trace",
    "arthas-stack": "stack",
    "arthas-monitor": "monitor",
    "arthas-tt": "tt",
  };
  const cmd = cmdMap[action];
  if (!cmd) return;
  const command = generateArthasCommand(ctx, cmd);
  // 走原生剪贴板(NSPasteboard):异步 await 之后也能可靠写入,不再需要点两遍
  // (navigator.clipboard.writeText 在 await 出用户手势后会被 WKWebView 拒绝)。
  await copyTextToClipboard(command);
  showStatus(`Copied: ${command}`);
}
