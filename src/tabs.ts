export interface OpenTab {
  path: string;
  name: string;
  content: string;
  dirty: boolean;
}

export class TabManager {
  private container: HTMLElement;
  private onTabActivate: (path: string, content: string) => void;
  private onTabClose: ((path: string) => void) | null;
  private onStateChange: (() => void) | null;
  private onAllClosed: (() => void) | null;
  private tabs: OpenTab[] = [];
  private activeIndex: number = -1;
  private activeElement: HTMLElement | null = null;

  constructor(
    container: HTMLElement,
    onTabActivate: (path: string, content: string) => void,
    onStateChange?: () => void,
    onTabClose?: (path: string) => void,
    onAllClosed?: () => void,
  ) {
    this.container = container;
    this.onTabActivate = onTabActivate;
    this.onStateChange = onStateChange ?? null;
    this.onTabClose = onTabClose ?? null;
    this.onAllClosed = onAllClosed ?? null;
  }

  openFile(path: string, content?: string, displayName?: string) {
    const existing = this.tabs.findIndex((t) => t.path === path);
    if (existing >= 0) {
      if (content !== undefined) {
        this.tabs[existing].content = content;
      }
      this.activateTab(existing);
      return;
    }

    const name = displayName || path.split("/").pop() || path;
    const fileContent = content ?? "";

    this.tabs.push({ path, name, content: fileContent, dirty: false });
    this.activeIndex = this.tabs.length - 1;
    this.render();
    this.onTabActivate(path, fileContent);
    this.onStateChange?.();
  }

  hasTab(path: string): boolean {
    return this.tabs.some((t) => t.path === path);
  }

  activatePath(path: string): boolean {
    const index = this.tabs.findIndex((t) => t.path === path);
    if (index < 0) return false;
    this.activateTab(index);
    return true;
  }

  setContent(path: string, content: string) {
    const tab = this.tabs.find((t) => t.path === path);
    if (tab) tab.content = content;
  }

  markSaved(path: string) {
    const tab = this.tabs.find((t) => t.path === path);
    if (tab && tab.dirty) {
      tab.dirty = false;
      this.updateTabElement(path);
    }
  }

  markDirty(path: string) {
    const tab = this.tabs.find((t) => t.path === path);
    if (tab && !tab.dirty) {
      tab.dirty = true;
      this.updateTabElement(path);
    }
  }

  closeTab(index: number) {
    if (index < 0 || index >= this.tabs.length) return;
    const closed = this.tabs[index];
    const wasActive = index === this.activeIndex;
    this.tabs.splice(index, 1);
    if (closed) this.onTabClose?.(closed.path);

    if (this.tabs.length === 0) {
      this.activeIndex = -1;
      this.render();
      this.onAllClosed?.();
    } else if (wasActive) {
      this.activeIndex = Math.min(index, this.tabs.length - 1);
      this.render();
      const tab = this.tabs[this.activeIndex];
      this.onTabActivate(tab.path, tab.content);
    } else {
      if (index < this.activeIndex) this.activeIndex--;
      this.render();
    }
    this.onStateChange?.();
  }

  closeByPathPrefix(prefix: string) {
    for (let i = this.tabs.length - 1; i >= 0; i--) {
      if (this.tabs[i].path === prefix || this.tabs[i].path.startsWith(prefix + "/")) {
        this.onTabClose?.(this.tabs[i].path);
        this.tabs.splice(i, 1);
      }
    }
    if (this.activeIndex >= this.tabs.length) {
      this.activeIndex = this.tabs.length - 1;
    }
    if (this.activeIndex >= 0) {
      this.render();
      const tab = this.tabs[this.activeIndex];
      this.onTabActivate(tab.path, tab.content);
    } else {
      this.render();
      this.onAllClosed?.();
    }
    this.onStateChange?.();
  }

  getActiveFile(): string | null {
    if (this.activeIndex >= 0 && this.activeIndex < this.tabs.length) {
      return this.tabs[this.activeIndex].path;
    }
    return null;
  }

  getTabs(): OpenTab[] {
    return this.tabs.map((t) => ({ ...t }));
  }

  getActiveIndex(): number {
    return this.activeIndex;
  }

  restoreState(tabs: OpenTab[], activeIndex: number) {
    this.tabs = tabs.map((t) => ({ ...t }));
    this.activeIndex = tabs.length === 0
      ? -1
      : Math.max(0, Math.min(activeIndex, tabs.length - 1));
    this.render();
    if (this.activeIndex >= 0 && this.activeIndex < this.tabs.length) {
      const tab = this.tabs[this.activeIndex];
      this.onTabActivate(tab.path, tab.content);
    }
  }

  closeOtherTabs(keepIndex: number) {
    if (keepIndex < 0 || keepIndex >= this.tabs.length) return;
    const kept = this.tabs[keepIndex];
    for (let i = this.tabs.length - 1; i >= 0; i--) {
      if (i !== keepIndex) this.onTabClose?.(this.tabs[i].path);
    }
    this.tabs = [kept];
    this.activeIndex = 0;
    this.render();
    this.onTabActivate(kept.path, kept.content);
    this.onStateChange?.();
  }

