export interface FileNode {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileNode[];
  expanded?: boolean;
}

export class FileTree {
  private container: HTMLElement;
  private onFileOpen: (path: string) => void;
  private root: FileNode | null = null;
  private activeFilePath: string | null = null;

  constructor(container: HTMLElement, onFileOpen: (path: string) => void) {
    this.container = container;
    this.onFileOpen = onFileOpen;
  }

  setRoot(node: FileNode) {
    this.root = node;
    this.render();
  }

  highlightFile(path: string) {
    this.activeFilePath = path;
    this.container.querySelectorAll(".tree-item").forEach((el) => {
      el.classList.toggle("active", (el as HTMLElement).dataset.path === path);
    });
  }

  revealFile(path: string) {
    if (!this.root) return;
    const parts = path.replace(this.root.path + "/", "").split("/");
    let current = this.root;

    for (let i = 0; i < parts.length - 1; i++) {
      const child = current.children?.find((c) => c.name === parts[i]);
      if (child && child.isDir) {
        child.expanded = true;
        current = child;
      }
    }

    this.render();

    requestAnimationFrame(() => {
      const el = this.container.querySelector(`[data-path="${CSS.escape(path)}"]`);
      if (el) {
        el.scrollIntoView({ block: "center", behavior: "smooth" });
        el.classList.add("active");
      }
    });
  }

  private render() {
    this.container.innerHTML = "";
    if (this.root) {
      this.renderNode(this.root, 0);
    }
  }

  private renderNode(node: FileNode, depth: number) {
    const item = document.createElement("div");
    item.className = "tree-item";
    if (node.path === this.activeFilePath) item.classList.add("active");
    item.dataset.path = node.path;

    let indent = "";
    for (let i = 0; i < depth; i++) {
      indent += '<span class="tree-indent"></span>';
    }

    const icon = node.isDir
      ? node.expanded ? "▾" : "▸"
      : this.getFileIcon(node.name);

    item.innerHTML = `${indent}<span class="tree-icon">${icon}</span><span class="tree-label">${node.name}</span>`;

    item.addEventListener("click", () => {
      if (node.isDir) {
        node.expanded = !node.expanded;
        this.render();
      } else {
        this.onFileOpen(node.path);
      }
    });

    this.container.appendChild(item);

    if (node.isDir && node.expanded && node.children) {
      const sorted = [...node.children].sort((a, b) => {
        if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      for (const child of sorted) {
        this.renderNode(child, depth + 1);
      }
    }
  }

  private getFileIcon(name: string): string {
    if (name.endsWith(".java")) return "☕";
    if (name.endsWith(".py")) return "🐍";
    if (name.endsWith(".ts") || name.endsWith(".tsx")) return "TS";
    if (name.endsWith(".js") || name.endsWith(".jsx")) return "JS";
    if (name.endsWith(".json")) return "{}";
    if (name.endsWith(".xml") || name.endsWith(".pom")) return "◇";
    if (name.endsWith(".md")) return "▤";
    return "○";
  }
}
