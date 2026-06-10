import { EditorView, Decoration, type DecorationSet, WidgetType } from "@codemirror/view";
import { EditorState, StateEffect, StateField, RangeSetBuilder, type Extension, Compartment } from "@codemirror/state";
import { warmEarthTheme } from "./editor-theme";
import { showStatus } from "./utils";
import { appConfirm } from "./dialog";
import {
  gitMergeConflicts, gitShowConflictVersion, gitMergeAbort,
  gitResolveConflictFile, gitCheckoutConflictSide,
  parseConflictFile, type ConflictChunk,
} from "./tauri-api";

interface MergeFileState {
  relPath: string;
  resolved: boolean;
  resolution?: "ours" | "theirs" | "merged";
}

let activeRepo = "";
let activeBranch = "";
let conflictFiles: MergeFileState[] = [];

let mergeOursView: EditorView | null = null;
let mergeTheirsView: EditorView | null = null;
let mergeResultView: EditorView | null = null;
let mergeScrollCleanup: (() => void) | null = null;
let activeConflicts: ConflictChunk[] = [];
let activeMergeFile = "";

const langCompartment = new Compartment();
const langCache = new Map<string, Promise<Extension>>();

// ---- Result-pane chunk position tracking (auto-maps through edits) ----

interface ChunkPos {
  from: number;
  to: number;
  idx: number;
  oursText: string;
  theirsText: string;
  resolved: boolean;
}

const initChunksEffect = StateEffect.define<ChunkPos[]>();
const resolveChunkEffect = StateEffect.define<number>();

const chunkPositionsField = StateField.define<ChunkPos[]>({
  create() { return []; },
  update(chunks, tr) {
    for (const e of tr.effects) {
      if (e.is(initChunksEffect)) return e.value;
      if (e.is(resolveChunkEffect)) {
        chunks = chunks.map(c => c.idx === e.value ? { ...c, resolved: true } : c);
      }
    }
    if (!tr.docChanged) return chunks;
    return chunks.map(c => {
      if (c.resolved) return c;
      return { ...c, from: tr.changes.mapPos(c.from), to: tr.changes.mapPos(c.to, 1) };
    });
  },
});

const chunkDecorations = EditorView.decorations.compute([chunkPositionsField], (state) => {
  const chunks = state.field(chunkPositionsField);
  const doc = state.doc;
  const lineFroms = new Set<number>();

  for (const chunk of chunks) {
    if (chunk.resolved) continue;
    const from = Math.min(chunk.from, doc.length);
    const to = Math.min(chunk.to, doc.length);
    if (from === to && doc.length > 0) continue;
    const startLine = doc.lineAt(from).number;
    const endLine = doc.lineAt(to).number;
    for (let ln = startLine; ln <= endLine; ln++) {
      lineFroms.add(doc.line(ln).from);
    }
  }

  const sorted = [...lineFroms].sort((a, b) => a - b);
  const builder = new RangeSetBuilder<Decoration>();
  for (const f of sorted) builder.add(f, f, conflictLineDeco);
  return builder.finish();
});

function loadLang(filename: string): Promise<Extension> | null {
  const ext = filename.split(".").pop()?.toLowerCase();
  if (!ext) return null;
  const key = ext === "tsx" ? "typescript" : ext === "jsx" ? "javascript" : ext;
  if (!["java", "python", "typescript", "javascript", "ts", "js"].includes(key)) return null;
  const normalKey = key === "ts" ? "typescript" : key === "js" ? "javascript" : key;

  const cached = langCache.get(normalKey);
  if (cached) return cached;

  const promise = (async (): Promise<Extension> => {
    if (normalKey === "java") return (await import("@codemirror/lang-java")).java();
    if (normalKey === "python") return (await import("@codemirror/lang-python")).python();
    const mod = await import("@codemirror/lang-javascript");
    return normalKey === "typescript"
      ? mod.javascript({ typescript: true, jsx: true })
      : mod.javascript({ jsx: true });
  })();

  langCache.set(normalKey, promise);
  return promise;
}

// ---- Conflict highlight decorations ----

const oursLineDeco = Decoration.line({ class: "merge-ours-line" });
const theirsLineDeco = Decoration.line({ class: "merge-theirs-line" });
const conflictLineDeco = Decoration.line({ class: "merge-conflict-line" });

// ---- Accept button widgets ----

class AcceptButtonWidget extends WidgetType {
  constructor(
    private side: "ours" | "theirs",
    private chunkIndex: number,
  ) { super(); }

