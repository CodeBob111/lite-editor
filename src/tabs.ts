interface OpenTab {
  path: string;
  name: string;
  content: string;
  dirty: boolean;
}

export class TabManager {
  private container: HTMLElement;
  private onTabActivate: (path: string, content: string) => void;
  private tabs: OpenTab[] = [];
  private activeIndex: number = -1;

  constructor(
    container: HTMLElement,
    onTabActivate: (path: string, content: string) => void
  ) {
    this.container = container;
    this.onTabActivate = onTabActivate;
  }

  openFile(path: string, content?: string) {
    const existing = this.tabs.findIndex((t) => t.path === path);
    if (existing >= 0) {
      if (content !== undefined) {
        this.tabs[existing].content = content;
      }
      this.activateTab(existing);
      return;
    }

    const name = path.split("/").pop() || path;
    const fileContent = content ?? "";

    this.tabs.push({ path, name, content: fileContent, dirty: false });
    this.activateTab(this.tabs.length - 1);
  }

  markSaved(path: string) {
    const tab = this.tabs.find((t) => t.path === path);
    if (tab) {
      tab.dirty = false;
      this.render();
    }
  }

  markDirty(path: string) {
    const tab = this.tabs.find((t) => t.path === path);
    if (tab && !tab.dirty) {
      tab.dirty = true;
      this.render();
    }
  }

  closeTab(index: number) {
    this.tabs.splice(index, 1);
    if (this.activeIndex >= this.tabs.length) {
      this.activeIndex = this.tabs.length - 1;
    }
    if (this.activeIndex >= 0) {
      this.activateTab(this.activeIndex);
    } else {
      this.render();
    }
  }

  getActiveFile(): string | null {
    if (this.activeIndex >= 0 && this.activeIndex < this.tabs.length) {
      return this.tabs[this.activeIndex].path;
    }
    return null;
  }

  private activateTab(index: number) {
    this.activeIndex = index;
    this.render();
    const tab = this.tabs[index];
    if (tab) {
      this.onTabActivate(tab.path, tab.content);
    }
  }

  private render() {
    this.container.innerHTML = "";
    this.tabs.forEach((tab, i) => {
      const el = document.createElement("div");
      el.className = `tab${i === this.activeIndex ? " active" : ""}`;

      const label = document.createElement("span");
      label.textContent = tab.dirty ? `● ${tab.name}` : tab.name;
      el.appendChild(label);

      const close = document.createElement("span");
      close.className = "tab-close";
      close.textContent = "×";
      close.addEventListener("click", (e) => {
        e.stopPropagation();
        this.closeTab(i);
      });
      el.appendChild(close);

      el.addEventListener("click", () => this.activateTab(i));
      this.container.appendChild(el);
    });
  }
}
