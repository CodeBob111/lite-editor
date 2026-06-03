import { invoke as rawInvoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { record, type PerfEntry } from "./perf-monitor";

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const t0 = performance.now();
  return rawInvoke<T>(cmd, args).finally(() => {
    const ms = performance.now() - t0;
    const entry: PerfEntry = { ts: Date.now(), kind: "ipc", label: cmd, ms };
    const first = args ? Object.values(args)[0] : undefined;
    if (typeof first === "string" && first.length < 120) entry.args = first;
    record(entry);
  });
}

// ---- Types matching Rust structs ----

export interface FileNode {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileNode[];
  expanded?: boolean;
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

export interface SearchResult {
  path: string;
  line: number;
  column: number;
  text: string;
}

// ---- File system ----

export async function readDirTree(path: string, maxDepth?: number): Promise<FileNode> {
  return invoke<FileNode>("read_dir_tree", { path, maxDepth: maxDepth ?? 4 });
}

export async function readFile(path: string): Promise<string> {
  return invoke<string>("read_file", { path });
}

export async function writeFile(path: string, content: string): Promise<void> {
  return invoke<void>("write_file", { path, content });
}

export async function createFile(path: string): Promise<void> {
  return invoke<void>("create_file", { path });
}

export async function createDir(path: string): Promise<void> {
  return invoke<void>("create_dir", { path });
}

export async function deletePath(path: string): Promise<void> {
  return invoke<void>("delete_path", { path });
}

export async function copyPath(src: string, dest: string): Promise<void> {
  return invoke<void>("copy_path", { src, dest });
}

export async function renamePath(oldPath: string, newPath: string): Promise<void> {
  return invoke<void>("rename_path", { oldPath, newPath });
}

// ---- Folder dialog ----

export async function openFolderDialog(): Promise<string | null> {
  const result = await open({ directory: true, multiple: false, title: "Open Folder" });
  if (typeof result === "string") return result;
  return null;
}

export async function saveFileDialog(defaultPath?: string): Promise<string | null> {
  const result = await save({ title: "Save File", defaultPath });
  if (typeof result === "string") return result;
  return null;
}

// ---- Search ----

export async function searchInFiles(
  projectPath: string,
  query: string,
  caseSensitive?: boolean,
  maxResults?: number,
): Promise<SearchResult[]> {
  return invoke<SearchResult[]>("search_in_files", {
    projectPath,
    query,
    caseSensitive: caseSensitive ?? false,
    maxResults: maxResults ?? 500,
  });
}

export async function listAllFiles(projectPath: string): Promise<string[]> {
  return invoke<string[]>("list_all_files", { projectPath });
}

// ---- Maven ----

export async function parseMavenModules(projectPath: string): Promise<MavenModule[]> {
  return invoke<MavenModule[]>("parse_maven_modules", { projectPath });
}

export async function runMavenCommand(projectPath: string, goals: string[]): Promise<void> {
  return invoke<void>("run_maven_command", { projectPath, goals });
}

export interface MavenCollectResult {
  exit_code: number;
  output: string;
}

export async function runMavenCollect(projectPath: string, goals: string[]): Promise<MavenCollectResult> {
  return invoke<MavenCollectResult>("run_maven_collect", { projectPath, goals });
}

export function onMavenOutput(callback: (line: string) => void): Promise<UnlistenFn> {
  return listen<string>("maven-output", (event) => callback(event.payload));
}

export function onMavenDone(callback: (exitCode: number) => void): Promise<UnlistenFn> {
  return listen<number>("maven-done", (event) => callback(event.payload));
}

// ---- LSP ----

export async function startLsp(language: string, rootPath: string): Promise<void> {
  return invoke<void>("start_lsp", { language, rootPath });
}

export async function stopLsp(language: string, rootPath: string): Promise<void> {
  return invoke<void>("stop_lsp", { language, rootPath });
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

export interface DecompiledClass {
  path: string;
  content: string;
}

export async function findClassInMaven(fqn: string): Promise<DecompiledClass | null> {
  return invoke<DecompiledClass | null>("find_class_in_maven", { fqn });
}

export async function lspIsReady(filePath: string): Promise<boolean> {
  return invoke<boolean>("lsp_is_ready", { filePath });
}

export interface LspProgressEvent {
  language: string;
  kind: string;
  message: string;
  percentage: number | null;
}

export function onLspProgress(callback: (event: LspProgressEvent) => void): Promise<UnlistenFn> {
  return listen<LspProgressEvent>("lsp-progress", (event) => callback(event.payload));
}

export interface LspDiagnosticParams {
  uri: string;
  diagnostics: Array<{
    range: {
      start: { line: number; character: number };
      end: { line: number; character: number };
    };
    severity?: number;
    message: string;
    source?: string;
  }>;
}

export function onLspDiagnostics(callback: (params: LspDiagnosticParams) => void): Promise<UnlistenFn> {
  return listen<LspDiagnosticParams>("lsp-diagnostics", (event) => callback(event.payload));
}

// ---- File watcher ----

export async function startFileWatcher(projectPath: string): Promise<void> {
  return invoke<void>("start_file_watcher", { projectPath });
}

export async function stopFileWatcher(projectPath: string): Promise<void> {
  return invoke<void>("stop_file_watcher", { projectPath });
}

export interface FileChangeEvent {
  project: string;
  hasStructural: boolean;
}

export function onFileChanged(callback: (event: FileChangeEvent) => void): Promise<UnlistenFn> {
  return listen<FileChangeEvent>("file-changed", (event) => callback(event.payload));
}

// ---- Session persistence ----

export async function saveSessionData(data: string): Promise<void> {
  return invoke<void>("save_session", { data });
}

export async function loadSessionData(): Promise<string | null> {
  return invoke<string | null>("load_session", {});
}

// ---- Terminal ----

export async function spawnTerminal(cwd: string, cols: number, rows: number): Promise<number> {
  return invoke<number>("spawn_terminal", { cwd, cols, rows });
}

export async function writeTerminal(id: number, data: string): Promise<void> {
  return invoke<void>("write_terminal", { id, data });
}

export async function resizeTerminal(id: number, cols: number, rows: number): Promise<void> {
  return invoke<void>("resize_terminal", { id, cols, rows });
}

export async function closeTerminal(id: number): Promise<void> {
  return invoke<void>("close_terminal", { id });
}

export async function getClaudeStatus(id: number): Promise<string | null> {
  return invoke<string | null>("get_claude_status", { id });
}


export function onTerminalOutput(callback: (id: number, data: string) => void): Promise<UnlistenFn> {
  return listen<[number, string]>("terminal-output", (event) => callback(event.payload[0], event.payload[1]));
}

export function onTerminalExit(callback: (id: number) => void): Promise<UnlistenFn> {
  return listen<number>("terminal-exit", (event) => callback(event.payload));
}

// ---- Menu ----

export function onMenuAction(callback: (id: string) => void): Promise<UnlistenFn> {
  return listen<string>("menu-action", (event) => callback(event.payload));
}

// ---- Git ----

export interface GitRepo {
  name: string;
  path: string;
}

export async function gitDiscoverRepos(root: string): Promise<GitRepo[]> {
  return invoke<GitRepo[]>("git_discover_repos", { root });
}

export interface GitChange {
  path: string;
  old_path: string | null;
  status: string;
  staged: boolean;
}

export async function gitStatus(cwd: string): Promise<GitChange[]> {
  return invoke<GitChange[]>("git_status", { cwd });
}

export interface BatchResult<T> {
  path: string;
  result: T | null;
  error: string | null;
}

export async function gitStatusBatch(paths: string[]): Promise<BatchResult<GitChange[]>[]> {
  return invoke<BatchResult<GitChange[]>[]>("git_status_batch", { paths });
}

export async function gitCurrentBranchBatch(paths: string[]): Promise<BatchResult<string>[]> {
  return invoke<BatchResult<string>[]>("git_current_branch_batch", { paths });
}

export async function gitShowFile(cwd: string, relPath: string): Promise<string> {
  return invoke<string>("git_show_file", { cwd, relPath });
}

export async function gitShowStaged(cwd: string, relPath: string): Promise<string> {
  return invoke<string>("git_show_staged", { cwd, relPath });
}

export async function gitCurrentBranch(cwd: string): Promise<string> {
  return invoke<string>("git_current_branch", { cwd });
}

export interface GitBranch {
  name: string;
  current: boolean;
  remote: boolean;
  ahead: number;
  behind: number;
  tracking: string | null;
  upstream_gone: boolean;
}

export async function gitListBranches(cwd: string): Promise<GitBranch[]> {
  return invoke<GitBranch[]>("git_list_branches", { cwd });
}

export async function gitCheckout(cwd: string, branch: string, force?: boolean): Promise<string> {
  return invoke<string>("git_checkout", { cwd, branch, force: force ?? false });
}

export async function gitNewBranch(cwd: string, branch: string, startPoint: string): Promise<string> {
  return invoke<string>("git_new_branch", { cwd, branch, startPoint });
}

export async function gitDeleteBranch(cwd: string, branch: string, force: boolean): Promise<string> {
  return invoke<string>("git_delete_branch", { cwd, branch, force });
}

export async function gitPull(cwd: string, branch?: string, tracking?: string | null): Promise<string> {
  return invoke<string>("git_pull", { cwd, branch: branch ?? null, tracking: tracking ?? null });
}

export async function gitPrune(cwd: string, remote?: string | null): Promise<string> {
  return invoke<string>("git_prune", { cwd, remote: remote ?? null });
}

export async function gitFetchBranch(cwd: string, remote: string, branch: string): Promise<string> {
  return invoke<string>("git_fetch_branch", { cwd, remote, branch });
}

export async function gitPush(cwd: string, branch: string): Promise<string> {
  return invoke<string>("git_push", { cwd, branch });
}

export async function gitCommit(cwd: string, files: string[], message: string): Promise<string> {
  return invoke<string>("git_commit", { cwd, files, message });
}

export interface MergeResult {
  success: boolean;
  message: string;
  conflicts: string[];
}

export async function gitMerge(cwd: string, branch: string): Promise<MergeResult> {
  return invoke<MergeResult>("git_merge", { cwd, branch });
}

export async function gitMergeConflicts(cwd: string): Promise<string[]> {
  return invoke<string[]>("git_merge_conflicts", { cwd });
}

export async function gitShowConflictVersion(cwd: string, relPath: string, stage: number): Promise<string> {
  return invoke<string>("git_show_conflict_version", { cwd, relPath, stage });
}

export async function gitMergeAbort(cwd: string): Promise<string> {
  return invoke<string>("git_merge_abort", { cwd });
}

export async function gitResolveConflictFile(cwd: string, relPath: string, content: string): Promise<string> {
  return invoke<string>("git_resolve_conflict_file", { cwd, relPath, content });
}

export async function gitCheckoutConflictSide(cwd: string, relPath: string, side: "ours" | "theirs"): Promise<string> {
  return invoke<string>("git_checkout_conflict_side", { cwd, relPath, side });
}

export async function gitDiscardChanges(cwd: string, relPath: string, status: string): Promise<string> {
  return invoke<string>("git_discard_changes", { cwd, relPath, status });
}

export async function gitRebase(cwd: string, branch: string): Promise<string> {
  return invoke<string>("git_rebase", { cwd, branch });
}

export async function gitRenameBranch(cwd: string, oldName: string, newName: string): Promise<string> {
  return invoke<string>("git_rename_branch", { cwd, oldName, newName });
}

export interface GitCommit {
  hash: string;
  short_hash: string;
  parents: string[];
  author: string;
  date: string;
  refs: string;
  subject: string;
}

export async function gitLog(cwd: string, branch: string, limit = 200): Promise<GitCommit[]> {
  return invoke<GitCommit[]>("git_log", { cwd, branch, limit });
}

export async function gitCommitPatch(cwd: string, hash: string): Promise<string> {
  return invoke<string>("git_commit_patch", { cwd, hash });
}

export async function gitCherryPick(cwd: string, hash: string): Promise<string> {
  return invoke<string>("git_cherry_pick", { cwd, hash });
}

export async function gitCheckoutRevision(cwd: string, hash: string): Promise<string> {
  return invoke<string>("git_checkout_revision", { cwd, hash });
}

export async function gitRevertCommit(cwd: string, hash: string): Promise<string> {
  return invoke<string>("git_revert_commit", { cwd, hash });
}

export async function gitNewBranchAtCommit(cwd: string, branch: string, hash: string): Promise<string> {
  return invoke<string>("git_new_branch_at_commit", { cwd, branch, hash });
}

// ---- Java Index ----

export interface ClassLocation {
  path: string;
  package: string;
  fqn: string;
}

export async function buildJavaIndex(projectPath: string): Promise<number> {
  return invoke<number>("build_java_index", { projectPath });
}

export async function loadJavaIndex(projectPath: string): Promise<number> {
  return invoke<number>("load_java_index", { projectPath });
}

export async function searchJavaClass(projectPath: string, className: string): Promise<ClassLocation[]> {
  return invoke<ClassLocation[]>("search_java_class", { projectPath, className });
}

export async function updateJavaIndexFile(projectPath: string, filePath: string): Promise<void> {
  return invoke<void>("update_java_index_file", { projectPath, filePath });
}

export async function removeJavaIndexFile(projectPath: string, filePath: string): Promise<void> {
  return invoke<void>("remove_java_index_file", { projectPath, filePath });
}

// ---- Git Blame ----

export interface BlameLine {
  author: string;
  timestamp: number;
}

export async function gitBlame(cwd: string, filePath: string): Promise<BlameLine[]> {
  return invoke<BlameLine[]>("git_blame", { cwd, filePath });
}

// ---- Git Clone ----

export async function gitClone(url: string, directory: string): Promise<string> {
  return invoke<string>("git_clone", { url, directory });
}

export async function gitRemoteUrl(cwd: string, remote?: string): Promise<string> {
  return invoke<string>("git_remote_url", { cwd, remote: remote ?? null });
}

// ---- Astore ----

export interface AstoreSession {
  username: string;
  nickName: string;
  empId: string;
  authToken: string;
  accessToken: string;
  refreshToken: string;
  ssoToken: string;
  currentEnv: string;
  templateId: number | null;
  siteId: number | null;
  changeRecordId: number | null;
  tenantCode: string | null;
  bizCode: string | null;
  projectType: string;
}

export async function astoreLogin(username: string, password: string): Promise<AstoreSession> {
  return invoke<AstoreSession>("astore_login", { username, password });
}

export async function astoreLogout(): Promise<void> {
  return invoke<void>("astore_logout");
}

export async function astoreGetSession(): Promise<AstoreSession> {
  return invoke<AstoreSession>("astore_get_session");
}

export async function astoreDetectProject(projectPath: string): Promise<AstoreSession> {
  return invoke<AstoreSession>("astore_detect_project", { projectPath });
}

export async function astoreSetEnv(env: string): Promise<void> {
  return invoke<void>("astore_set_env", { env });
}

export async function astoreSyncCode(gitUrl: string, branch: string): Promise<string> {
  return invoke<string>("astore_sync_code", { gitUrl, branch });
}

export async function astorePublish(): Promise<string> {
  return invoke<string>("astore_publish");
}

export async function astoreGetEditorUrl(): Promise<string> {
  return invoke<string>("astore_get_editor_url");
}

export async function astoreGetRecordUrl(): Promise<string> {
  return invoke<string>("astore_get_record_url");
}

// ---- Helpers ----