  toDOM(): HTMLElement {
    const btn = document.createElement("button");
    btn.className = `merge-accept-btn merge-accept-${this.side}`;
    btn.textContent = this.side === "ours" ? ">>" : "<<";
    btn.title = this.side === "ours" ? "Accept Yours" : "Accept Theirs";
    btn.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      acceptChunk(this.chunkIndex, this.side);
    });
    return btn;
  }

  eq(other: AcceptButtonWidget): boolean {
    return this.side === other.side && this.chunkIndex === other.chunkIndex;
  }
}

// ---- Conflicts Dialog ----

export async function showConflictsDialog(repoPath: string, branch: string) {
  activeRepo = repoPath;
  activeBranch = branch;

  const overlay = document.getElementById("merge-conflicts-overlay")!;
  const title = document.getElementById("merge-conflicts-title")!;
  const applyBtn = document.getElementById("merge-conflicts-apply")! as HTMLButtonElement;

  const repoName = repoPath.split("/").pop() || repoPath;
  title.textContent = `Merge Conflicts — ${repoName}`;

  try {
    const files = await gitMergeConflicts(repoPath);
    conflictFiles = files.map((f) => ({ relPath: f, resolved: false }));
  } catch (err) {
    showStatus(`Failed to get conflicts: ${err}`, true);
    return;
  }

  renderConflictsList();
  applyBtn.disabled = true;
  overlay.classList.remove("hidden");
}

function renderConflictsList() {
  const list = document.getElementById("merge-conflicts-list")!;
  const applyBtn = document.getElementById("merge-conflicts-apply")! as HTMLButtonElement;

  list.textContent = "";
  for (let i = 0; i < conflictFiles.length; i++) {
    const f = conflictFiles[i];
    const statusClass = f.resolved ? "resolved" : "unresolved";
    const statusText = f.resolved
      ? (f.resolution === "ours" ? "Yours" : f.resolution === "theirs" ? "Theirs" : "Merged")
      : "Unresolved";

    const row = document.createElement("div");
    row.className = "merge-conflict-row";

    const icon = document.createElement("span");
    icon.className = "merge-conflict-icon";
    icon.textContent = "!";
    row.appendChild(icon);

    const name = document.createElement("span");
    name.className = "merge-conflict-name";
    name.title = f.relPath;
    name.textContent = f.relPath;
    row.appendChild(name);

    const status = document.createElement("span");
    status.className = `merge-conflict-status ${statusClass}`;
    status.textContent = statusText;
    row.appendChild(status);

    const actions = document.createElement("div");
    actions.className = "merge-conflict-actions";

    const btnYours = document.createElement("button");
    btnYours.className = "merge-conflict-btn";
    btnYours.textContent = "Accept Yours";
    btnYours.dataset.action = "accept-yours";
    btnYours.dataset.index = String(i);
    actions.appendChild(btnYours);

    const btnTheirs = document.createElement("button");
    btnTheirs.className = "merge-conflict-btn";
    btnTheirs.textContent = "Accept Theirs";
    btnTheirs.dataset.action = "accept-theirs";
    btnTheirs.dataset.index = String(i);
    actions.appendChild(btnTheirs);

    const btnMerge = document.createElement("button");
    btnMerge.className = "merge-conflict-btn merge-btn";
    btnMerge.textContent = "Merge...";
    btnMerge.dataset.action = "merge";
    btnMerge.dataset.index = String(i);
    actions.appendChild(btnMerge);

    row.appendChild(actions);
    list.appendChild(row);
  }

  const allResolved = conflictFiles.length > 0 && conflictFiles.every((f) => f.resolved);
  applyBtn.disabled = !allResolved;
}

function hideConflictsDialog() {
  document.getElementById("merge-conflicts-overlay")!.classList.add("hidden");
}

async function handleConflictAction(action: string, index: number) {
  const file = conflictFiles[index];
  if (!file) return;

  if (action === "accept-yours") {
    try {
      await gitCheckoutConflictSide(activeRepo, file.relPath, "ours");
      file.resolved = true;
      file.resolution = "ours";
      renderConflictsList();
      showStatus(`Resolved ${file.relPath} → Yours`);
    } catch (err) {
      showStatus(`Failed: ${err}`, true);
    }
  } else if (action === "accept-theirs") {
    try {
      await gitCheckoutConflictSide(activeRepo, file.relPath, "theirs");
      file.resolved = true;
      file.resolution = "theirs";
      renderConflictsList();
      showStatus(`Resolved ${file.relPath} → Theirs`);
    } catch (err) {
      showStatus(`Failed: ${err}`, true);
    }
  } else if (action === "merge") {
    openMergeEditor(file.relPath);
  }
}

