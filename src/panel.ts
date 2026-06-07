export class PanelManager {
  private tabsContainer: HTMLElement;
  private contentContainer: HTMLElement;
  private mavenLines: string[] = [];
  private readonly MAX_MAVEN_LINES = 5000;
  private mavenOutputFrame: number | null = null;
  private onSwitchCallbacks: Map<string, () => void> = new Map();
  private onLeaveCallbacks: Map<string, () => void> = new Map();
  private activePanelId: string | null = null;
  // Maven 面板已移到左侧栏(不再是底部 tab);构建开始时用它来聚焦左侧 Maven 视图。
  private mavenFocusHandler: (() => void) | null = null;

  constructor(tabsContainer: HTMLElement, contentContainer: HTMLElement) {
    this.tabsContainer = tabsContainer;
    this.contentContainer = contentContainer;
    this.activePanelId = this.tabsContainer.querySelector<HTMLElement>(".panel-tab.active")?.dataset.panel ?? null;
    this.bindTabs();
  }

  showMavenModules(modules: { name: string; groupId: string; artifactId: string }[]) {
    const tree = document.getElementById("maven-tree")!;
    tree.innerHTML = "";

    if (modules.length === 0) {
      tree.innerHTML = '<div class="panel-placeholder">No Maven modules found</div>';
      return;
    }

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

  appendMavenLine(line: string) {
    this.mavenLines.push(line);
    if (this.mavenLines.length > this.MAX_MAVEN_LINES) {
      this.mavenLines = this.mavenLines.slice(-this.MAX_MAVEN_LINES);
    }
    this.scheduleMavenOutputRender();
  }

  private scheduleMavenOutputRender() {
    if (this.mavenOutputFrame !== null) return;
    this.mavenOutputFrame = requestAnimationFrame(() => {
      this.mavenOutputFrame = null;
      this.renderMavenOutput();
    });
  }

  private renderMavenOutput() {
    const outputEl = document.getElementById("maven-output")!;
    outputEl.textContent = this.mavenLines.join("\n");
    outputEl.scrollTop = outputEl.scrollHeight;
  }

  setMavenFocusHandler(fn: () => void) {
    this.mavenFocusHandler = fn;
  }

  clearMavenOutput(header: string) {
    this.mavenFocusHandler?.();
    this.mavenLines = [header];
    this.renderMavenOutput();
  }

  onSwitch(panelId: string, callback: () => void) {
    this.onSwitchCallbacks.set(panelId, callback);
  }

  onLeave(panelId: string, callback: () => void) {
    this.onLeaveCallbacks.set(panelId, callback);
  }

  getActivePanel(): string | null {
    return this.activePanelId;
  }

  switchTo(panelId: string) {
    if (this.activePanelId === panelId) return;

    if (this.activePanelId) {
      this.onLeaveCallbacks.get(this.activePanelId)?.();
    }

    this.activePanelId = panelId;
    this.tabsContainer.querySelectorAll(".panel-tab").forEach((tab) => {
      tab.classList.toggle("active", (tab as HTMLElement).dataset.panel === panelId);
    });
    this.contentContainer.querySelectorAll(".panel-view").forEach((view) => {
      view.classList.toggle("active", view.id === `${panelId}-panel`);
    });
    const cb = this.onSwitchCallbacks.get(panelId);
    if (cb) cb();
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
