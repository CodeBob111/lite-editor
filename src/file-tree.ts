import { readDirTree, deletePath, copyPath, type FileNode } from "./tauri-api";
import { showStatus } from "./utils";
import { fileIconMeta } from "./file-icons";

export type { FileNode };

interface FlatItem {
  node: FileNode;
  depth: number;
}

const ITEM_HEIGHT = 26;
const OVERSCAN_AHEAD = 12;
const OVERSCAN_BEHIND = 4;

export class FileTree {
  private container: HTMLElement;
  private onFileOpen: (path: string) => void;
  private onContextMenu: ((node: FileNode, x: number, y: number) => void) | null = null;
  private onMultiContextMenu: ((nodes: FileNode[], x: number, y: number) => void) | null = null;
  private root: FileNode | null = null;
  private activeFilePath: string | null = null;
  private renderFrame: number | null = null;
  private pathIndex = new Map<string, FileNode>();

  private flatList: FlatItem[] = [];
  private spacer: HTMLElement;
  private viewport: HTMLElement;
  private contentWidth = 0;
  private renderedStart = -1;
  private renderedEnd = -1;
  private lastScrollTop = 0;
  private pool: HTMLElement[] = [];

  private selectedPaths = new Set<string>();
  private anchorIndex = -1;
  private clipboard: { paths: string[]; cut: boolean } | null = null;
  private undoStack: ({ type: "delete"; entries: { path: string; backup: string }[] } | { type: "paste"; paths: string[] })[] = [];

