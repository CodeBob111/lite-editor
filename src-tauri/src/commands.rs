use serde::Serialize;
use std::path::Path;
use std::process::Command;
use walkdir::WalkDir;

#[derive(Serialize, Clone)]
pub struct FileNode {
    name: String,
    path: String,
    is_dir: bool,
    children: Option<Vec<FileNode>>,
}

#[tauri::command]
pub fn read_dir_tree(path: String, max_depth: Option<usize>) -> Result<FileNode, String> {
    let root = Path::new(&path);
    if !root.exists() {
        return Err(format!("Path does not exist: {}", path));
    }
    build_tree(root, max_depth.unwrap_or(4), 0).ok_or_else(|| "Failed to read directory".into())
}

fn build_tree(path: &Path, max_depth: usize, current_depth: usize) -> Option<FileNode> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    if should_skip(&name) {
        return None;
    }

    if path.is_dir() {
        let children = if current_depth < max_depth {
            let mut entries: Vec<FileNode> = std::fs::read_dir(path)
                .ok()?
                .filter_map(|e| e.ok())
                .filter_map(|e| build_tree(&e.path(), max_depth, current_depth + 1))
                .collect();
            entries.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))
            });
            Some(entries)
        } else {
            Some(vec![])
        };

        Some(FileNode {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir: true,
            children,
        })
    } else {
        Some(FileNode {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir: false,
            children: None,
        })
    }
}

fn should_skip(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".idea" | ".vscode" | "node_modules" | "target" | ".DS_Store" | "__pycache__"
    )
}

#[tauri::command]
pub fn read_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {}", path, e))
}

#[tauri::command]
pub fn write_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, &content).map_err(|e| format!("Failed to write {}: {}", path, e))
}

#[derive(Serialize)]
pub struct MavenModule {
    name: String,
    group_id: String,
    artifact_id: String,
    version: String,
    packaging: String,
    pom_path: String,
    modules: Vec<String>,
}

#[tauri::command]
pub fn parse_maven_modules(project_path: String) -> Result<Vec<MavenModule>, String> {
    let mut modules = Vec::new();

    for entry in WalkDir::new(&project_path)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() == "pom.xml" {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Some(module) = parse_pom(&content, entry.path().to_string_lossy().to_string()) {
                    modules.push(module);
                }
            }
        }
    }

    Ok(modules)
}

fn parse_pom(content: &str, pom_path: String) -> Option<MavenModule> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(content);
    let mut buf = Vec::new();
    let mut current_tag = String::new();
    let mut group_id = String::new();
    let mut artifact_id = String::new();
    let mut version = String::new();
    let mut packaging = String::from("jar");
    let mut child_modules = Vec::new();
    let mut depth = 0;
    let mut in_parent = false;
    let mut in_modules = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "parent" { in_parent = true; }
                if tag == "modules" { in_modules = true; }
                current_tag = tag;
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "parent" { in_parent = false; }
                if tag == "modules" { in_modules = false; }
                depth -= 1;
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                if !in_parent && depth == 2 {
                    match current_tag.as_str() {
                        "groupId" => group_id = text.clone(),
                        "artifactId" => artifact_id = text.clone(),
                        "version" => version = text.clone(),
                        "packaging" => packaging = text.clone(),
                        _ => {}
                    }
                }
                if in_modules && current_tag == "module" {
                    child_modules.push(text);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    if artifact_id.is_empty() {
        return None;
    }

    Some(MavenModule {
        name: artifact_id.clone(),
        group_id,
        artifact_id,
        version,
        packaging,
        pom_path,
        modules: child_modules,
    })
}

#[tauri::command]
pub fn run_maven_command(project_path: String, goals: Vec<String>) -> Result<String, String> {
    let output = Command::new("mvn")
        .args(&goals)
        .current_dir(&project_path)
        .output()
        .map_err(|e| format!("Failed to run mvn: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!("{}\n{}", stdout, stderr))
    }
}