// ---- 3-Way Merge Editor ----

async function openMergeEditor(relPath: string) {
  activeMergeFile = relPath;
  const overlay = document.getElementById("merge-editor-overlay")!;
  const titleEl = document.getElementById("merge-editor-title")!;

  const repoName = activeRepo.split("/").pop() || activeRepo;
  titleEl.textContent = `${repoName}: ${relPath}`;

  try {
    const absPath = activeRepo + "/" + relPath;
    // 冲突标记解析在 Rust 侧完成(传 path,省去全文两趟 IPC)
    const parsed = await parseConflictFile(absPath);
    activeConflicts = parsed.chunks;

    let oursText: string;
    let theirsText: string;

    if (parsed.chunks.length > 0) {
      oursText = parsed.ours;
      theirsText = parsed.theirs;
    } else {
      oursText = await gitShowConflictVersion(activeRepo, relPath, 2);
      theirsText = await gitShowConflictVersion(activeRepo, relPath, 3);
    }

    destroyMergeEditors();

    const readonlyExts: Extension[] = [
      EditorState.readOnly.of(true),
      ...warmEarthTheme,
      EditorView.lineWrapping,
      langCompartment.of([]),
    ];

    mergeOursView = new EditorView({
      state: EditorState.create({ doc: oursText, extensions: readonlyExts }),
      parent: document.getElementById("merge-editor-ours")!,
    });

    mergeTheirsView = new EditorView({
      state: EditorState.create({ doc: theirsText, extensions: readonlyExts }),
      parent: document.getElementById("merge-editor-theirs")!,
    });

    mergeResultView = new EditorView({
      state: EditorState.create({
        doc: oursText,
        extensions: [
          ...warmEarthTheme,
          EditorView.lineWrapping,
          langCompartment.of([]),
          chunkPositionsField,
          chunkDecorations,
          EditorView.updateListener.of((update) => {
            if (update.state.field(chunkPositionsField) !== update.startState.field(chunkPositionsField)) {
              updateConflictCounter();
            }
          }),
        ],
      }),
      parent: document.getElementById("merge-editor-result")!,
    });

    const resultDoc = mergeResultView.state.doc;
    const initialChunks: ChunkPos[] = parsed.chunks.map((c, i) => {
      // EOF 处的空 ours 块(删除 vs 新增、无尾随换行)在 ours 文本里没有对应行,
      // 直接取行号会越界 RangeError;表达为文档末尾的零宽插入点,accept theirs
      // 时补上分隔换行——绝不能夹到末行整行区间,否则 accept 会吃掉最后一行
      if (c.ours_start + 1 > resultDoc.lines) {
        return {
          from: resultDoc.length, to: resultDoc.length, idx: i,
          oursText: "",
          theirsText: c.theirs_text ? `\n${c.theirs_text}` : "",
          resolved: false,
        };
      }
      const startLine = c.ours_start + 1;
      const endLine = Math.min(c.ours_end, resultDoc.lines);
      const from = resultDoc.line(startLine).from;
      const to = endLine >= startLine ? resultDoc.line(endLine).to : from;
      return { from, to, idx: i, oursText: c.ours_text, theirsText: c.theirs_text, resolved: false };
    });
    mergeResultView.dispatch({ effects: initChunksEffect.of(initialChunks) });

    applyLangToMergeEditors(relPath);
    applyConflictHighlights();
    mergeScrollCleanup = syncMergeScroll();
    updateConflictCounter();

    overlay.classList.remove("hidden");
  } catch (err) {
    showStatus(`Failed to open merge editor: ${err}`, true);
  }
}

function applyLangToMergeEditors(filename: string) {
  const promise = loadLang(filename);
  if (!promise) return;
  promise.then((ext) => {
    for (const view of [mergeOursView, mergeTheirsView, mergeResultView]) {
      if (view) view.dispatch({ effects: langCompartment.reconfigure(ext) });
    }
  }).catch(() => {});
}

function applyConflictHighlights() {
  if (!mergeOursView || !mergeTheirsView) return;

  const oursRanges: [number, number][] = activeConflicts.map((c) => [c.ours_start, c.ours_end]);
  const theirsRanges: [number, number][] = activeConflicts.map((c) => [c.theirs_start, c.theirs_end]);

  applyHighlightsToView(mergeOursView, oursRanges, "ours");
  applyHighlightsToView(mergeTheirsView, theirsRanges, "theirs");
}

