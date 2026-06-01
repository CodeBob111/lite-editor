import { gitClone } from "./tauri-api";

const DEFAULT_BASE = "/Users/baobingjiang/IdeaProjects/";

let overlay: HTMLElement;
let urlInput: HTMLInputElement;
let dirInput: HTMLInputElement;
let cloneBtn: HTMLButtonElement;
let errorEl: HTMLElement;
let progressEl: HTMLElement;
let closeBtn: HTMLElement;
let onCloned: ((dir: string) => void) | null = null;

function extractRepoName(url: string): string {
  const cleaned = url.replace(/\/+$/, "").replace(/\.git$/, "");
  const last = cleaned.split("/").pop() || "";
  return last;
}

function init() {
  overlay = document.getElementById("vcs-clone-overlay")!;
  urlInput = document.getElementById("vcs-clone-url") as HTMLInputElement;
  dirInput = document.getElementById("vcs-clone-dir") as HTMLInputElement;
  cloneBtn = document.getElementById("vcs-clone-btn") as HTMLButtonElement;
  errorEl = document.getElementById("vcs-clone-error")!;
  progressEl = document.getElementById("vcs-clone-progress")!;
  closeBtn = document.getElementById("vcs-clone-close")!;

  dirInput.value = DEFAULT_BASE;

  urlInput.addEventListener("input", () => {
    const name = extractRepoName(urlInput.value.trim());
    dirInput.value = DEFAULT_BASE + name;
    errorEl.classList.add("hidden");
  });

  cloneBtn.addEventListener("click", doClone);

  closeBtn.addEventListener("click", hide);

  overlay.addEventListener("click", (e) => {
    if (e.target === overlay) hide();
  });

  urlInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") doClone();
    if (e.key === "Escape") hide();
  });

  dirInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") doClone();
    if (e.key === "Escape") hide();
  });
}

async function doClone() {
  const url = urlInput.value.trim();
  const dir = dirInput.value.trim();
  if (!url) {
    showError("Please enter a repository URL");
    return;
  }
  if (!dir) {
    showError("Please enter a directory");
    return;
  }
  errorEl.classList.add("hidden");
  progressEl.classList.remove("hidden");
  cloneBtn.disabled = true;

  try {
    await gitClone(url, dir);
    progressEl.classList.add("hidden");
    hide();
    if (onCloned) onCloned(dir);
  } catch (e: any) {
    progressEl.classList.add("hidden");
    showError(String(e));
    cloneBtn.disabled = false;
  }
}

function showError(msg: string) {
  errorEl.textContent = msg;
  errorEl.classList.remove("hidden");
}

export function showVcsClone(callback: (dir: string) => void) {
  if (!overlay) init();
  onCloned = callback;
  urlInput.value = "";
  dirInput.value = DEFAULT_BASE;
  errorEl.classList.add("hidden");
  progressEl.classList.add("hidden");
  cloneBtn.disabled = false;
  overlay.classList.remove("hidden");
  urlInput.focus();
}

function hide() {
  overlay.classList.add("hidden");
  onCloned = null;
}

export function hideVcsClone() {
  hide();
}

export function isVcsCloneVisible(): boolean {
  return overlay ? !overlay.classList.contains("hidden") : false;
}
