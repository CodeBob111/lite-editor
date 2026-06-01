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
  contextMenuTarget = node;
  positionContextMenu(document.getElementById("context-menu")!, x, y);
}

export function hideContextMenu() {
  document.getElementById("context-menu")!.classList.add("hidden");
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
