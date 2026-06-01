import { EditorView } from "@codemirror/view";
import { app } from "./state";
import { positionContextMenu, showStatus } from "./utils";
import { isBlameActive, toggleBlame } from "./git-blame-gutter";

export function showEditorContextMenu(view: EditorView, x: number, y: number) {
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
    const rect = arthasTrigger.getBoundingClientRect();
    arthasSub.style.left = `${rect.right}px`;
    arthasSub.style.top = `${rect.top}px`;
    arthasSub.style.display = "block";
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
    arthasSub.style.display = "none";
    arthasTrigger.removeEventListener("mouseenter", showSub);
    arthasTrigger.removeEventListener("mouseleave", hideSub);
    arthasSub.removeEventListener("mouseleave", hideSub);
    arthasTrigger.removeEventListener("click", toggleSub);
    document.removeEventListener("click", dismiss);
    document.removeEventListener("contextmenu", dismiss);
  };
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
  }, 0);
}

async function handleArthasAction(view: EditorView, action: string) {
  const { getJavaContext, generateArthasCommand } = await import("./arthas");
  const pos = view.state.selection.main.head;
  const ctx = getJavaContext(view, pos);
  if (!ctx) {
    showStatus("Cannot determine Java class context", true);
    return;
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
  await navigator.clipboard.writeText(command);
  showStatus(`Copied: ${command}`);
}
