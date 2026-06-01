import { Compartment, type Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";

export const languageCompartment = new Compartment();
const languageCache = new Map<string, Promise<Extension>>();
const hydratedLanguage = new WeakMap<EditorView, string>();

export function languageKey(filename: string): "java" | "python" | "typescript" | "javascript" | null {
  if (filename.endsWith(".java")) return "java";
  if (filename.endsWith(".py")) return "python";
  if (filename.endsWith(".ts") || filename.endsWith(".tsx")) return "typescript";
  if (filename.endsWith(".js") || filename.endsWith(".jsx")) return "javascript";
  return null;
}

export function loadLanguageExtension(filename: string): Promise<Extension> | null {
  const key = languageKey(filename);
  if (!key) return null;

  const cached = languageCache.get(key);
  if (cached) return cached;

  const promise = (async (): Promise<Extension> => {
    if (key === "java") return (await import("@codemirror/lang-java")).java();
    if (key === "python") return (await import("@codemirror/lang-python")).python();
    const mod = await import("@codemirror/lang-javascript");
    return key === "typescript"
      ? mod.javascript({ typescript: true, jsx: true })
      : mod.javascript({ jsx: true });
  })();

  languageCache.set(key, promise);
  return promise;
}

export function hydrateEditorLanguage(view: EditorView, filename: string) {
  const key = languageKey(filename);
  if (!key || hydratedLanguage.get(view) === key) return;
  hydratedLanguage.set(view, key);

  loadLanguageExtension(filename)
    ?.then((extension) => {
      if (hydratedLanguage.get(view) !== key) return;
      view.dispatch({ effects: languageCompartment.reconfigure(extension) });
    })
    .catch(() => {
      hydratedLanguage.delete(view);
    });
}
