// 薄壳层(M0a 起):全部命令转发 nib-core,本文件不含业务逻辑。
// 命令名/参数名/返回类型与历史完全一致,前端零改动。
// 阻塞与异步重活都在 core 自持的 runtime 上执行(RFC v2 §3):
// on_worker 型 core 函数内部已落 core runtime;astore(reqwest)经 rt::run 显式搬运。

use nib_core::NibCore;
use std::sync::Arc;
use tauri::State;

type Core<'a> = State<'a, Arc<NibCore>>;

// ---- File system ----

#[tauri::command]
pub async fn read_dir_tree(
    path: String,
    max_depth: Option<usize>,
) -> Result<nib_core::fs::FileNode, String> {
    nib_core::fs::read_dir_tree(path, max_depth).await
}

#[tauri::command]
pub async fn read_file(path: String) -> Result<String, String> {
    nib_core::fs::read_file(path).await
}

#[tauri::command]
pub async fn write_file(path: String, content: String) -> Result<(), String> {
    nib_core::fs::write_file(path, content).await
}

#[tauri::command]
pub async fn create_file(path: String) -> Result<(), String> {
    nib_core::fs::create_file(path).await
}

#[tauri::command]
pub async fn create_dir(path: String) -> Result<(), String> {
    nib_core::fs::create_dir(path).await
}

#[tauri::command]
pub async fn delete_path(path: String) -> Result<(), String> {
    nib_core::fs::delete_path(path).await
}

#[tauri::command]
pub async fn copy_path(src: String, dest: String) -> Result<(), String> {
    nib_core::fs::copy_path(src, dest).await
}

#[tauri::command]
pub async fn rename_path(old_path: String, new_path: String) -> Result<(), String> {
    nib_core::fs::rename_path(old_path, new_path).await
}

// ---- Search ----

#[tauri::command]
pub async fn search_in_files(
    project_path: String,
    query: String,
    case_sensitive: Option<bool>,
    max_results: Option<usize>,
) -> Result<Vec<nib_core::search::SearchResult>, String> {
    nib_core::search::search_in_files(project_path, query, case_sensitive, max_results).await
}

#[tauri::command]
pub async fn list_all_files(project_path: String) -> Result<Vec<String>, String> {
    nib_core::search::list_all_files(project_path).await
}

// ---- Maven / Markdown ----

#[tauri::command]
pub async fn parse_maven_modules(
    project_path: String,
) -> Result<Vec<nib_core::maven::MavenModule>, String> {
    nib_core::maven::parse_maven_modules(project_path).await
}

#[tauri::command]
pub fn run_maven_command(
    project_path: String,
    goals: Vec<String>,
    core: Core<'_>,
) -> Result<(), String> {
    nib_core::maven::run_maven_command(project_path, goals, core.events.clone())
}

#[tauri::command]
pub async fn maven_dependency_tree(
    project_path: String,
) -> Result<nib_core::maven::MavenDepTree, String> {
    nib_core::maven::maven_dependency_tree(project_path).await
}

#[tauri::command]
pub async fn maven_add_exclusion(
    pom_path: String,
    parent_group_id: String,
    parent_artifact_id: String,
    exclude_group_id: String,
    exclude_artifact_id: String,
) -> Result<(), String> {
    nib_core::maven::maven_add_exclusion(
        pom_path,
        parent_group_id,
        parent_artifact_id,
        exclude_group_id,
        exclude_artifact_id,
    )
    .await
}

#[tauri::command]
pub async fn render_markdown(text: String) -> Result<String, String> {
    nib_core::markdown::render_markdown(text).await
}

// ---- File watcher ----

#[tauri::command]
pub fn start_file_watcher(project_path: String, core: Core<'_>) -> Result<(), String> {
    nib_core::watch::start_file_watcher(project_path, core.events.clone(), &core.watcher)
}

#[tauri::command]
pub fn stop_file_watcher(project_path: String, core: Core<'_>) -> Result<(), String> {
    nib_core::watch::stop_file_watcher(&project_path, &core.watcher)
}

// ---- Session / Settings ----

#[tauri::command]
pub async fn save_session(data: String, core: Core<'_>) -> Result<(), String> {
    nib_core::session::save_session(&core.dirs, data).await
}

#[tauri::command]
pub async fn load_session(core: Core<'_>) -> Result<Option<String>, String> {
    nib_core::session::load_session(&core.dirs).await
}

#[tauri::command]
pub async fn save_settings(data: String, core: Core<'_>) -> Result<(), String> {
    nib_core::session::save_settings(&core.dirs, data).await
}

#[tauri::command]
pub async fn load_settings(core: Core<'_>) -> Result<Option<String>, String> {
    nib_core::session::load_settings(&core.dirs).await
}

// ---- Clipboard ----