function applyHighlightsToView(view: EditorView, ranges: [number, number][], type: "ours" | "theirs" | "conflict") {
  const deco = type === "ours" ? oursLineDeco : type === "theirs" ? theirsLineDeco : conflictLineDeco;
  const doc = view.state.doc;
  const decoList: { from: number; deco: Decoration }[] = [];

  for (let ri = 0; ri < ranges.length; ri++) {
    const [startLine, endLine] = ranges[ri];
    for (let ln = startLine + 1; ln <= Math.min(endLine, doc.lines); ln++) {
      const line = doc.line(ln);
      decoList.push({ from: line.from, deco });
    }

    if (type === "ours" && startLine < doc.lines) {
      const line = doc.line(startLine + 1);
      const widget = Decoration.widget({
        widget: new AcceptButtonWidget("ours", ri),
        side: -1,
      });
      decoList.push({ from: line.from, deco: widget });
    } else if (type === "theirs" && startLine < doc.lines) {
      const line = doc.line(startLine + 1);
      const widget = Decoration.widget({
        widget: new AcceptButtonWidget("theirs", ri),
        side: -1,
      });
      decoList.push({ from: line.from, deco: widget });
    }
  }

  decoList.sort((a, b) => a.from - b.from || a.deco.startSide - b.deco.startSide);
  const builder = new RangeSetBuilder<Decoration>();
  for (const d of decoList) builder.add(d.from, d.from, d.deco);
  const decoSet = builder.finish();

  view.dispatch({
    effects: StateEffect.appendConfig.of(
      EditorView.decorations.of(decoSet),
    ),
  });
}

function acceptChunk(chunkIndex: number, side: "ours" | "theirs") {
  if (!mergeResultView) return;
  const chunks = mergeResultView.state.field(chunkPositionsField);
  const chunk = chunks.find(c => c.idx === chunkIndex && !c.resolved);
  if (!chunk) return;

  const doc = mergeResultView.state.doc;
  const from = Math.min(chunk.from, doc.length);
  const to = Math.min(chunk.to, doc.length);
  const newText = side === "ours" ? chunk.oursText : chunk.theirsText;

  mergeResultView.dispatch({
    changes: { from, to, insert: newText },
    effects: resolveChunkEffect.of(chunkIndex),
  });
}

function closeMergeEditor() {
  document.getElementById("merge-editor-overlay")!.classList.add("hidden");
  destroyMergeEditors();
}

function destroyMergeEditors() {
  mergeScrollCleanup?.();
  mergeScrollCleanup = null;
  if (mergeOursView) { mergeOursView.destroy(); mergeOursView = null; }
  if (mergeTheirsView) { mergeTheirsView.destroy(); mergeTheirsView = null; }
  if (mergeResultView) { mergeResultView.destroy(); mergeResultView = null; }
  document.getElementById("merge-editor-ours")!.textContent = "";
  document.getElementById("merge-editor-theirs")!.textContent = "";
  document.getElementById("merge-editor-result")!.textContent = "";
  activeConflicts = [];
}

function syncMergeScroll(): () => void {
  if (!mergeOursView || !mergeTheirsView || !mergeResultView) return () => {};
  const views = [mergeOursView, mergeTheirsView, mergeResultView];
  let syncing = false;

  const handlers: [HTMLElement, EventListener][] = [];
  for (const src of views) {
    const handler = () => {
      if (syncing) return;
      syncing = true;
      for (const tgt of views) {
        if (tgt !== src) {
          tgt.scrollDOM.scrollTop = src.scrollDOM.scrollTop;
          tgt.scrollDOM.scrollLeft = src.scrollDOM.scrollLeft;
        }
      }
      requestAnimationFrame(() => { syncing = false; });
    };
    src.scrollDOM.addEventListener("scroll", handler, { passive: true });
    handlers.push([src.scrollDOM, handler as EventListener]);
  }

  return () => {
    for (const [el, handler] of handlers) {
      el.removeEventListener("scroll", handler);
    }
  };
}

async function saveMergeResult() {
  if (!mergeResultView || !activeMergeFile) return;
  const content = mergeResultView.state.doc.toString();
  try {
    await gitResolveConflictFile(activeRepo, activeMergeFile, content);
    const file = conflictFiles.find((f) => f.relPath === activeMergeFile);
    if (file) {
      file.resolved = true;
      file.resolution = "merged";
    }
    closeMergeEditor();
    renderConflictsList();
    showStatus(`Resolved ${activeMergeFile} → Merged`);
  } catch (err) {
    showStatus(`Failed to save: ${err}`, true);
  }
}

