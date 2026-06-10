import { readFile, renderMarkdown } from "./tauri-api";

// 渲染已迁入 Rust(comrak,GFM 扩展对齐原 marked gfm:true / breaks:false 配置)。
// 真异步 IPC 下两次在飞的渲染可能乱序返回,用单调递增 seq 保证旧结果不覆盖新结果。
let renderSeq = 0;

let previewEl: HTMLElement;
let splitHandle: HTMLElement;
let editorContainer: HTMLElement;
let editorArea: HTMLElement;
let currentPath: string | null = null;
let active = false;
let splitRatio = 0.5;

export function initMdPreview() {
  previewEl = document.getElementById("md-preview")!;
  splitHandle = document.getElementById("md-split-handle")!;
  editorContainer = document.getElementById("editor-container")!;
  editorArea = document.getElementById("editor-area")!;

  splitHandle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    const rect = editorArea.getBoundingClientRect();
    const onMove = (ev: MouseEvent) => {
      const ratio = (ev.clientX - rect.left) / rect.width;
      splitRatio = Math.max(0.15, Math.min(0.85, ratio));
      applySplit();
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  });
}

function applySplit() {
  const pct = splitRatio * 100;
  editorContainer.style.width = `${pct}%`;
  previewEl.style.width = `${100 - pct}%`;
}

export function isMdPreviewActive(): boolean {
  return active;
}

export async function showMdPreview(filePath: string, content?: string) {
  const text = content ?? await readFile(filePath);
  const seq = ++renderSeq;
  const html = await renderMarkdown(text);
  if (seq !== renderSeq) return;
  previewEl.innerHTML = `<div class="md-body">${html}</div>`;

  editorArea.classList.add("md-split-active");
  applySplit();

  currentPath = filePath;
  active = true;
  updatePreviewButton(true);
}

export function hideMdPreview() {
  editorArea.classList.remove("md-split-active");
  editorContainer.style.width = "";
  previewEl.style.width = "";

  currentPath = null;
  active = false;
  updatePreviewButton(false);
}

export function toggleMdPreview(filePath: string, content?: string) {
  if (active && currentPath === filePath) {
    hideMdPreview();
  } else {
    showMdPreview(filePath, content);
  }
}

export async function refreshMdPreview(content: string) {
  if (!active) return;
  const seq = ++renderSeq;
  const html = await renderMarkdown(content);
  if (seq !== renderSeq) return;
  const scrollTop = previewEl.scrollTop;
  previewEl.innerHTML = `<div class="md-body">${html}</div>`;
  previewEl.scrollTop = scrollTop;
}

export function updatePreviewButton(previewActive?: boolean) {
  const btn = document.getElementById("btn-md-preview")!;
  const isActive = previewActive ?? active;
  btn.textContent = isActive ? "✎" : "⬡";
  btn.title = isActive ? "Close Preview" : "Preview Markdown";
}

export function showPreviewButtonForFile(filePath: string | null) {
  const btn = document.getElementById("btn-md-preview")!;
  if (filePath && filePath.endsWith(".md")) {
    btn.classList.remove("hidden");
  } else {
    btn.classList.add("hidden");
    if (active) hideMdPreview();
  }
}
