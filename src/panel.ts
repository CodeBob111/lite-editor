export interface UsageItem {
  file: string;
  line: number;
  text: string;
}

export class PanelManager {
  private tabsContainer: HTMLElement;
  private contentContainer: HTMLElement;
  private activePanel: string = "usages";

  constructor(tabsContainer: HTMLElement, contentContainer: HTMLElement) {
    this.tabsContainer = tabsContainer;
    this.contentContainer = contentContainer;
    this.bindTabs();
  }

  showUsages(symbol: string, usages: UsageItem[], onNavigate: (file: string, line: number) => void) {
    this.switchTo("usages");
    const panel = document.getElementById("usages-panel")!;

    if (usages.length === 0) {
      panel.innerHTML = `<div class="panel-placeholder">No usages found for "${symbol}"</div>`;
      return;
    }

    panel.innerHTML = `<div style="padding: 4px 8px; color: var(--text-muted); font-size: 11px;">${usages.length} usages of <strong>${symbol}</strong></div>`;

    for (const usage of usages) {
      const item = document.createElement("div");
      item.className = "usage-item";
      item.innerHTML = `
        <span class="usage-file">${usage.file.split("/").pop()}</span>
        <span class="usage-line">${usage.line}</span>
        <span class="usage-text">${this.escapeHtml(usage.text)}</span>
      `;
      item.addEventListener("click", () => onNavigate(usage.file, usage.line));
      panel.appendChild(item);
    }
  }

  showMavenModules(modules: { name: string; groupId: string; artifactId: string }[]) {
    const panel = document.getElementById("maven-panel")!;
    const tree = document.getElementById("maven-tree")!;
    tree.innerHTML = "";

    for (const mod of modules) {
      const el = document.createElement("div");
      el.className = "maven-module";
      el.innerHTML = `
        <div class="maven-module-name">
          <span style="color: var(--accent);">m</span>
          <span>${mod.artifactId}</span>
        </div>
        <div class="maven-actions">
          <button class="maven-action" data-cmd="compile" data-module="${mod.artifactId}">compile</button>
          <button class="maven-action" data-cmd="test" data-module="${mod.artifactId}">test</button>
          <button class="maven-action" data-cmd="package" data-module="${mod.artifactId}">package</button>
          <button class="maven-action" data-cmd="install" data-module="${mod.artifactId}">install</button>
        </div>
      `;
      tree.appendChild(el);
    }
  }

  private switchTo(panelId: string) {
    this.activePanel = panelId;
    this.tabsContainer.querySelectorAll(".panel-tab").forEach((tab) => {
      tab.classList.toggle("active", (tab as HTMLElement).dataset.panel === panelId);
    });
    this.contentContainer.querySelectorAll(".panel-view").forEach((view) => {
      view.classList.toggle("active", view.id === `${panelId}-panel`);
    });
  }

  private bindTabs() {
    this.tabsContainer.querySelectorAll(".panel-tab").forEach((tab) => {
      tab.addEventListener("click", () => {
        this.switchTo((tab as HTMLElement).dataset.panel!);
      });
    });
  }

  private escapeHtml(text: string): string {
    const el = document.createElement("span");
    el.textContent = text;
    return el.innerHTML;
  }
}