  closeActiveTab() {
    if (this.activeIndex >= 0) this.closeTab(this.activeIndex);
  }

  closeAll() {
    for (const t of this.tabs) this.onTabClose?.(t.path);
    this.tabs = [];
    this.activeIndex = -1;
    this.render();
    this.onStateChange?.();
  }

  private activateTab(index: number) {
    if (index < 0 || index >= this.tabs.length) return;
    const wasActive = this.activeIndex === index;
    this.activeIndex = index;
    this.updateActiveTabElements();
    const tab = this.tabs[index];
    if (tab && !wasActive) {
      this.onTabActivate(tab.path, tab.content);
      this.onStateChange?.();
    }
  }

  private render() {
    this.container.innerHTML = "";
    this.activeElement = null;
    const fragment = document.createDocumentFragment();
    this.tabs.forEach((tab, i) => {
      const el = document.createElement("div");
      el.className = `tab${i === this.activeIndex ? " active" : ""}`;
      el.dataset.path = tab.path;
      if (i === this.activeIndex) this.activeElement = el;

      const close = document.createElement("span");
      close.className = "tab-close";
      close.textContent = "×";
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        this.closeTab(i);
      });
      el.appendChild(close);

      const icon = document.createElement("span");
      const isDiff = tab.path.startsWith("diff:");
      const ext = tab.name.split(".").pop()?.toLowerCase() || "";
      const iconMap: Record<string, [string, string]> = {
        java: ["J", "tab-icon-java"],
        xml: ["X", "tab-icon-xml"],
        md: ["M", "tab-icon-md"],
        ts: ["T", "tab-icon-ts"],
        tsx: ["T", "tab-icon-ts"],
        js: ["J", "tab-icon-js"],
        json: ["{}", "tab-icon-json"],
        css: ["#", "tab-icon-css"],
        html: ["H", "tab-icon-html"],
      };
      const [letter, cls] = isDiff ? ["D", "tab-icon-diff"] : (iconMap[ext] || ["·", "tab-icon-default"]);
      icon.className = `tab-icon ${cls}`;
      icon.textContent = letter;
      el.appendChild(icon);

      const label = document.createElement("span");
      label.className = "tab-label";
      label.textContent = tab.dirty ? `● ${tab.name}` : tab.name;
      el.appendChild(label);

      el.addEventListener("click", () => this.activateTab(i));
      el.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.showTabContextMenu(i, e.clientX, e.clientY);
      });
      fragment.appendChild(el);
    });
    this.container.appendChild(fragment);
  }

  private updateActiveTabElements() {
    this.activeElement?.classList.remove("active");
    const tab = this.tabs[this.activeIndex];
    this.activeElement = tab
      ? this.container.querySelector<HTMLElement>(`.tab[data-path="${CSS.escape(tab.path)}"]`)
      : null;
    this.activeElement?.classList.add("active");
  }

  private updateTabElement(path: string) {
    const index = this.tabs.findIndex((t) => t.path === path);
    if (index < 0) return;
    const tab = this.tabs[index];
    const el = this.container.querySelector<HTMLElement>(`.tab[data-path="${CSS.escape(path)}"]`);
    const label = el?.querySelector<HTMLElement>(".tab-label");
    if (label) {
      label.textContent = tab.dirty ? `● ${tab.name}` : tab.name;
    }
  }

  private showTabContextMenu(index: number, x: number, y: number) {
    let menu = document.getElementById("tab-context-menu") as HTMLElement | null;
    if (!menu) {
      menu = document.createElement("div");
      menu.id = "tab-context-menu";
      menu.className = "context-menu";
      document.body.appendChild(menu);
    }
    menu.innerHTML = "";

    const items: [string, string, () => void][] = [
      ["Close", "⌘W", () => this.closeTab(index)],
      ["Close Other Tabs", "", () => this.closeOtherTabs(index)],
      ["Close All Tabs", "", () => this.closeAll()],
    ];

    for (const [label, shortcut, action] of items) {
      const item = document.createElement("div");
      item.className = "context-menu-item";
      const labelSpan = document.createElement("span");
      labelSpan.textContent = label;
      item.appendChild(labelSpan);
      if (shortcut) {
        const sc = document.createElement("span");
        sc.className = "context-menu-shortcut";
        sc.textContent = shortcut;
        item.appendChild(sc);
      }
      item.addEventListener("click", () => {
        menu!.classList.add("hidden");
        action();
      });
      menu.appendChild(item);
    }

    const maxX = window.innerWidth - 200;
    const maxY = window.innerHeight - 100;
    menu.style.left = `${Math.min(x, maxX)}px`;
    menu.style.top = `${Math.min(y, maxY)}px`;
    menu.classList.remove("hidden");

    const dismiss = (e: MouseEvent) => {
      if (!menu!.contains(e.target as Node)) {
        menu!.classList.add("hidden");
        document.removeEventListener("mousedown", dismiss, true);
      }
    };
    setTimeout(() => document.addEventListener("mousedown", dismiss, true), 0);
  }
}