#[tauri::command]
pub fn copy_files_to_clipboard(paths: Vec<String>) -> Result<(), String> {
    nib_core::clipboard::copy_files_to_clipboard(paths)
}

#[tauri::command]
pub fn copy_text_to_clipboard(text: String) -> Result<(), String> {
    nib_core::clipboard::copy_text_to_clipboard(text)
}

// ---- LSP ----

#[tauri::command]
pub async fn start_lsp(language: String, root_path: String, core: Core<'_>) -> Result<(), String> {
    nib_core::lsp::start_lsp(
        language,
        root_path,
        core.events.clone(),
        core.dirs.jdtls_workspaces(),
        &core.lsp,
    )
    .await
}

#[tauri::command]
pub async fn stop_lsp(language: String, root_path: String, core: Core<'_>) -> Result<(), String> {
    nib_core::lsp::stop_lsp(language, root_path, &core.lsp).await
}

#[tauri::command]
pub fn lsp_is_ready(file_path: String, core: Core<'_>) -> Result<bool, String> {
    nib_core::lsp::lsp_is_ready(file_path, &core.lsp)
}

#[tauri::command]
pub async fn lsp_did_open(
    file_path: String,
    language_id: String,
    content: String,
    core: Core<'_>,
) -> Result<(), String> {
    nib_core::lsp::lsp_did_open(file_path, language_id, content, &core.lsp).await
}

#[tauri::command]
pub async fn lsp_did_change(
    file_path: String,
    content: String,
    core: Core<'_>,
) -> Result<(), String> {
    nib_core::lsp::lsp_did_change(file_path, content, &core.lsp).await
}

#[tauri::command]
pub async fn lsp_find_references(
    file_path: String,
    line: u32,
    character: u32,
    core: Core<'_>,
) -> Result<Vec<nib_core::lsp::LspUsage>, String> {
    nib_core::lsp::lsp_find_references(file_path, line, character, &core.lsp).await
}

#[tauri::command]
pub async fn lsp_goto_definition(
    file_path: String,
    line: u32,
    character: u32,
    core: Core<'_>,
) -> Result<Option<nib_core::lsp::LspUsage>, String> {
    nib_core::lsp::lsp_goto_definition(file_path, line, character, &core.lsp).await
}

#[tauri::command]
pub async fn lsp_document_symbols(
    file_path: String,
    core: Core<'_>,
) -> Result<serde_json::Value, String> {
    nib_core::lsp::lsp_document_symbols(file_path, &core.lsp).await
}

#[tauri::command]
pub async fn find_class_in_maven(
    fqn: String,
    core: Core<'_>,
) -> Result<Option<nib_core::lsp::DecompiledClass>, String> {
    nib_core::lsp::find_class_in_maven(fqn, core.dirs.decompiled()).await
}

// ---- Git ----

#[tauri::command]
pub async fn git_current_branch(cwd: String) -> Result<String, String> {
    nib_core::git::git_current_branch(cwd).await
}

#[tauri::command]
pub async fn git_current_branch_batch(
    paths: Vec<String>,
) -> Vec<nib_core::git::BatchResult<String>> {
    nib_core::git::git_current_branch_batch(paths).await
}

#[tauri::command]
pub async fn git_status_batch(
    paths: Vec<String>,
) -> Vec<nib_core::git::BatchResult<Vec<nib_core::git::GitChange>>> {
    nib_core::git::git_status_batch(paths).await
}

#[tauri::command]
pub async fn git_list_branches(cwd: String) -> Result<Vec<nib_core::git::GitBranch>, String> {
    nib_core::git::git_list_branches(cwd).await
}

#[tauri::command]
pub async fn git_checkout(
    cwd: String,
    branch: String,
    force: Option<bool>,
) -> Result<String, String> {
    nib_core::git::git_checkout(cwd, branch, force).await
}

#[tauri::command]
pub async fn git_new_branch(
    cwd: String,
    branch: String,
    start_point: String,
) -> Result<String, String> {
    nib_core::git::git_new_branch(cwd, branch, start_point).await
}

#[tauri::command]
pub async fn git_delete_branch(cwd: String, branch: String, force: bool) -> Result<String, String> {
    nib_core::git::git_delete_branch(cwd, branch, force).await
}

#[tauri::command]
pub async fn git_prune(cwd: String, remote: Option<String>) -> Result<String, String> {
    nib_core::git::git_prune(cwd, remote).await
}

#[tauri::command]
pub async fn git_pull(
    cwd: String,
    branch: Option<String>,
    tracking: Option<String>,
) -> Result<String, String> {
    nib_core::git::git_pull(cwd, branch, tracking).await
}

#[tauri::command]
pub async fn git_fetch_branch(cwd: String, remote: String, branch: String) -> Result<String, String> {
    nib_core::git::git_fetch_branch(cwd, remote, branch).await
}

