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
    // Cmd+F 查找条:自定义面板(见 editor-search-panel.ts),对齐 IntelliJ IDEA 的查找条。
    // 顶部定位由 search({ top: true }) 负责;下面是工具条本体与各控件的样式。
    ".cm-idea-search": {
      display: "flex",
      alignItems: "center",
      gap: "5px",
      padding: "7px 10px",
      backgroundColor: ui.panel,
      fontSize: "12.5px",
    },
    ".cm-idea-search-icon": {
      color: ui.muted,
      fontSize: "15px",
      lineHeight: "1",
    },
    ".cm-idea-search-input": {
      flex: "0 1 300px",
      minWidth: "150px",
      fontSize: "13px",
      padding: "5px 9px",
      borderRadius: "6px",
      backgroundColor: ui.card,
      border: `1px solid ${ui.border}`,
      color: ui.text,
      outline: "none",
      fontFamily: "inherit",
    },
    ".cm-idea-search-input:focus": {
      borderColor: ui.accent,
      boxShadow: "0 0 0 2px rgba(102, 183, 189, 0.22)",
    },
    ".cm-idea-search-toggle": {
      minWidth: "24px",
      height: "24px",
      padding: "0 5px",
      borderRadius: "5px",
      backgroundColor: "transparent",
      border: "1px solid transparent",
      color: ui.muted,
      fontSize: "12px",
      fontWeight: "600",
      cursor: "pointer",
      fontFamily: "inherit",
      lineHeight: "1",
    },
    ".cm-idea-search-toggle:hover": {
      backgroundColor: ui.card,
      color: ui.textSubtle,
    },
    ".cm-idea-search-toggle.active": {
      backgroundColor: "rgba(102, 183, 189, 0.18)",
      borderColor: ui.accent,
      color: ui.accent,
    },
    ".cm-idea-search-count": {
      minWidth: "30px",
      padding: "0 4px",
      color: ui.muted,
      fontSize: "12px",
      whiteSpace: "nowrap",
    },
    ".cm-idea-search-count.empty, .cm-idea-search-count.error": {
      color: "#df6b73",
    },
    ".cm-idea-search-nav": {
      width: "24px",
      height: "24px",
      padding: "0",
      borderRadius: "5px",
      backgroundColor: "transparent",
      border: "none",
      color: ui.textSubtle,
      fontSize: "14px",
      cursor: "pointer",
      lineHeight: "1",
    },
    ".cm-idea-search-nav:hover": {
      backgroundColor: ui.card,
      color: ui.text,
    },
    ".cm-idea-search-close": {
      marginLeft: "2px",
      color: ui.muted,
      fontSize: "13px",
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
