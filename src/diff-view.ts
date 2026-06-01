import { MergeView } from "@codemirror/merge";
import { EditorView } from "@codemirror/view";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import { warmEarthTheme } from "./editor-theme";

let activeDiffView: MergeView | null = null;
let activeDiffCleanup: (() => void) | null = null;
let closeDiffTimer: ReturnType<typeof setTimeout> | null = null;
const diffLanguageCompartment = new Compartment();
const diffLanguageCache = new Map<string, Promise<Extension>>();

function cancelScheduledClose() {
  if (closeDiffTimer !== null) {
    clearTimeout(closeDiffTimer);
    closeDiffTimer = null;
  }
}

function languageKey(filename: string): "java" | "python" | "typescript" | "javascript" | null {
  if (filename.endsWith(".java")) return "java";
  if (filename.endsWith(".py")) return "python";
  if (filename.endsWith(".ts") || filename.endsWith(".tsx")) return "typescript";
  if (filename.endsWith(".js") || filename.endsWith(".jsx")) return "javascript";
  return null;
}

function loadLanguageExtension(filename: string): Promise<Extension> | null {
  const key = languageKey(filename);
  if (!key) return null;

  const cached = diffLanguageCache.get(key);
  if (cached) return cached;

  const promise = (async (): Promise<Extension> => {
    if (key === "java") return (await import("@codemirror/lang-java")).java();
    if (key === "python") return (await import("@codemirror/lang-python")).python();
    const mod = await import("@codemirror/lang-javascript");
    return key === "typescript"
      ? mod.javascript({ typescript: true, jsx: true })
      : mod.javascript({ jsx: true });
  })();

  diffLanguageCache.set(key, promise);
  return promise;
}

export function openDiffView(
  container: HTMLElement,
  original: string,
  modified: string,
  filename: string,
) {
  cancelScheduledClose();
  closeDiffView();
  container.innerHTML = "";

  const originalDoc = normalizeLineEndings(original);
  const modifiedDoc = normalizeLineEndings(modified);

  activeDiffView = new MergeView({
    a: {
      doc: originalDoc,
      extensions: [
        EditorState.readOnly.of(true),
        ...warmEarthTheme,
        diffLanguageCompartment.of([]),
      ],
    },
    b: {
      doc: modifiedDoc,
      extensions: [
        EditorState.readOnly.of(true),
        ...warmEarthTheme,
        diffLanguageCompartment.of([]),
      ],
    },
    parent: container,
    gutter: true,
    highlightChanges: true,
    diffConfig: { scanLimit: 100_000, timeout: 350 },
    collapseUnchanged: { margin: 3, minSize: 4 },
  });

  const view = activeDiffView;
  activeDiffCleanup = syncHorizontalScroll(view);
  resetDiffScroll(view);
  loadLanguageExtension(filename)
    ?.then((extension) => {
      if (activeDiffView !== view) return;
      view.a.dispatch({ effects: diffLanguageCompartment.reconfigure(extension) });
      view.b.dispatch({ effects: diffLanguageCompartment.reconfigure(extension) });
    })
    .catch(() => {});
}

export function closeDiffView() {
  cancelScheduledClose();
  activeDiffCleanup?.();
  activeDiffCleanup = null;
  if (activeDiffView) {
    activeDiffView.destroy();
    activeDiffView = null;
  }
}

export function scheduleCloseDiffView() {
  if (!activeDiffView || closeDiffTimer !== null) return;

  const close = () => {
    closeDiffTimer = null;
    closeDiffView();
  };

  if ("requestIdleCallback" in window) {
    const idleId = (window as any).requestIdleCallback(close, { timeout: 1000 });
    closeDiffTimer = setTimeout(() => {
      (window as any).cancelIdleCallback?.(idleId);
      close();
    }, 1200);
  } else {
    closeDiffTimer = setTimeout(close, 120);
  }
}

export function isDiffViewOpen(): boolean {
  return activeDiffView !== null;
}

function normalizeLineEndings(text: string) {
  return text.replace(/\r\n?/g, "\n");
}

function resetDiffScroll(view: MergeView) {
  requestAnimationFrame(() => {
    for (const editor of [view.a, view.b]) {
      editor.scrollDOM.scrollLeft = 0;
      editor.scrollDOM.scrollTop = 0;
      editor.requestMeasure();
    }
  });
}

function syncHorizontalScroll(view: MergeView) {
  let syncing = false;

  const sync = (source: HTMLElement, target: HTMLElement) => {
    if (syncing) return;
    syncing = true;
    target.scrollLeft = source.scrollLeft;
    requestAnimationFrame(() => {
      syncing = false;
    });
  };

  const onLeftScroll = () => sync(view.a.scrollDOM, view.b.scrollDOM);
  const onRightScroll = () => sync(view.b.scrollDOM, view.a.scrollDOM);

  view.a.scrollDOM.addEventListener("scroll", onLeftScroll, { passive: true });
  view.b.scrollDOM.addEventListener("scroll", onRightScroll, { passive: true });

  return () => {
    view.a.scrollDOM.removeEventListener("scroll", onLeftScroll);
    view.b.scrollDOM.removeEventListener("scroll", onRightScroll);
  };
}
