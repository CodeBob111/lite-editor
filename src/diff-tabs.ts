import type { TabManager } from "./tabs";
import type { GitChange } from "./tauri-api";

export const diffDataStore = new Map<string, { original: string; modified: string; filename: string }>();

let _editorContainer: HTMLElement;
let _tabManager: TabManager;

export function initDiffTabs(editorContainer: HTMLElement, tabManager: TabManager) {
  _editorContainer = editorContainer;
  _tabManager = tabManager;
}

export function isDiffTab(path: string): boolean {
  return path.startsWith("diff:");
}

export function diffTabPath(repoPath: string, relPath: string): string {
  return `diff:${repoPath}/${relPath}`;
}

export function destroyActiveDiff() {
  _editorContainer.classList.remove("diff-styles");
  const mergeEl = _editorContainer.querySelector(".cm-mergeView");
  if (mergeEl) {
    import("./diff-view").then((m) => m.closeDiffView()).catch(() => {});
    mergeEl.remove();
  }
}

export async function renderDiffInEditor(path: string) {
  const data = diffDataStore.get(path);
  if (!data) return;
  _editorContainer.innerHTML = "";
  _editorContainer.classList.add("diff-styles");
  const { openDiffView } = await import("./diff-view");
  openDiffView(_editorContainer, data.original, data.modified, data.filename);
}

export function openDiffAsTab(repoPath: string, change: GitChange, original: string, modified: string) {
  const relPath = change.path;
  const tabPath = diffTabPath(repoPath, relPath);
  const filename = relPath.split("/").pop() || relPath;
  const suffix = change.status === "Deleted" ? " (deleted)" : change.status === "Added" ? " (new)" : "";
  diffDataStore.set(tabPath, { original, modified, filename });
  _tabManager.openFile(tabPath, "", `${filename}${suffix}`);
}