function updateConflictCounter() {
  const counter = document.getElementById("merge-editor-counter")!;
  if (mergeResultView) {
    const unresolved = mergeResultView.state.field(chunkPositionsField).filter(c => !c.resolved).length;
    counter.textContent = `${unresolved} conflict${unresolved !== 1 ? "s" : ""}`;
  } else {
    counter.textContent = `${activeConflicts.length} conflict${activeConflicts.length !== 1 ? "s" : ""}`;
  }
}

function navigateConflict(direction: 1 | -1) {
  if (!mergeResultView) return;
  const chunks = mergeResultView.state.field(chunkPositionsField).filter(c => !c.resolved);
  if (chunks.length === 0) return;

  const cursorPos = mergeResultView.state.selection.main.head;

  let target: ChunkPos | null = null;
  if (direction === 1) {
    target = chunks.find(c => c.from > cursorPos) ?? chunks[0];
  } else {
    for (let i = chunks.length - 1; i >= 0; i--) {
      if (chunks[i].from < cursorPos) { target = chunks[i]; break; }
    }
    if (!target) target = chunks[chunks.length - 1];
  }

  if (target) {
    const from = Math.min(target.from, mergeResultView.state.doc.length);
    mergeResultView.dispatch({
      selection: { anchor: from },
      effects: EditorView.scrollIntoView(from, { y: "center" }),
    });
    const origChunk = activeConflicts[target.idx];
    if (origChunk && mergeOursView) {
      const oursDoc = mergeOursView.state.doc;
      const oursLn = Math.min(origChunk.ours_start + 1, oursDoc.lines);
      mergeOursView.dispatch({
        effects: EditorView.scrollIntoView(oursDoc.line(oursLn).from, { y: "center" }),
      });
    }
    if (origChunk && mergeTheirsView) {
      const theirsDoc = mergeTheirsView.state.doc;
      const theirsLn = Math.min(origChunk.theirs_start + 1, theirsDoc.lines);
      mergeTheirsView.dispatch({
        effects: EditorView.scrollIntoView(theirsDoc.line(theirsLn).from, { y: "center" }),
      });
    }
  }
}

// ---- Event wiring ----

export function initMergeConflict() {
  document.getElementById("merge-conflicts-list")!.addEventListener("click", (e) => {
    const btn = (e.target as HTMLElement).closest("[data-action]") as HTMLElement | null;
    if (!btn) return;
    const action = btn.dataset.action!;
    const index = Number(btn.dataset.index);
    handleConflictAction(action, index);
  });

  document.getElementById("merge-conflicts-close")!.addEventListener("click", hideConflictsDialog);

  document.getElementById("merge-conflicts-abort")!.addEventListener("click", async () => {
    if (await appConfirm("Abort merge? All conflict resolutions will be lost.")) {
      try {
        await gitMergeAbort(activeRepo);
        showStatus("Merge aborted");
        hideConflictsDialog();
      } catch (err) {
        showStatus(`Abort failed: ${err}`, true);
      }
    }
  });

  document.getElementById("merge-conflicts-apply")!.addEventListener("click", () => {
    hideConflictsDialog();
    showStatus("All conflicts resolved. Commit to complete the merge.");
  });

  document.getElementById("merge-editor-save")!.addEventListener("click", saveMergeResult);
  document.getElementById("merge-editor-cancel")!.addEventListener("click", closeMergeEditor);
  document.getElementById("merge-editor-prev")!.addEventListener("click", () => navigateConflict(-1));
  document.getElementById("merge-editor-next")!.addEventListener("click", () => navigateConflict(1));

  document.getElementById("merge-conflicts-overlay")!.addEventListener("click", (e) => {
    if (e.target === e.currentTarget) hideConflictsDialog();
  });

  document.addEventListener("keydown", (e) => {
    const mergeEditorVisible = !document.getElementById("merge-editor-overlay")!.classList.contains("hidden");
    if (mergeEditorVisible && e.key === "Escape") {
      e.preventDefault();
      closeMergeEditor();
      return;
    }
    const conflictsVisible = !document.getElementById("merge-conflicts-overlay")!.classList.contains("hidden");
    if (conflictsVisible && e.key === "Escape") {
      e.preventDefault();
      hideConflictsDialog();
    }
  });
}
