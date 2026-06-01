import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";

const baseTheme = EditorView.theme(
  {
    "&": {
      color: "#e0e0e0",
      backgroundColor: "#0e0e0e",
    },
    ".cm-content": {
      caretColor: "#808080",
    },
    ".cm-cursor, .cm-dropCursor": {
      borderLeftColor: "#808080",
    },
    "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
      {
        backgroundColor: "rgba(160, 160, 160, 0.2)",
      },
    ".cm-panels": {
      backgroundColor: "#161616",
      color: "#e0e0e0",
    },
    ".cm-panels.cm-panels-top": {
      borderBottom: "1px solid #2a2a2a",
    },
    ".cm-panels.cm-panels-bottom": {
      borderTop: "1px solid #2a2a2a",
    },
    ".cm-searchMatch": {
      backgroundColor: "rgba(160, 160, 160, 0.2)",
      outline: "1px solid rgba(160, 160, 160, 0.35)",
    },
    ".cm-searchMatch.cm-searchMatch-selected": {
      backgroundColor: "rgba(160, 160, 160, 0.4)",
    },
    ".cm-activeLine": {
      backgroundColor: "rgba(40, 40, 40, 0.5)",
    },
    ".cm-selectionMatch": {
      backgroundColor: "rgba(160, 160, 160, 0.12)",
    },
    "&.cm-focused .cm-matchingBracket, &.cm-focused .cm-nonmatchingBracket": {
      backgroundColor: "rgba(160, 160, 160, 0.25)",
    },
    ".cm-gutters": {
      backgroundColor: "#0e0e0e",
      color: "#5a5a5a",
      border: "none",
      borderRight: "1px solid #2a2a2a",
    },
    ".cm-activeLineGutter": {
      backgroundColor: "rgba(40, 40, 40, 0.5)",
      color: "#a0a0a0",
    },
    ".cm-foldPlaceholder": {
      backgroundColor: "transparent",
      border: "none",
      color: "#5a5a5a",
    },
    ".cm-tooltip": {
      border: "1px solid #2a2a2a",
      backgroundColor: "#1c1c1c",
      borderRadius: "6px",
      boxShadow: "0 8px 32px rgba(0,0,0,0.6)",
    },
    ".cm-tooltip .cm-tooltip-arrow:before": {
      borderTopColor: "transparent",
      borderBottomColor: "transparent",
    },
    ".cm-tooltip .cm-tooltip-arrow:after": {
      borderTopColor: "#1c1c1c",
      borderBottomColor: "#1c1c1c",
    },
    ".cm-tooltip-autocomplete": {
      "& > ul > li[aria-selected]": {
        backgroundColor: "rgba(160, 160, 160, 0.12)",
        color: "#e0e0e0",
      },
    },
  },
  { dark: true },
);

const highlightStyle = HighlightStyle.define([
  { tag: tags.keyword, color: "#c586c0" },
  { tag: [tags.name, tags.deleted, tags.character, tags.propertyName, tags.macroName], color: "#d4d4d4" },
  { tag: [tags.function(tags.variableName), tags.labelName], color: "#dcdcaa" },
  { tag: [tags.color, tags.constant(tags.name), tags.standard(tags.name)], color: "#c586c0" },
  { tag: [tags.definition(tags.name), tags.separator], color: "#d4d4d4" },
  { tag: [tags.typeName, tags.className, tags.number, tags.changed, tags.annotation, tags.modifier, tags.self, tags.namespace], color: "#4ec9b0" },
  { tag: [tags.operator, tags.operatorKeyword, tags.url, tags.escape, tags.regexp, tags.link, tags.special(tags.string)], color: "#d4d4d4" },
  { tag: [tags.meta, tags.comment], color: "#6a9955" },
  { tag: tags.strong, fontWeight: "bold" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strikethrough, textDecoration: "line-through" },
  { tag: tags.link, color: "#569cd6", textDecoration: "underline" },
  { tag: tags.heading, fontWeight: "bold", color: "#569cd6" },
  { tag: [tags.atom, tags.bool, tags.special(tags.variableName)], color: "#569cd6" },
  { tag: [tags.processingInstruction, tags.string, tags.inserted], color: "#ce9178" },
  { tag: tags.invalid, color: "#d45555" },
]);

export const warmEarthTheme = [baseTheme, syntaxHighlighting(highlightStyle)];
