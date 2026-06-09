import { app, destroyCachedViewsByPrefix } from "./state";
import { showStatus, positionContextMenu } from "./utils";
import { openFile } from "./file-ops";
import { createFile, createDir, deletePath, renamePath } from "./tauri-api";
import { appPrompt, appConfirm } from "./dialog";
import type { FileNode } from "./file-tree";
import type { TabManager } from "./tabs";

let tabManager: TabManager;
let refreshTreeFn: () => Promise<void>;
let contextMenuTarget: FileNode | null = null;

export function initContextMenu(tm: TabManager, refreshTree: () => Promise<void>) {
  tabManager = tm;
  refreshTreeFn = refreshTree;

  document.getElementById("context-menu")!.addEventListener("click", (e) => {
    const action = (e.target as HTMLElement).dataset.action;
    if (action) handleContextMenuAction(action);
  });
  document.addEventListener("click", () => hideContextMenu());
  document.addEventListener("contextmenu", () => hideContextMenu());
}

export function showContextMenu(node: FileNode, x: number, y: number) {
  document.getElementById("multi-context-menu")?.classList.add("hidden");
  contextMenuTarget = node;
  positionContextMenu(document.getElementById("context-menu")!, x, y);
}

// 多选右键菜单:对全部选中项操作(目前只有删除)。单选走上面的静态 #context-menu;
// 多选项数不固定,故动态构建一个独立菜单。
export function showMultiContextMenu(nodes: FileNode[], x: number, y: number) {
  hideContextMenu();
  let menu = document.getElementById("multi-context-menu");
  if (!menu) {
    menu = document.createElement("div");
    menu.id = "multi-context-menu";
    menu.className = "context-menu";
    document.body.appendChild(menu);
  }
  menu.replaceChildren();

  const item = document.createElement("div");
  item.className = "context-menu-item";
  item.textContent = `Delete ${nodes.length} items`;
  item.addEventListener("click", (e) => {
    e.stopPropagation();
    menu!.classList.add("hidden");
    void deleteMultiple(nodes);
  });
  menu.appendChild(item);

  menu.classList.remove("hidden");
  positionContextMenu(menu, x, y);

  const dismiss = (e: MouseEvent) => {
    if (!menu!.contains(e.target as Node)) {
      menu!.classList.add("hidden");
      document.removeEventListener("mousedown", dismiss, true);
    }
  };
  setTimeout(() => document.addEventListener("mousedown", dismiss, true), 0);
}

// 批量删除:沿用单文件删除的清理(关标签页、销毁缓存的编辑器视图、清诊断),逐个删除后刷新一次。
async function deleteMultiple(nodes: FileNode[]) {
  if (nodes.length === 0) return;
  if (!(await appConfirm(`Delete ${nodes.length} items?`))) return;
  let deleted = 0;
  for (const node of nodes) {
    try {
      await deletePath(node.path);
      destroyCachedViewsByPrefix(node.path);
      tabManager.closeByPathPrefix(node.path);
      for (const key of app.diagnosticsMap.keys()) {
        const fp = key.replace("file://", "");
        if (fp.startsWith(node.path)) app.diagnosticsMap.delete(key);
      }
      deleted++;
    } catch (e) {
      showStatus(`Failed to delete ${node.name}: ${e}`, true);
    }
  }
  if (!tabManager.getActiveFile()) {
    app.currentFilePath = null;
    app.editorView = null;
  }
  await refreshTreeFn();
  showStatus(`Deleted ${deleted} item${deleted === 1 ? "" : "s"}`);
}

export function hideContextMenu() {
  document.getElementById("context-menu")!.classList.add("hidden");
  document.getElementById("multi-context-menu")?.classList.add("hidden");
  contextMenuTarget = null;
}

async function handleContextMenuAction(action: string) {
  if (!contextMenuTarget) return;
  const targetNode = contextMenuTarget;
  const parentPath = targetNode.isDir ? targetNode.path : targetNode.path.substring(0, targetNode.path.lastIndexOf("/"));
  hideContextMenu();

  switch (action) {
    case "new-file": {
      const name = await appPrompt("New file name:");
      if (!name) return;
      try {
        await createFile(`${parentPath}/${name}`);
        await refreshTreeFn();
        openFile(`${parentPath}/${name}`);
        showStatus(`Created ${name}`);
      } catch (e) {
        showStatus(`Failed to create file: ${e}`, true);
      }
      break;
    }
    case "new-folder": {
      const name = await appPrompt("New folder name:");
      if (!name) return;
      try {
        await createDir(`${parentPath}/${name}`);
        await refreshTreeFn();
        showStatus(`Created folder ${name}`);
      } catch (e) {
        showStatus(`Failed to create folder: ${e}`, true);
      }
      break;
    }
    case "rename": {
      const oldName = targetNode.name;
      const newName = await appPrompt("Rename to:", oldName);
      if (!newName || newName === oldName) return;
      const dirPath = targetNode.path.substring(0, targetNode.path.lastIndexOf("/"));
      try {
        await renamePath(targetNode.path, `${dirPath}/${newName}`);
        await refreshTreeFn();
        showStatus(`Renamed to ${newName}`);
      } catch (e) {
        showStatus(`Failed to rename: ${e}`, true);
      }
      break;
    }
    case "delete": {
      if (!(await appConfirm(`Delete "${targetNode.name}"?`))) return;
      try {
        await deletePath(targetNode.path);
        destroyCachedViewsByPrefix(targetNode.path);
        tabManager.closeByPathPrefix(targetNode.path);
        if (!tabManager.getActiveFile()) {
          app.currentFilePath = null;
          app.editorView = null;
        }
        for (const key of app.diagnosticsMap.keys()) {
          const fp = key.replace("file://", "");
          if (fp.startsWith(targetNode.path)) {
            app.diagnosticsMap.delete(key);
          }
        }
        await refreshTreeFn();
        showStatus(`Deleted ${targetNode.name}`);
      } catch (e) {
        showStatus(`Failed to delete: ${e}`, true);
      }
      break;
    }
  }
}
