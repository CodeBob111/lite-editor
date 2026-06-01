import { app } from "./state";
import { abbreviatePath } from "./utils";

interface RecentProject {
  path: string;
  name: string;
  openedAt: number;
}

const RECENT_PROJECTS_KEY = "lite-editor:recent-projects";
const RECENT_PROJECTS_MAX = 12;

let _onOpen: (path: string) => void = () => {};

export function initRecentProjects(onOpen: (path: string) => void) {
  _onOpen = onOpen;
}

function projectNameFromPath(path: string) {
  return path.split("/").filter(Boolean).pop() || path;
}

function loadRecentProjects(): RecentProject[] {
  try {
    const raw = localStorage.getItem(RECENT_PROJECTS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];

    const seen = new Set<string>();
    const projects: RecentProject[] = [];
    for (const item of parsed) {
      if (!item || typeof item.path !== "string" || seen.has(item.path)) continue;
      seen.add(item.path);
      projects.push({
        path: item.path,
        name: typeof item.name === "string" ? item.name : projectNameFromPath(item.path),
        openedAt: typeof item.openedAt === "number" ? item.openedAt : 0,
      });
    }
    return projects.sort((a, b) => b.openedAt - a.openedAt).slice(0, RECENT_PROJECTS_MAX);
  } catch {
    return [];
  }
}

function saveRecentProjects(projects: RecentProject[]) {
  localStorage.setItem(RECENT_PROJECTS_KEY, JSON.stringify(projects.slice(0, RECENT_PROJECTS_MAX)));
}

export function rememberRecentProject(path: string) {
  const next = loadRecentProjects().filter((project) => project.path !== path);
  next.unshift({ path, name: projectNameFromPath(path), openedAt: Date.now() });
  saveRecentProjects(next);
}

export function removeRecentProject(path: string) {
  saveRecentProjects(loadRecentProjects().filter((project) => project.path !== path));
  renderRecentProjects();
}

export function hideRecentProjects() {
  document.getElementById("recent-projects-overlay")?.classList.add("hidden");
}

export function showRecentProjects() {
  renderRecentProjects();
  document.getElementById("recent-projects-overlay")?.classList.remove("hidden");
}

export function renderRecentProjects() {
  const listEl = document.getElementById("recent-projects-list");
  if (!listEl) return;

  const projects = loadRecentProjects();
  listEl.innerHTML = "";
  if (projects.length === 0) {
    const empty = document.createElement("div");
    empty.className = "recent-projects-empty";
    empty.textContent = "No recent projects";
    listEl.appendChild(empty);
    return;
  }

  const currentPaths = new Set(app.projects.map((project) => project.path));
  for (const project of projects) {
    const item = document.createElement("button");
    item.className = "recent-project-item";
    item.type = "button";

    const meta = document.createElement("span");
    meta.className = "recent-project-meta";

    const name = document.createElement("span");
    name.className = "recent-project-name";
    name.textContent = project.name;
    meta.appendChild(name);

    const path = document.createElement("span");
    path.className = "recent-project-path";
    path.textContent = abbreviatePath(project.path);
    meta.appendChild(path);

    const state = document.createElement("span");
    state.className = "recent-project-state";
    state.textContent = currentPaths.has(project.path) ? "Open" : "";

    const remove = document.createElement("span");
    remove.className = "recent-project-remove";
    remove.textContent = "×";
    remove.title = "Remove";
    remove.addEventListener("click", (event) => {
      event.stopPropagation();
      removeRecentProject(project.path);
    });

    item.append(meta, state, remove);
    item.addEventListener("click", () => {
      hideRecentProjects();
      _onOpen(project.path);
    });
    listEl.appendChild(item);
  }
}
