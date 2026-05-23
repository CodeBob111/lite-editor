/**
 * Thin wrapper around Tauri IPC calls.
 * Every backend command lives here — components import from this module.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

// ---- Types matching Rust structs (snake_case from serde) ----

export interface FileNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: FileNode[];
}

export interface MavenModule {
  name: string;
  group_id: string;
  artifact_id: string;
  version: string;
  packaging: string;
  pom_path: string;
  modules: string[];
}

export interface LspUsage {
  uri: string;
  line: number;
  character: number;
  text: string;
}

// ---- File system ----

export async function readDirTree(path: string, maxDepth?: number): Promise<FileNode> {
  return invoke<FileNode>("read_dir_tree", { path, maxDepth: maxDepth ?? 12 });
}

export async function readFile(path: string): Promise<string> {
  return invoke<string>("read_file", { path });
}

export async function writeFile(path: string, content: string): Promise<void> {
  return invoke<void>("write_file", { path, content });
}

// ---- Folder dialog ----

export async function openFolderDialog(): Promise<string | null> {
  const result = await open({ directory: true, multiple: false, title: "Open Folder" });
  if (typeof result === "string") return result;
  return null;
}

// ---- Maven ----

export async function parseMavenModules(projectPath: string): Promise<MavenModule[]> {
  return invoke<MavenModule[]>("parse_maven_modules", { projectPath });
}

export async function runMavenCommand(projectPath: string, goals: string[]): Promise<string> {
  return invoke<string>("run_maven_command", { projectPath, goals });
}

export function onMavenOutput(callback: (line: string) => void): Promise<UnlistenFn> {
  return listen<string>("maven-output", (event) => callback(event.payload));
}

// ---- LSP ----

export async function startLsp(language: string, rootPath: string): Promise<void> {
  return invoke<void>("start_lsp", { language, rootPath });
}

export async function stopLsp(language: string): Promise<void> {
  return invoke<void>("stop_lsp", { language });
}

export async function lspDidOpen(filePath: string, languageId: string, content: string): Promise<void> {
  return invoke<void>("lsp_did_open", { filePath, languageId, content });
}

export async function lspDidChange(filePath: string, content: string): Promise<void> {
  return invoke<void>("lsp_did_change", { filePath, content });
}

export async function lspFindReferences(filePath: string, line: number, character: number): Promise<LspUsage[]> {
  return invoke<LspUsage[]>("lsp_find_references", { filePath, line, character });
}

export async function lspGotoDefinition(filePath: string, line: number, character: number): Promise<LspUsage | null> {
  return invoke<LspUsage | null>("lsp_goto_definition", { filePath, line, character });
}

// ---- Helpers ----

/** Convert FileNode from Rust (snake_case) to front-end shape (camelCase). */
export function toFrontendNode(node: FileNode): {
  name: string; path: string; isDir: boolean; children?: ReturnType<typeof toFrontendNode>[];
} {
  return {
    name: node.name,
    path: node.path,
    isDir: node.is_dir,
    children: node.children?.map(toFrontendNode),
  };
}
