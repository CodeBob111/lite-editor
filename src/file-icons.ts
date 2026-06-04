// 文件图标元信息（字形 + 颜色 + 是否淡化）的单一来源。
// Explorer 文件树与 Git 改动面板共用，保证同一文件在各处颜色一致。
// 配色跟随应用主题，保持侧栏、标签和 Git 改动面板的文件类型颜色一致。

export interface FileIconMeta {
  glyph: string;
  color: string;
  /** 被 git 忽略 / lock 等噪音文件：标签淡化显示 */
  dim?: boolean;
}

export function fileIconMeta(name: string): FileIconMeta {
  const lower = name.toLowerCase();

  // 特殊文件名优先于扩展名
  if (lower === "pom.xml") return { glyph: "m", color: "#7faedb" };
  if (
    lower === ".gitignore" ||
    lower === ".gitattributes" ||
    lower === ".gitmodules"
  )
    return { glyph: "≡", color: "#69737b", dim: true };
  if (
    lower.endsWith(".lock") ||
    lower.endsWith("-lock.json") ||
    lower.endsWith("-lock.yaml")
  )
    return { glyph: "≡", color: "#69737b", dim: true };

  const dot = lower.lastIndexOf(".");
  const ext = dot >= 0 ? lower.slice(dot + 1) : "";
  switch (ext) {
    case "java":
      return { glyph: "J", color: "#d08a5c" };
    case "kt":
    case "kts":
      return { glyph: "K", color: "#b997d2" };
    case "ts":
    case "tsx":
      return { glyph: "T", color: "#74b8d6" };
    case "js":
    case "jsx":
    case "mjs":
    case "cjs":
      return { glyph: "J", color: "#d8bf6a" };
    case "json":
      return { glyph: "{}", color: "#d8bf6a" };
    case "xml":
      return { glyph: "X", color: "#b997d2" };
    case "html":
    case "htm":
      return { glyph: "H", color: "#d88964" };
    case "css":
    case "scss":
    case "sass":
    case "less":
      return { glyph: "#", color: "#66b7bd" };
    case "md":
    case "markdown":
      return { glyph: "M", color: "#7faedb" };
    case "py":
      return { glyph: "P", color: "#74b8d6" };
    case "rs":
      return { glyph: "R", color: "#d88964" };
    case "go":
      return { glyph: "G", color: "#66b7bd" };
    case "sh":
    case "bash":
    case "zsh":
      return { glyph: "$", color: "#8fbc8f" };
    case "yaml":
    case "yml":
      return { glyph: "Y", color: "#a7b0b6" };
    case "properties":
      return { glyph: "P", color: "#a7b0b6" };
    case "toml":
    case "ini":
    case "conf":
    case "cfg":
    case "editorconfig":
    case "classpath":
    case "project":
      return { glyph: "≡", color: "#d6a457" };
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "svg":
    case "webp":
    case "ico":
    case "bmp":
      return { glyph: "▣", color: "#8fbc8f" };
    case "txt":
    case "log":
      return { glyph: "≡", color: "#69737b", dim: true };
    default:
      return { glyph: "·", color: "#a7b0b6" };
  }
}