  constructor(container: HTMLElement, onFileOpen: (path: string) => void) {
    this.container = container;
    this.onFileOpen = onFileOpen;

    this.spacer = document.createElement("div");
    this.spacer.style.cssText = "position:relative;min-width:100%";
    this.viewport = document.createElement("div");
    this.viewport.style.cssText = "position:absolute;left:0;top:0;will-change:transform";
    this.spacer.appendChild(this.viewport);
    this.container.appendChild(this.spacer);

    this.container.tabIndex = -1;
    this.container.style.outline = "none";

    this.container.addEventListener("scroll", () => this.renderViewport(), { passive: true });

    this.container.addEventListener("mousedown", (e) => {
      this.container.focus();
      if (e.shiftKey) e.preventDefault();
    });

    this.container.addEventListener("click", (e) => {
      const item = (e.target as HTMLElement).closest<HTMLElement>(".tree-item");
      if (!item) return;
      const path = item.dataset.path;
      if (!path) return;
      const node = this.pathIndex.get(path);
      if (!node) return;
      const clickedIndex = this.flatList.findIndex((f) => f.node.path === path);

      if (e.shiftKey && this.anchorIndex >= 0 && clickedIndex >= 0) {
        const lo = Math.min(this.anchorIndex, clickedIndex);
        const hi = Math.max(this.anchorIndex, clickedIndex);
        this.selectedPaths.clear();
        for (let i = lo; i <= hi; i++) {
          this.selectedPaths.add(this.flatList[i].node.path);
        }
        this.renderedStart = -1;
        this.renderViewport();
        return;
      }

      if (e.metaKey || e.ctrlKey) {
        if (this.selectedPaths.has(path)) {
          this.selectedPaths.delete(path);
        } else {
          this.selectedPaths.add(path);
        }
        this.anchorIndex = clickedIndex;
        this.renderedStart = -1;
        this.renderViewport();
        return;
      }

      this.selectedPaths.clear();
      this.selectedPaths.add(path);
      this.anchorIndex = clickedIndex;
      this.renderedStart = -1;
      this.renderViewport();

      if (node.isDir) {
        this.toggleDir(node);
      } else {
        this.onFileOpen(node.path);
      }
    });

    this.container.addEventListener("contextmenu", (e) => {
      const item = (e.target as HTMLElement).closest<HTMLElement>(".tree-item");
      if (!item) return;
      const path = item.dataset.path;
      if (!path) return;
      const node = this.pathIndex.get(path);
      if (!node) return;
      e.preventDefault();
      e.stopPropagation();

      if (this.selectedPaths.size > 1 && this.selectedPaths.has(path) && this.onMultiContextMenu) {
        const nodes = this.flatList
          .filter((f) => this.selectedPaths.has(f.node.path))
          .map((f) => f.node);
        this.onMultiContextMenu(nodes, e.clientX, e.clientY);
      } else if (this.onContextMenu) {
        this.selectedPaths.clear();
        this.selectedPaths.add(path);
        this.anchorIndex = this.flatList.findIndex((f) => f.node.path === path);
        this.renderedStart = -1;
        this.renderViewport();
        this.onContextMenu(node, e.clientX, e.clientY);
      }
    });

    document.addEventListener("keydown", (e) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      // Only hijack Cmd+C/V/Z/Backspace for file ops when focus is NOT in an
      // editable surface. Otherwise these steal the keystroke from whatever is
      // focused — e.g. Cmd+V in the terminal (xterm's textarea) was pasting a
      // file into the tree instead of reaching the shell, and preventDefault
      // killed the terminal's own paste event.
      const active = document.activeElement as HTMLElement | null;
      if (
        active?.tagName === "INPUT" ||
        active?.tagName === "TEXTAREA" ||
        active?.isContentEditable ||
        active?.closest(".cm-editor") ||
        active?.closest("#search-panel") ||
        active?.closest(".terminal-wrapper")
      ) return;

      if ((e.key === "Backspace" || e.key === "Delete") && this.selectedPaths.size > 0) {
        e.preventDefault();
        this.deleteSelected();
      } else if (e.key === "c" && this.selectedPaths.size > 0) {
        e.preventDefault();
        this.copySelected();
      } else if (e.key === "v" && this.clipboard) {
        e.preventDefault();
        this.pasteClipboard();
      } else if (e.key === "z" && this.undoStack.length > 0) {
        e.preventDefault();
        this.undoLast();
      }
    });
  }

  private undoTrashDir(): string {
    return "/tmp/lite-editor-trash";
  }

  private async deleteSelected() {
    const paths = [...this.selectedPaths];
    if (paths.length === 0) return;

    const trashDir = this.undoTrashDir() + "/" + Date.now();
    const backups: { path: string; backup: string }[] = [];
    let deleted = 0;

    for (const p of paths) {
      const name = p.split("/").pop()!;
      const backup = trashDir + "/" + name;
      try {
        await copyPath(p, backup);
        backups.push({ path: p, backup });
        await deletePath(p);
        deleted++;
      } catch (err) {
        showStatus(`Failed to delete ${name}: ${err}`, true);
      }
    }

    if (backups.length > 0) {
      this.undoStack.push({ type: "delete", entries: backups });
    }

    this.selectedPaths.clear();
    showStatus(`Deleted ${deleted} item(s) (Cmd+Z to undo)`);
    await this.refreshRoot();
  }

  private copySelected() {
    this.clipboard = { paths: [...this.selectedPaths], cut: false };
    showStatus(`Copied ${this.clipboard.paths.length} item(s)`);
  }

  private async pasteClipboard() {
    if (!this.clipboard || this.clipboard.paths.length === 0) return;

    let targetDir: string | null = null;
    if (this.selectedPaths.size === 1) {
      const sel = [...this.selectedPaths][0];
      const node = this.pathIndex.get(sel);
      targetDir = node?.isDir ? node.path : sel.substring(0, sel.lastIndexOf("/"));
    } else if (this.root) {
      targetDir = this.root.path;
    }
    if (!targetDir) return;

    let pasted = 0;
    const pastedPaths: string[] = [];
    for (const src of this.clipboard.paths) {
      const name = src.split("/").pop()!;
      let dest = targetDir + "/" + name;
      if (dest === src) {
        const ext = name.lastIndexOf(".");
        const base = ext > 0 ? name.substring(0, ext) : name;
        const suffix = ext > 0 ? name.substring(ext) : "";
        dest = targetDir + "/" + base + " copy" + suffix;
      }
      try {
        await copyPath(src, dest);
        pastedPaths.push(dest);
        pasted++;
      } catch (err) {
        showStatus(`Failed to paste ${name}: ${err}`, true);
      }
    }

    if (pastedPaths.length > 0) {
      this.undoStack.push({ type: "paste", paths: pastedPaths });
    }
    showStatus(`Pasted ${pasted} item(s) (Cmd+Z to undo)`);
    await this.refreshRoot();
  }

  private async undoLast() {
    const op = this.undoStack.pop();
    if (!op) return;

    if (op.type === "delete") {
      let restored = 0;
      for (const { path, backup } of op.entries) {
        try {
          await copyPath(backup, path);
          await deletePath(backup);
          restored++;
        } catch (err) {
          showStatus(`Failed to restore ${path.split("/").pop()}: ${err}`, true);
        }
      }
      showStatus(`Restored ${restored} item(s)`);
    } else if (op.type === "paste") {
      let removed = 0;
      for (const p of op.paths) {
        try {
          await deletePath(p);
          removed++;
        } catch (err) {
          showStatus(`Failed to undo paste ${p.split("/").pop()}: ${err}`, true);
        }
      }
      showStatus(`Undo paste: removed ${removed} item(s)`);
    }
    await this.refreshRoot();
  }

  private async refreshRoot() {
    if (!this.root) return;
    const fresh = await readDirTree(this.root.path, 4);
    this.root.children = fresh.children;
    this.rebuild();
  }

  setContextMenuHandler(handler: (node: FileNode, x: number, y: number) => void) {
    this.onContextMenu = handler;
  }

  setMultiContextMenuHandler(handler: (nodes: FileNode[], x: number, y: number) => void) {
    this.onMultiContextMenu = handler;
  }

  getSelectedNodes(): FileNode[] {
    if (this.selectedPaths.size === 0) return [];
    return this.flatList
      .filter((f) => this.selectedPaths.has(f.node.path))
      .map((f) => f.node);
  }

  clearSelection() {
    this.selectedPaths.clear();
    this.anchorIndex = -1;
    this.renderedStart = -1;
    this.renderViewport();
  }

  setRoot(node: FileNode | null) {
    this.root = node;
    this.rebuild();
  }

  getRoot(): FileNode | null {
    return this.root;
  }

  highlightFile(path: string) {
    this.activeFilePath = path;
    this.renderedStart = -1;
    this.renderViewport();
  }

  async revealFile(path: string) {
    if (!this.root) return;
    if (!path.startsWith(this.root.path + "/")) return;

    this.root.expanded = true;
    const parts = path.replace(this.root.path + "/", "").split("/");
    let current = this.root;

    for (let i = 0; i < parts.length - 1; i++) {
      let child = current.children?.find((c) => c.name === parts[i]);
      if (!child && current.isDir) {
        try {
          const freshNode = await readDirTree(current.path, 4);
          current.children = freshNode.children;
          child = current.children?.find((c) => c.name === parts[i]);
        } catch { break; }
      }
      if (child && child.isDir) {
        child.expanded = true;
        current = child;
      } else {
        break;
      }
    }

    if (!current.children?.find((c) => c.name === parts[parts.length - 1])) {
      try {
        const freshNode = await readDirTree(current.path, 2);
        current.children = freshNode.children;
      } catch { /* ignore */ }
    }

    this.rebuild();

    requestAnimationFrame(() => {
      const idx = this.flatList.findIndex((f) => f.node.path === path);
      if (idx >= 0) {
        this.container.scrollTop = idx * ITEM_HEIGHT - this.container.clientHeight / 2;
        this.renderViewport();
      }
    });
  }

  getAllFiles(): string[] {
    const files: string[] = [];
    if (this.root) this.collectFiles(this.root, files);
    return files;
  }

  refresh() {
    this.rebuild();
  }

  private scheduleRender() {
    if (this.renderFrame !== null) return;
    this.renderFrame = requestAnimationFrame(() => {
      this.renderFrame = null;
      this.rebuild();
    });
  }

  private collectFiles(node: FileNode, files: string[]) {
    if (!node.isDir) {
      files.push(node.path);
    } else if (node.children) {
      for (const child of node.children) {
        this.collectFiles(child, files);
      }
    }
  }

  private async toggleDir(node: FileNode) {
    node.expanded = !node.expanded;

    const idx = this.flatList.findIndex((f) => f.node === node);
    if (idx < 0) {
      this.rebuild();
      return;
    }

    if (!node.expanded) {
      const nodeDepth = this.flatList[idx].depth;
      let removeEnd = idx + 1;
      while (removeEnd < this.flatList.length && this.flatList[removeEnd].depth > nodeDepth) {
        this.pathIndex.delete(this.flatList[removeEnd].node.path);
        removeEnd++;
      }
      this.flatList.splice(idx + 1, removeEnd - idx - 1);
    } else if (node.children && node.children.length > 0) {
      const items: FlatItem[] = [];
      const depth = this.flatList[idx].depth;
      for (const child of node.children) {
        this.flattenInto(child, depth + 1, items);
      }
      this.flatList.splice(idx + 1, 0, ...items);
    }

    this.updateSpacerSize();
    this.renderedStart = -1;
    this.renderedEnd = -1;
    this.renderViewport();

    if (!node.expanded || (node.children && node.children.length > 0)) return;
    try {
      const freshNode = await readDirTree(node.path, 2);
      node.children = freshNode.children ?? [];
      if (node.expanded) {
        const newIdx = this.flatList.findIndex((f) => f.node === node);
        if (newIdx >= 0) {
          const items: FlatItem[] = [];
          const depth = this.flatList[newIdx].depth;
          for (const child of node.children) {
            this.flattenInto(child, depth + 1, items);
          }
          this.flatList.splice(newIdx + 1, 0, ...items);
          this.updateSpacerSize();
          this.renderedStart = -1;
          this.renderViewport();
        }
      }
    } catch { /* ignore */ }
  }

  private flattenInto(node: FileNode, depth: number, out: FlatItem[]) {
    this.pathIndex.set(node.path, node);
    out.push({ node, depth });
    if (node.isDir && node.expanded && node.children) {
      for (const child of node.children) {
        this.flattenInto(child, depth + 1, out);
      }
    }
  }

  private rebuild() {
    this.flatList = [];
    this.pathIndex.clear();
    if (this.root) {
      this.flattenNode(this.root, 0);
    }
    this.container.scrollTop = 0;
    this.updateSpacerSize();
    this.renderedStart = -1;
    this.renderedEnd = -1;
    this.renderViewport();
  }

  private updateSpacerSize() {
    this.spacer.style.height = `${this.flatList.length * ITEM_HEIGHT}px`;
    this.contentWidth = this.estimateContentWidth();
    this.spacer.style.width = `${this.contentWidth}px`;
    this.viewport.style.width = `${this.contentWidth}px`;
  }

  private flattenNode(node: FileNode, depth: number) {
    this.pathIndex.set(node.path, node);
    this.flatList.push({ node, depth });
    if (node.isDir && node.expanded && node.children) {
      for (const child of node.children) {
        this.flattenNode(child, depth + 1);
      }
    }
  }

  private renderViewport() {
    if (this.flatList.length === 0) {
      this.returnAllToPool();
      this.spacer.style.height = "auto";
      this.spacer.style.width = "100%";
      this.viewport.style.width = "100%";
      return;
    }

    const scrollTop = this.container.scrollTop;
    const viewH = this.container.clientHeight;

    const scrollingDown = scrollTop >= this.lastScrollTop;
    this.lastScrollTop = scrollTop;
    const behind = scrollingDown ? OVERSCAN_BEHIND : OVERSCAN_AHEAD;
    const ahead = scrollingDown ? OVERSCAN_AHEAD : OVERSCAN_BEHIND;

    const rawStart = Math.floor(scrollTop / ITEM_HEIGHT);
    const rawEnd = Math.ceil((scrollTop + viewH) / ITEM_HEIGHT);
    const start = Math.max(0, rawStart - behind);
    const end = Math.min(this.flatList.length, rawEnd + ahead);

    if (start === this.renderedStart && end === this.renderedEnd) return;
    this.renderedStart = start;
    this.renderedEnd = end;

    const count = Math.max(0, end - start);
    const children = this.viewport.children;

    while (children.length > count) {
      const el = this.viewport.lastChild as HTMLElement;
      this.viewport.removeChild(el);
      this.pool.push(el);
    }

    while (children.length < count) {
      let el = this.pool.pop();
      if (!el) {
        el = document.createElement("div");
        // 三槽：[折叠箭头][类型图标][名称]。文件夹有箭头+文件夹图标，
        // 文件箭头槽留空（仍占宽），使各行类型图标对齐成一列。
        const chevron = document.createElement("span");
        chevron.className = "tree-chevron";
        const icon = document.createElement("span");
        icon.className = "tree-icon";
        const label = document.createElement("span");
        label.className = "tree-label";
        el.appendChild(chevron);
        el.appendChild(icon);
        el.appendChild(label);
      }
      this.viewport.appendChild(el);
    }

    const cw = this.contentWidth - 8;
    for (let i = 0; i < count; i++) {
      const { node, depth } = this.flatList[start + i];
      const el = children[i] as HTMLElement;
      const isActive = node.path === this.activeFilePath;
      const isSelected = this.selectedPaths.has(node.path);
      let cls = "tree-item";
      if (isActive) cls += " active";
      if (isSelected) cls += " selected";
      el.className = cls;
      el.dataset.path = node.path;
      const style = el.style;
      style.paddingLeft = `${10 + depth * 16}px`;
      style.height = `${ITEM_HEIGHT}px`;
      style.width = `${cw}px`;
      const chevronEl = el.children[0] as HTMLElement;
      const iconEl = el.children[1] as HTMLElement;
      const labelEl = el.children[2] as HTMLElement;
      if (node.isDir) {
        chevronEl.textContent = node.expanded ? "▾" : "▸";
        iconEl.textContent = "▤";
        // inline 着色：覆盖 .tree-item.active 的继承色，选中/打开时颜色不丢。
        iconEl.style.color = "#d6a457";
        labelEl.style.color = "";
      } else {
        chevronEl.textContent = "";
        const meta = fileIconMeta(node.name);
        iconEl.textContent = meta.glyph;
        iconEl.style.color = meta.color;
        labelEl.style.color = meta.dim ? "var(--text-muted)" : "";
      }
      labelEl.textContent = node.name;
    }

    this.viewport.style.top = `${start * ITEM_HEIGHT}px`;
  }

  private returnAllToPool() {
    const children = this.viewport.children;
    while (children.length > 0) {
      const el = this.viewport.lastChild as HTMLElement;
      this.viewport.removeChild(el);
      this.pool.push(el);
    }
  }

  private estimateContentWidth(): number {
    let max = this.container.clientWidth;
    for (const { node, depth } of this.flatList) {
      const width = 10 + depth * 16 + 16 + 16 + 4 + node.name.length * 8 + 28;
      if (width > max) max = width;
    }
    return Math.max(max, this.container.clientWidth);
  }

}
