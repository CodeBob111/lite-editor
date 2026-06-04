import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";

const ui = {
  bg: "#101314",
  panel: "#171a1c",
  card: "#1d2224",
  border: "#2b3336",
  text: "#dde3e7",
  textSubtle: "#a7b0b6",
  muted: "#69737b",
  accent: "#66b7bd",
  selection: "rgba(102, 183, 189, 0.24)",
  search: "rgba(214, 164, 87, 0.28)",
};

const baseTheme = EditorView.theme(
  {
    "&": {
      color: ui.text,
      backgroundColor: ui.bg,
    },
    ".cm-content": {
      caretColor: ui.accent,
    },
    ".cm-cursor, .cm-dropCursor": {
      borderLeftColor: ui.accent,
    },
    "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
      {
        backgroundColor: ui.selection,
      },
    ".cm-panels": {
      backgroundColor: ui.panel,
      color: ui.text,
    },
    ".cm-panels.cm-panels-top": {
      borderBottom: `1px solid ${ui.border}`,
    },
    ".cm-panels.cm-panels-bottom": {
      borderTop: `1px solid ${ui.border}`,
    },
    ".cm-searchMatch": {
      backgroundColor: ui.search,
      outline: "1px solid rgba(214, 164, 87, 0.42)",
    },
    ".cm-searchMatch.cm-searchMatch-selected": {
      backgroundColor: "rgba(214, 164, 87, 0.46)",
    },
    ".cm-activeLine": {
      backgroundColor: "rgba(102, 183, 189, 0.055)",
    },
    ".cm-selectionMatch": {
      backgroundColor: "rgba(102, 183, 189, 0.14)",
    },
    "&.cm-focused .cm-matchingBracket, &.cm-focused .cm-nonmatchingBracket": {
      backgroundColor: "rgba(102, 183, 189, 0.22)",
    },
    ".cm-gutters": {
      backgroundColor: ui.bg,
      color: ui.muted,
      border: "none",
      borderRight: `1px solid ${ui.border}`,
    },
    ".cm-activeLineGutter": {
      backgroundColor: "rgba(102, 183, 189, 0.07)",
      color: ui.textSubtle,
    },
    ".cm-foldPlaceholder": {
      backgroundColor: "transparent",
      border: "none",
      color: ui.muted,
    },
    ".cm-tooltip": {
      border: `1px solid ${ui.border}`,
      backgroundColor: ui.card,
      borderRadius: "6px",
      boxShadow: "0 8px 32px rgba(0,0,0,0.6)",
    },
    ".cm-tooltip .cm-tooltip-arrow:before": {
      borderTopColor: "transparent",
      borderBottomColor: "transparent",
    },
    ".cm-tooltip .cm-tooltip-arrow:after": {
      borderTopColor: ui.card,
      borderBottomColor: ui.card,
    },
    ".cm-tooltip-autocomplete": {
      "& > ul > li[aria-selected]": {
        backgroundColor: "rgba(102, 183, 189, 0.14)",
        color: ui.text,
      },
    },
  },
  { dark: true },
);

const highlightStyle = HighlightStyle.define([
  { tag: tags.keyword, color: "#b997d2" },
  { tag: [tags.name, tags.deleted, tags.character, tags.macroName], color: "#d7dee3" },
  { tag: tags.propertyName, color: "#9ccfd8" },
  { tag: [tags.function(tags.variableName), tags.labelName], color: "#d7c985" },
  { tag: [tags.color, tags.constant(tags.name), tags.standard(tags.name)], color: "#d7a65f" },
  { tag: [tags.definition(tags.name), tags.separator], color: "#d7dee3" },
  { tag: [tags.typeName, tags.className, tags.changed, tags.self, tags.namespace], color: "#6fc3b2" },
  { tag: [tags.number, tags.annotation, tags.modifier], color: "#d7a65f" },
  { tag: [tags.operator, tags.operatorKeyword, tags.url, tags.escape, tags.regexp, tags.link, tags.special(tags.string)], color: "#bac4ca" },
  { tag: [tags.meta, tags.comment], color: "#7f916f", fontStyle: "italic" },
  { tag: tags.strong, fontWeight: "bold" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strikethrough, textDecoration: "line-through" },
  { tag: tags.link, color: "#7faedb", textDecoration: "underline" },
  { tag: tags.heading, fontWeight: "bold", color: "#7faedb" },
  { tag: [tags.atom, tags.bool, tags.special(tags.variableName)], color: "#7faedb" },
  { tag: [tags.processingInstruction, tags.string, tags.inserted], color: "#d89a74" },
  { tag: tags.invalid, color: "#df6b73" },
]);

export const warmEarthTheme = [baseTheme, syntaxHighlighting(highlightStyle)];
