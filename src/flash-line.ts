import { StateEffect, StateField } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView } from "@codemirror/view";

const flashEffect = StateEffect.define<number>();
const clearEffect = StateEffect.define<void>();
const deco = Decoration.line({ class: "cm-flash-line" });

export const flashLineField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(flashEffect)) {
        const line = tr.state.doc.line(Math.min(e.value, tr.state.doc.lines));
        return Decoration.set([deco.range(line.from)]);
      }
      if (e.is(clearEffect)) return Decoration.none;
    }
    return value;
  },
  provide: (f) => EditorView.decorations.from(f),
});

let timer: ReturnType<typeof setTimeout> | null = null;

export function flashLine(view: EditorView, line: number) {
  if (timer) clearTimeout(timer);
  view.dispatch({ effects: flashEffect.of(line) });
  timer = setTimeout(() => {
    timer = null;
    try { view.dispatch({ effects: clearEffect.of(undefined) }); } catch {}
  }, 1500);
}
