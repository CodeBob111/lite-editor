import { EditorView, gutter, GutterMarker } from "@codemirror/view";
import { Compartment, StateField, StateEffect } from "@codemirror/state";
import { gitBlame, type BlameLine } from "./tauri-api";

interface BlameState {
  lines: BlameLine[];
  minTs: number;
  maxTs: number;
}

const setBlameData = StateEffect.define<BlameState | null>();

const blameField = StateField.define<BlameState | null>({
  create: () => null,
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setBlameData)) return e.value;
    }
    return value;
  },
});

function formatDate(ts: number): string {
  if (ts === 0) return "";
  const d = new Date(ts * 1000);
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const target = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diff = today.getTime() - target.getTime();
  const days = diff / 86400000;
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}

function heatColor(heat: number): string {
  const r = Math.round(40 + heat * 30);
  const g = Math.round(45 + heat * 40);
  const b = Math.round(70 + heat * 110);
  const a = 0.15 + heat * 0.55;
  return `rgba(${r},${g},${b},${a})`;
}

function textColor(heat: number): string {
  const v = Math.round(120 + heat * 100);
  return `rgb(${v},${Math.round(v * 0.95)},${Math.round(v * 1.1)})`;
}

class BlameMarker extends GutterMarker {
  constructor(
    readonly text: string,
    readonly heat: number,
  ) {
    super();
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = "cm-blame-info";
    el.style.backgroundColor = heatColor(this.heat);
    el.style.color = textColor(this.heat);
    el.textContent = this.text;
    return el;
  }
}

const blameGutter = gutter({
  class: "cm-blame-gutter",
  lineMarker(view, line) {
    const state = view.state.field(blameField);
    if (!state) return null;
    const lineNo = view.state.doc.lineAt(line.from).number;
    const info = state.lines[lineNo - 1];
    if (!info) return null;
    const date = formatDate(info.timestamp);
    const author = info.author.length > 10 ? info.author.slice(0, 10) : info.author;
    const range = state.maxTs - state.minTs;
    const heat = range > 0 ? (info.timestamp - state.minTs) / range : 0.5;
    return new BlameMarker(`${date}  ${author}`, heat);
  },
  lineMarkerChange(update) {
    return update.transactions.some((t) =>
      t.effects.some((e) => e.is(setBlameData))
    );
  },
});

const blameTheme = EditorView.theme({
  ".cm-blame-gutter": {
    backgroundColor: "#111111",
    borderRight: "1px solid #2a2a2a",
  },
  ".cm-blame-gutter .cm-gutterElement": {
    minWidth: "160px",
    padding: "0",
  },
  ".cm-blame-info": {
    fontSize: "12px",
    fontFamily: "monospace",
    whiteSpace: "nowrap",
    display: "inline-block",
    width: "100%",
    padding: "0 8px",
    boxSizing: "border-box",
  },
});

const blameCompartment = new Compartment();

export const blameExtensions = [blameField, blameCompartment.of([])];

export function isBlameActive(view: EditorView): boolean {
  return view.state.field(blameField) !== null;
}

export async function toggleBlame(
  view: EditorView,
  projectPath: string,
  filePath: string,
) {
  const active = view.state.field(blameField) !== null;
  if (active) {
    view.dispatch({
      effects: [
        setBlameData.of(null),
        blameCompartment.reconfigure([]),
      ],
    });
    return;
  }
  try {
    const lines = await gitBlame(projectPath, filePath);
    let minTs = Infinity, maxTs = -Infinity;
    for (const l of lines) {
      if (l.timestamp > 0) {
        if (l.timestamp < minTs) minTs = l.timestamp;
        if (l.timestamp > maxTs) maxTs = l.timestamp;
      }
    }
    if (minTs === Infinity) { minTs = 0; maxTs = 0; }
    view.dispatch({
      effects: [
        blameCompartment.reconfigure([blameGutter, blameTheme]),
        setBlameData.of({ lines, minTs, maxTs }),
      ],
    });
  } catch {
    // file might not be tracked by git
  }
}

export function clearBlame(view: EditorView) {
  if (view.state.field(blameField) !== null) {
    view.dispatch({
      effects: [
        setBlameData.of(null),
        blameCompartment.reconfigure([]),
      ],
    });
  }
}