#[tauri::command]
pub async fn git_push(cwd: String, branch: String) -> Result<String, String> {
    nib_core::git::git_push(cwd, branch).await
}

#[tauri::command]
pub async fn git_merge(cwd: String, branch: String) -> Result<nib_core::git::MergeResult, String> {
    nib_core::git::git_merge(cwd, branch).await
}

#[tauri::command]
pub async fn git_merge_conflicts(cwd: String) -> Result<Vec<String>, String> {
    nib_core::git::git_merge_conflicts(cwd).await
}

#[tauri::command]
pub async fn git_show_conflict_version(
    cwd: String,
    rel_path: String,
    stage: u32,
) -> Result<String, String> {
    nib_core::git::git_show_conflict_version(cwd, rel_path, stage).await
}

#[tauri::command]
pub async fn git_merge_abort(cwd: String) -> Result<String, String> {
    nib_core::git::git_merge_abort(cwd).await
}

#[tauri::command]
pub async fn git_resolve_conflict_file(
    cwd: String,
    rel_path: String,
    content: String,
) -> Result<String, String> {
    nib_core::git::git_resolve_conflict_file(cwd, rel_path, content).await
}

#[tauri::command]
pub async fn git_checkout_conflict_side(
    cwd: String,
    rel_path: String,
    side: String,
) -> Result<String, String> {
    nib_core::git::git_checkout_conflict_side(cwd, rel_path, side).await
}

#[tauri::command]
pub async fn git_discard_changes(
    cwd: String,
    rel_path: String,
    status: String,
) -> Result<String, String> {
    nib_core::git::git_discard_changes(cwd, rel_path, status).await
}

#[tauri::command]
pub async fn git_rebase(cwd: String, branch: String) -> Result<String, String> {
    nib_core::git::git_rebase(cwd, branch).await
}

#[tauri::command]
pub async fn git_rename_branch(
    cwd: String,
    old_name: String,
    new_name: String,
) -> Result<String, String> {
    nib_core::git::git_rename_branch(cwd, old_name, new_name).await
}

#[tauri::command]
pub async fn git_log(
    cwd: String,
    branch: String,
    limit: Option<u32>,
) -> Result<Vec<nib_core::git::GitCommit>, String> {
    nib_core::git::git_log(cwd, branch, limit).await
}

#[tauri::command]
pub async fn git_commit_patch(cwd: String, hash: String) -> Result<String, String> {
    nib_core::git::git_commit_patch(cwd, hash).await
}

#[tauri::command]
pub async fn git_cherry_pick(cwd: String, hash: String) -> Result<String, String> {
    nib_core::git::git_cherry_pick(cwd, hash).await
}

#[tauri::command]
pub async fn git_checkout_revision(cwd: String, hash: String) -> Result<String, String> {
    nib_core::git::git_checkout_revision(cwd, hash).await
}

#[tauri::command]
pub async fn git_revert_commit(cwd: String, hash: String) -> Result<String, String> {
    nib_core::git::git_revert_commit(cwd, hash).await
}

#[tauri::command]
pub async fn git_new_branch_at_commit(
    cwd: String,
    branch: String,
    hash: String,
) -> Result<String, String> {
    nib_core::git::git_new_branch_at_commit(cwd, branch, hash).await
}

#[tauri::command]
pub async fn git_commit(
    cwd: String,
    files: Vec<String>,
    message: String,
) -> Result<String, String> {
    nib_core::git::git_commit(cwd, files, message).await
}

#[tauri::command]
pub async fn git_status(cwd: String) -> Result<Vec<nib_core::git::GitChange>, String> {
    nib_core::git::git_status(cwd).await
}

#[tauri::command]
pub async fn git_show_file(cwd: String, rel_path: String) -> Result<String, String> {
    nib_core::git::git_show_file(cwd, rel_path).await
}

#[tauri::command]
pub async fn git_show_staged(cwd: String, rel_path: String) -> Result<String, String> {
    nib_core::git::git_show_staged(cwd, rel_path).await
}

#[tauri::command]
pub async fn git_discover_repos(root: String) -> Vec<nib_core::git::GitRepo> {
    nib_core::git::git_discover_repos(root).await
}

#[tauri::command]
pub async fn git_blame(
    cwd: String,
    file_path: String,
) -> Result<Vec<nib_core::git::BlameLine>, String> {
    nib_core::git::git_blame(cwd, file_path).await
}

#[tauri::command]
pub async fn git_clone(url: String, directory: String) -> Result<String, String> {
    nib_core::git::git_clone(url, directory).await
}

#[tauri::command]
pub async fn git_remote_url(cwd: String, remote: Option<String>) -> Result<String, String> {
    nib_core::git::git_remote_url(cwd, remote).await
}

#[tauri::command]
pub async fn parse_conflict_file(path: String) -> Result<nib_core::git::ConflictParse, String> {
    nib_core::git::parse_conflict_file(path).await
}

// ---- Java index ----

#[tauri::command]
pub async fn build_java_index(project_path: String, core: Core<'_>) -> Result<u32, String> {
    nib_core::java_index::build_java_index(project_path, core.dirs.java_index(), &core.java_index)
        .await
}

#[tauri::command]
pub async fn load_java_index(project_path: String, core: Core<'_>) -> Result<u32, String> {
    nib_core::java_index::load_java_index(project_path, core.dirs.java_index(), &core.java_index)
        .await
}

#[tauri::command]
pub fn search_java_class(
    project_path: String,
    class_name: String,
    core: Core<'_>,
) -> Result<Vec<nib_core::java_index::ClassLocation>, String> {
    nib_core::java_index::search_java_class(project_path, class_name, &core.java_index)
}

#[tauri::command]
pub async fn update_java_index_file(
    project_path: String,
    file_path: String,
    core: Core<'_>,
) -> Result<(), String> {
    nib_core::java_index::update_java_index_file(
        project_path,
        file_path,
        core.dirs.java_index(),
        &core.java_index,
    )
    .await
}

#[tauri::command]
pub async fn remove_java_index_file(
    project_path: String,
    file_path: String,
    core: Core<'_>,
) -> Result<(), String> {
    nib_core::java_index::remove_java_index_file(
        project_path,
        file_path,
        core.dirs.java_index(),
        &core.java_index,
    )
    .await
}

// ---- Usage index ----

#[tauri::command]
pub async fn build_usage_index(project_path: String, core: Core<'_>) -> Result<u32, String> {
    nib_core::usage_index::build_usage_index(
        project_path,
        core.dirs.usage_index(),
        &core.usage_index,
    )
    .await
}

#[tauri::command]
pub async fn query_usages(
    project_path: String,
    symbol: String,
    limit: usize,
    core: Core<'_>,
) -> Result<Vec<nib_core::usage_index::Usage>, String> {
    nib_core::usage_index::query_usages(project_path, symbol, limit, &core.usage_index).await
}

#[tauri::command]
pub async fn update_usage_index_file(
    project_path: String,
    file_path: String,
    core: Core<'_>,
) -> Result<(), String> {
    nib_core::usage_index::update_usage_index_file(project_path, file_path, &core.usage_index).await
}

#[tauri::command]
pub fn remove_usage_index_file(
    project_path: String,
    file_path: String,
    core: Core<'_>,
) -> Result<(), String> {
    nib_core::usage_index::remove_usage_index_file(project_path, file_path, &core.usage_index)
}

// ---- Astore(reqwest 异步,经 rt::run 落 core runtime) ----

#[tauri::command]
pub async fn astore_login(
    username: String,
    password: String,
    core: Core<'_>,
) -> Result<nib_core::astore::AstoreSession, String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move { nib_core::astore::astore_login(&core.astore, username, password).await })
        .await
}

#[tauri::command]
pub async fn astore_logout(core: Core<'_>) -> Result<(), String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move { nib_core::astore::astore_logout(&core.astore).await }).await
}

#[tauri::command]
pub async fn astore_get_session(core: Core<'_>) -> Result<nib_core::astore::AstoreSession, String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move { nib_core::astore::astore_get_session(&core.astore).await }).await
}

#[tauri::command]
pub async fn astore_detect_project(
    project_path: String,
    core: Core<'_>,
) -> Result<nib_core::astore::AstoreSession, String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move {
        nib_core::astore::astore_detect_project(&core.astore, project_path).await
    })
    .await
}

#[tauri::command]
pub async fn astore_set_env(env: String, core: Core<'_>) -> Result<(), String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move { nib_core::astore::astore_set_env(&core.astore, env).await }).await
}

#[tauri::command]
pub async fn astore_sync_code(
    git_url: String,
    branch: String,
    core: Core<'_>,
) -> Result<String, String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move {
        nib_core::astore::astore_sync_code(&core.astore, git_url, branch).await
    })
    .await
}

#[tauri::command]
pub async fn astore_publish(core: Core<'_>) -> Result<String, String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move { nib_core::astore::astore_publish(&core.astore).await }).await
}

#[tauri::command]
pub async fn astore_get_editor_url(core: Core<'_>) -> Result<String, String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move { nib_core::astore::astore_get_editor_url(&core.astore).await })
        .await
}

#[tauri::command]
pub async fn astore_get_record_url(core: Core<'_>) -> Result<String, String> {
    let core = core.inner().clone();
    nib_core::rt::run(async move { nib_core::astore::astore_get_record_url(&core.astore).await })
        .await
}
