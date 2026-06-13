// Maven 依赖分析:跑 mvn dependency:tree 并在 Rust 侧完成解析/冲突检测/扁平化,
// 以及 pom.xml 的 exclusion 外科手术编辑。解析逻辑自前端 maven-helper.ts 迁入
// (数据产生端与解析端同侧),两处申报过的行为修正:
// 1. 深度计算保留前导空格——TS 正则贪婪 \s* 会把 last-child 后代的纯空格缩进吃掉,
//    导致这类节点深度算浅、挂错父节点;
// 2. direct_parent = depth-1 直接依赖——TS findDirectParent off-by-one 返回 depth-0
//    模块自身,exclude 在 pom 里搜模块自身 GAV 永远失败。

use crate::events::{CoreEvent, EventSink};
use crate::rt::on_worker;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::Arc;

#[derive(Serialize, Clone)]
pub struct DepCoordRef {
    pub group_id: String,
    pub artifact_id: String,
}

#[derive(Serialize)]
pub struct DepNode {
    group_id: String,
    artifact_id: String,
    version: String,
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    omitted_for: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direct_parent: Option<DepCoordRef>,
    children: Vec<DepNode>,
}

#[derive(Serialize)]
pub struct ConflictNode {
    group_id: String,
    artifact_id: String,
    version: String,
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    omitted_for: Option<String>,
    /// 祖先 artifactId 链(模块根 → 直接父),冲突视图展示用
    dep_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direct_parent: Option<DepCoordRef>,
}

#[derive(Serialize)]
pub struct MavenConflict {
    group_id: String,
    artifact_id: String,
    versions: Vec<String>,
    nodes: Vec<ConflictNode>,
}

#[derive(Serialize)]
pub struct MavenFlatDep {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
    pub scope: String,
    pub is_conflict: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted_for: Option<String>,
}

#[derive(Serialize)]
pub struct MavenDepTree {
    pub exit_code: i32,
    /// exit_code != 0 时为 None;成功时是 synthetic 根(空坐标,children = 各模块根)
    pub root: Option<DepNode>,
    pub conflicts: Vec<MavenConflict>,
    pub flat: Vec<MavenFlatDep>,
}

// ---- 坐标解析 ----

// 坐标里的 type/classifier 段只参与切分定位,不进 DTO——TS 渲染端从不消费
struct RawCoord {
    group_id: String,
    artifact_id: String,
    version: String,
    scope: String,
    omitted_for: Option<String>,
}

fn parse_dep_coord(raw: &str) -> Option<RawCoord> {
    let mut text = raw.trim().to_string();
    let mut omitted_for: Option<String> = None;

    // 括号包裹的省略项:"(g:a:type:ver:scope - omitted for conflict with X)"
    if text.starts_with('(') && text.ends_with(')') {
        text = text[1..text.len() - 1].to_string();
        const MARKER: &str = "omitted for conflict with";
        if let Some(pos) = text.rfind(MARKER) {
            let before = text[..pos].trim_end();
            let after = text[pos + MARKER.len()..].trim();
            if before.ends_with('-')
                && !after.is_empty()
                && after
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'))
            {
                omitted_for = Some(after.to_string());
                text = before[..before.len() - 1].trim_end().to_string();
            }
        }
    }

    // 其余尾部括号说明,如 "(version managed from X)"
    let t = text.trim_end();
    if t.ends_with(')') {
        if let Some(i) = t.find('(') {
            text = t[..i].trim_end().to_string();
        }
    }

    let parts: Vec<&str> = text.split(':').collect();
    // 真实坐标段不含空白;挡住 "Finished at: 2026-06-10T12:34:56+08:00" 这类
    // 恰好被冒号切成 4-5 段的收尾行,否则会变成树里的假节点
    if parts
        .iter()
        .any(|p| p.is_empty() || p.chars().any(char::is_whitespace))
    {
        return None;
    }
    let coord = |g: &str, a: &str, v: &str, s: &str| RawCoord {
        group_id: g.to_string(),
        artifact_id: a.to_string(),
        version: v.to_string(),
        scope: s.to_string(),
        omitted_for: omitted_for.clone(),
    };
    match parts.len() {
        // groupId:artifactId:type:version[:scope] / groupId:artifactId:type:classifier:version:scope
        4 => Some(coord(parts[0], parts[1], parts[3], "compile")),
        5 => Some(coord(parts[0], parts[1], parts[3], parts[4])),
        n if n >= 6 => Some(coord(parts[0], parts[1], parts[4], parts[5])),
        _ => None,
    }
}

// ---- 树解析(扁平节点 + 父指针,便于算 dep_path / direct_parent) ----

struct RawNode {
    coord: RawCoord,
    parent: Option<usize>,
    /// 路径上的 depth-1 祖先(真实声明在 pom 里的直接依赖);自身 depth<=1 时无
    direct_parent: Option<usize>,
    children: Vec<usize>,
}

fn parse_tree_nodes(output: &str) -> Vec<RawNode> {
    let mut nodes: Vec<RawNode> = Vec::new();
    // stack[i] = 当前路径上第 i 层节点的下标
    let mut stack: Vec<usize> = Vec::new();

    for raw_line in output.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let Some(rest) = line.strip_prefix("[INFO]") else {
            continue;
        };
        let rest = rest.strip_prefix(' ').unwrap_or(rest);

        // 树前缀每层 3 字符("+- " / "|  " / "\- " / "   ");坐标以字母/数字/'(' 开头
        let coord_start = rest
            .find(|c: char| !matches!(c, '|' | '+' | '\\' | '-' | ' '))
            .unwrap_or(rest.len());
        let tree_prefix = &rest[..coord_start];
        let coord_str = &rest[coord_start..];

        if coord_str.starts_with("BUILD")
            || coord_str.starts_with("Downloading")
            || coord_str.starts_with("Downloaded")
            || coord_str.starts_with("Verbose")
            || coord_str.trim().is_empty()
            || !coord_str.contains(':')
        {
            continue;
        }
        let Some(coord) = parse_dep_coord(coord_str) else {
            continue;
        };

        let prefix_clean_len = tree_prefix.trim_end_matches(' ').len();
        let depth = if prefix_clean_len == 0 {
            0
        } else {
            (prefix_clean_len + 1) / 3
        };

        stack.truncate(depth);
        let parent = stack.last().copied();
        let direct_parent = if stack.len() >= 2 { Some(stack[1]) } else { None };

        let idx = nodes.len();
        nodes.push(RawNode {
            coord,
            parent,
            direct_parent,
            children: Vec::new(),
        });
        if let Some(p) = parent {
            nodes[p].children.push(idx);
        }
        stack.push(idx);
    }

    nodes
}

fn coord_ref(nodes: &[RawNode], idx: usize) -> DepCoordRef {
    DepCoordRef {
        group_id: nodes[idx].coord.group_id.clone(),
        artifact_id: nodes[idx].coord.artifact_id.clone(),
    }
}

/// 祖先 artifactId 链:模块根 → … → 直接父
fn dep_path(nodes: &[RawNode], idx: usize) -> Vec<String> {
    let mut path = Vec::new();
    let mut cur = nodes[idx].parent;
    while let Some(p) = cur {
        path.push(nodes[p].coord.artifact_id.clone());
        cur = nodes[p].parent;
    }
    path.reverse();
    path
}

fn build_node(nodes: &[RawNode], idx: usize) -> DepNode {
    let n = &nodes[idx];
    DepNode {
        group_id: n.coord.group_id.clone(),
        artifact_id: n.coord.artifact_id.clone(),
        version: n.coord.version.clone(),
        scope: n.coord.scope.clone(),
        omitted_for: n.coord.omitted_for.clone(),
        direct_parent: n.direct_parent.map(|d| coord_ref(nodes, d)),
        children: n.children.iter().map(|&c| build_node(nodes, c)).collect(),
    }
}

pub(crate) fn build_dep_tree(exit_code: i32, output: &str) -> MavenDepTree {
    let nodes = parse_tree_nodes(output);

    // 冲突检测:同 groupId:artifactId 出现多版本,或任一节点带 omitted_for
    let mut key_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        let key = format!("{}:{}", n.coord.group_id, n.coord.artifact_id);
        let entry = groups.entry(key.clone()).or_default();
        if entry.is_empty() {
            key_order.push(key);
        }
        entry.push(i);
    }

    let mut conflicts: Vec<MavenConflict> = Vec::new();
    for key in &key_order {
        let idxs = &groups[key];
        let mut versions: Vec<String> = Vec::new();
        for &i in idxs {
            let v = &nodes[i].coord.version;
            if !versions.contains(v) {
                versions.push(v.clone());
            }
        }
        let has_omitted = idxs.iter().any(|&i| nodes[i].coord.omitted_for.is_some());
        if versions.len() > 1 || has_omitted {
            for &i in idxs {
                if let Some(o) = &nodes[i].coord.omitted_for {
                    if !versions.contains(o) {
                        versions.push(o.clone());
                    }
                }
            }
            let (group_id, artifact_id) = key.split_once(':').unwrap();
            conflicts.push(MavenConflict {
                group_id: group_id.to_string(),
                artifact_id: artifact_id.to_string(),
                versions,
                nodes: idxs
                    .iter()
                    .map(|&i| ConflictNode {
                        group_id: nodes[i].coord.group_id.clone(),
                        artifact_id: nodes[i].coord.artifact_id.clone(),
                        version: nodes[i].coord.version.clone(),
                        scope: nodes[i].coord.scope.clone(),
                        omitted_for: nodes[i].coord.omitted_for.clone(),
                        dep_path: dep_path(&nodes, i),
                        direct_parent: nodes[i].direct_parent.map(|d| coord_ref(&nodes, d)),
                    })
                    .collect(),
            });
        }
    }
    // 大小写不敏感字典序(近似 TS localeCompare 的 ASCII 行为),原串 tie-break
    conflicts.sort_by_cached_key(|c| (c.artifact_id.to_lowercase(), c.artifact_id.clone()));

    let conflict_keys: HashSet<String> = conflicts
        .iter()
        .map(|c| format!("{}:{}", c.group_id, c.artifact_id))
        .collect();

    // 扁平去重列表:g:a:v 首见为准
    let mut seen: HashSet<String> = HashSet::new();
    let mut flat: Vec<MavenFlatDep> = Vec::new();
    for n in &nodes {
        let key = format!(
            "{}:{}:{}",
            n.coord.group_id, n.coord.artifact_id, n.coord.version
        );
        if seen.insert(key) {
            flat.push(MavenFlatDep {
                group_id: n.coord.group_id.clone(),
                artifact_id: n.coord.artifact_id.clone(),
                version: n.coord.version.clone(),
                scope: n.coord.scope.clone(),
                is_conflict: conflict_keys
                    .contains(&format!("{}:{}", n.coord.group_id, n.coord.artifact_id)),
                omitted_for: n.coord.omitted_for.clone(),
            });
        }
    }
    flat.sort_by_cached_key(|f| (f.artifact_id.to_lowercase(), f.artifact_id.clone()));

    // synthetic 根:空坐标,children = 各 depth-0 模块根(多模块 reactor 多棵树)
    let root_children: Vec<DepNode> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent.is_none())
        .map(|(i, _)| build_node(&nodes, i))
        .collect();
    let root = DepNode {
        group_id: String::new(),
        artifact_id: String::new(),
        version: String::new(),
        scope: "compile".to_string(),
        omitted_for: None,
        direct_parent: None,
        children: root_children,
    };

    MavenDepTree {
        exit_code,
        root: Some(root),
        conflicts,
        flat,
    }
}

/// 用户可配置的 Maven 信息(像 IDEA)。空字段=用默认。
#[derive(Clone, Default)]
pub struct MavenConfig {
    /// Maven home(含 bin/mvn 的目录,如 ~/amaven-3.5.0)。空=用 PATH 里的 mvn。
    pub home: String,
    /// settings.xml(内网仓库配置)。空=mvn 默认 ~/.m2/settings.xml。
    pub settings: String,
    /// 本地仓库目录。空=mvn 默认 ~/.m2/repository。
    pub repo: String,
}

impl MavenConfig {
    /// 解析 mvn 可执行:配了 home 用 {home}/bin/mvn,否则用 PATH 的 mvn。
    fn mvn_bin(&self) -> String {
        let h = self.home.trim();
        if h.is_empty() {
            "mvn".to_string()
        } else {
            format!("{}/bin/mvn", h.trim_end_matches('/'))
        }
    }
    /// 附加 -s settings / -Dmaven.repo.local 参数。
    fn extra_args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if !self.settings.trim().is_empty() {
            a.push("-s".into());
            a.push(self.settings.trim().to_string());
        }
        if !self.repo.trim().is_empty() {
            a.push(format!("-Dmaven.repo.local={}", self.repo.trim()));
        }
        a
    }
}

pub async fn maven_dependency_tree(
    project_path: String,
    cfg: MavenConfig,
) -> Result<MavenDepTree, String> {
    on_worker(move || {
        let result = Command::new(cfg.mvn_bin())
            .arg("dependency:tree")
            .args(cfg.extra_args())
            .current_dir(&project_path)
            // Dock 启动的 app 拿不到 /opt/homebrew/bin,mvn 找不到(同 jdtls PATH 问题);
            // 且 mvn 脚本要 JAVA_HOME 才能跑(实测仅 java 在 PATH 不够),显式设上。
            .env("PATH", crate::lsp::augmented_path())
            .env("JAVA_HOME", crate::lsp::java_home())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("Failed to run mvn: {}", e))?;

        let exit_code = result.status.code().unwrap_or(-1);
        if exit_code != 0 {
            return Ok(MavenDepTree {
                exit_code,
                root: None,
                conflicts: Vec::new(),
                flat: Vec::new(),
            });
        }
        let stdout = String::from_utf8_lossy(&result.stdout);
        Ok(build_dep_tree(exit_code, &stdout))
    })
    .await
}

// ---- pom.xml exclusion 外科手术编辑 ----

pub(crate) fn add_exclusion(
    content: &str,
    parent_group_id: &str,
    parent_artifact_id: &str,
    exclude_group_id: &str,
    exclude_artifact_id: &str,
) -> Result<String, String> {
    let mut lines: Vec<String> = content.split('\n').map(String::from).collect();

    // 找匹配 parent GAV 的 <dependency> 块
    let mut dep_start: Option<usize> = None;
    let mut dep_end: Option<usize> = None;
    let mut found_group = false;
    let mut found_artifact = false;
    let mut in_exclusions = false;
    let group_tag = format!("<groupId>{}</groupId>", parent_group_id);
    let artifact_tag = format!("<artifactId>{}</artifactId>", parent_artifact_id);

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "<dependency>" {
            dep_start = Some(i);
            found_group = false;
            found_artifact = false;
            in_exclusions = false;
        }
        if dep_start.is_some() {
            if trimmed == "<exclusions>" {
                in_exclusions = true;
            }
            if trimmed == "</exclusions>" {
                in_exclusions = false;
            }
            // <exclusions> 块里的 groupId/artifactId 是排除项不是依赖本身——
            // 否则别的依赖排除过目标 GAV 时,exclusion 会被插进错误的 <dependency>
            if !in_exclusions {
                if trimmed == group_tag {
                    found_group = true;
                }
                if trimmed == artifact_tag {
                    found_artifact = true;
                }
            }
            if trimmed == "</dependency>" {
                if found_group && found_artifact {
                    dep_end = Some(i);
                    break;
                }
                dep_start = None;
            }
        }
    }

    let (Some(ds), Some(de)) = (dep_start, dep_end) else {
        return Err(format!(
            "Cannot find <dependency> for {}:{}",
            parent_group_id, parent_artifact_id
        ));
    };

    // 探测缩进
    let dep_indent: String = lines[ds]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let child_indent = format!("{}    ", dep_indent);
    let grand_child_indent = format!("{}    ", child_indent);

    // 该块内已有 <exclusions> 吗
    let mut exclusions_start: Option<usize> = None;
    let mut exclusions_end: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().take(de + 1).skip(ds) {
        if line.trim() == "<exclusions>" {
            exclusions_start = Some(i);
        }
        if line.trim() == "</exclusions>" {
            exclusions_end = Some(i);
        }
    }

    let exclusion_block = vec![
        format!("{}<exclusion>", grand_child_indent),
        format!(
            "{}    <groupId>{}</groupId>",
            grand_child_indent, exclude_group_id
        ),
        format!(
            "{}    <artifactId>{}</artifactId>",
            grand_child_indent, exclude_artifact_id
        ),
        format!("{}</exclusion>", grand_child_indent),
    ];

    if let (Some(_), Some(ee)) = (exclusions_start, exclusions_end) {
        // 插到 </exclusions> 之前
        lines.splice(ee..ee, exclusion_block);
    } else {
        // 在 </dependency> 之前插入整个 <exclusions> 块
        let mut block = vec![format!("{}<exclusions>", child_indent)];
        block.extend(exclusion_block);
        block.push(format!("{}</exclusions>", child_indent));
        lines.splice(de..de, block);
    }

    Ok(lines.join("\n"))
}

pub async fn maven_add_exclusion(
    pom_path: String,
    parent_group_id: String,
    parent_artifact_id: String,
    exclude_group_id: String,
    exclude_artifact_id: String,
) -> Result<(), String> {
    on_worker(move || {
        let content = std::fs::read_to_string(&pom_path)
            .map_err(|e| format!("Failed to read {}: {}", pom_path, e))?;
        let updated = add_exclusion(
            &content,
            &parent_group_id,
            &parent_artifact_id,
            &exclude_group_id,
            &exclude_artifact_id,
        )?;
        std::fs::write(&pom_path, updated).map_err(|e| format!("Failed to write {}: {}", pom_path, e))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[INFO] Scanning for projects...
[INFO] --- maven-dependency-plugin:3.1.1:tree (default-cli) @ app ---
[INFO] com.example:app:jar:1.0.0
[INFO] +- org.alpha:lib-a:jar:1.0:compile
[INFO] |  +- org.beta:lib-b:jar:2.0:compile
[INFO] |  \\- (org.gamma:lib-c:jar:1.5:compile - omitted for conflict with 2.0)
[INFO] \\- org.gamma:lib-c:jar:2.0:test
[INFO]    \\- org.delta:lib-d:jar:3.0:test
[INFO] BUILD SUCCESS
[INFO] Total time: 2.5 s
";

    #[test]
    fn parses_tree_shape_with_last_child_descendants() {
        let tree = build_dep_tree(0, SAMPLE);
        let root = tree.root.as_ref().unwrap();
        assert_eq!(root.children.len(), 1, "单模块 → synthetic 根下一棵树");
        let module = &root.children[0];
        assert_eq!(module.artifact_id, "app");
        assert_eq!(module.children.len(), 2);
        let lib_a = &module.children[0];
        assert_eq!(lib_a.children.len(), 2);
        // 行为修正:`[INFO]    \- lib-d` 的纯空格缩进保留 → lib-d 是 lib-c 的孩子
        let lib_c = &module.children[1];
        assert_eq!(lib_c.artifact_id, "lib-c");
        assert_eq!(lib_c.children.len(), 1);
        assert_eq!(lib_c.children[0].artifact_id, "lib-d");
    }

    #[test]
    fn direct_parent_is_depth1_ancestor() {
        let tree = build_dep_tree(0, SAMPLE);
        let module = &tree.root.unwrap().children[0];
        // depth-1 节点自身无 direct_parent
        assert!(module.children[0].direct_parent.is_none());
        // depth-2 节点的 direct_parent = depth-1 直接依赖
        let lib_b = &module.children[0].children[0];
        assert_eq!(lib_b.direct_parent.as_ref().unwrap().artifact_id, "lib-a");
        let lib_d = &module.children[1].children[0];
        assert_eq!(lib_d.direct_parent.as_ref().unwrap().artifact_id, "lib-c");
    }

    #[test]
    fn detects_conflicts_with_versions_and_dep_path() {
        let tree = build_dep_tree(0, SAMPLE);
        assert_eq!(tree.conflicts.len(), 1);
        let c = &tree.conflicts[0];
        assert_eq!(c.artifact_id, "lib-c");
        assert_eq!(c.versions, vec!["1.5", "2.0"]);
        assert_eq!(c.nodes.len(), 2);
        let omitted = c.nodes.iter().find(|n| n.omitted_for.is_some()).unwrap();
        assert_eq!(omitted.omitted_for.as_deref(), Some("2.0"));
        assert_eq!(omitted.dep_path, vec!["app", "lib-a"]);
        assert_eq!(omitted.direct_parent.as_ref().unwrap().artifact_id, "lib-a");
    }

    #[test]
    fn flat_dedups_and_marks_conflicts() {
        let tree = build_dep_tree(0, SAMPLE);
        // app, lib-a, lib-b, lib-c:1.5, lib-c:2.0, lib-d
        assert_eq!(tree.flat.len(), 6);
        let lib_c_entries: Vec<_> = tree.flat.iter().filter(|f| f.artifact_id == "lib-c").collect();
        assert_eq!(lib_c_entries.len(), 2);
        assert!(lib_c_entries.iter().all(|f| f.is_conflict));
        assert!(!tree.flat.iter().find(|f| f.artifact_id == "lib-b").unwrap().is_conflict);
    }

    #[test]
    fn parses_coord_variants() {
        let c = parse_dep_coord("g:a:jar:1.0").unwrap();
        assert_eq!((c.version.as_str(), c.scope.as_str()), ("1.0", "compile"));
        let c = parse_dep_coord("g:a:jar:1.0:test").unwrap();
        assert_eq!((c.version.as_str(), c.scope.as_str()), ("1.0", "test"));
        // 6 段:classifier 在第 4 段
        let c = parse_dep_coord("g:a:jar:linux-x86_64:2.0:runtime").unwrap();
        assert_eq!((c.version.as_str(), c.scope.as_str()), ("2.0", "runtime"));
        // 尾部管理说明剥除
        let c = parse_dep_coord("g:a:jar:1.0:compile (version managed from 2.0)").unwrap();
        assert_eq!(c.version, "1.0");
        assert!(parse_dep_coord("g:a").is_none());
        // mvn 收尾行恰好被冒号切成 5 段,不能当坐标
        assert!(parse_dep_coord("Finished at: 2026-06-10T12:34:56+08:00").is_none());
    }

    #[test]
    fn build_summary_lines_do_not_become_nodes() {
        let out = "\
[INFO] com.example:app:jar:1.0
[INFO] +- org.x:dep-x:jar:1.0:compile
[INFO] BUILD SUCCESS
[INFO] Total time:  2.5 s
[INFO] Finished at: 2026-06-10T12:34:56+08:00
";
        let tree = build_dep_tree(0, out);
        assert_eq!(tree.flat.len(), 2, "只有 app 和 dep-x,无假节点");
    }

    #[test]
    fn exclusion_lines_in_other_dependency_do_not_match() {
        // lib-x 的排除项里出现 org.alpha:lib-a,不能把 lib-x 误认成 lib-a 的依赖块
        let pom = "\
<project>
    <dependencies>
        <dependency>
            <groupId>org.foo</groupId>
            <artifactId>lib-x</artifactId>
            <exclusions>
                <exclusion>
                    <groupId>org.alpha</groupId>
                    <artifactId>lib-a</artifactId>
                </exclusion>
            </exclusions>
        </dependency>
        <dependency>
            <groupId>org.alpha</groupId>
            <artifactId>lib-a</artifactId>
        </dependency>
    </dependencies>
</project>";
        let updated = add_exclusion(pom, "org.alpha", "lib-a", "org.gamma", "lib-c").unwrap();
        let lib_a_dep = updated.find("<artifactId>lib-a</artifactId>\n        </dependency>");
        assert!(lib_a_dep.is_none(), "lib-a 块应已插入 exclusions 而非保持原样");
        // 新 exclusion 落在第二个 <dependency>(lib-a 自己的块)里
        let second_dep = updated.rfind("<dependency>").unwrap();
        let gamma = updated.find("org.gamma").unwrap();
        assert!(gamma > second_dep, "exclusion 必须插进 lib-a 的块,不是 lib-x 的");
    }

    #[test]
    fn multi_module_reactor_yields_multiple_roots() {
        let out = "\
[INFO] com.example:mod-a:jar:1.0
[INFO] +- org.x:dep-x:jar:1.0:compile
[INFO] com.example:mod-b:jar:1.0
[INFO] \\- org.y:dep-y:jar:1.0:compile
";
        let tree = build_dep_tree(0, out);
        assert_eq!(tree.root.unwrap().children.len(), 2);
    }

    #[test]
    fn empty_output_yields_empty_root() {
        let tree = build_dep_tree(0, "[INFO] BUILD SUCCESS\n");
        assert!(tree.root.unwrap().children.is_empty());
        assert!(tree.conflicts.is_empty());
        assert!(tree.flat.is_empty());
    }

    const POM: &str = "\
<project>
    <dependencies>
        <dependency>
            <groupId>org.alpha</groupId>
            <artifactId>lib-a</artifactId>
            <version>1.0</version>
        </dependency>
    </dependencies>
</project>";

    #[test]
    fn inserts_new_exclusions_block_with_indent() {
        let updated = add_exclusion(POM, "org.alpha", "lib-a", "org.gamma", "lib-c").unwrap();
        let expected = "\
<project>
    <dependencies>
        <dependency>
            <groupId>org.alpha</groupId>
            <artifactId>lib-a</artifactId>
            <version>1.0</version>
            <exclusions>
                <exclusion>
                    <groupId>org.gamma</groupId>
                    <artifactId>lib-c</artifactId>
                </exclusion>
            </exclusions>
        </dependency>
    </dependencies>
</project>";
        assert_eq!(updated, expected);
    }

    #[test]
    fn appends_into_existing_exclusions_block() {
        let pom = "\
<project>
    <dependencies>
        <dependency>
            <groupId>org.alpha</groupId>
            <artifactId>lib-a</artifactId>
            <exclusions>
                <exclusion>
                    <groupId>org.old</groupId>
                    <artifactId>lib-old</artifactId>
                </exclusion>
            </exclusions>
        </dependency>
    </dependencies>
</project>";
        let updated = add_exclusion(pom, "org.alpha", "lib-a", "org.gamma", "lib-c").unwrap();
        let new_pos = updated.find("lib-c").unwrap();
        let old_pos = updated.find("lib-old").unwrap();
        let close_pos = updated.find("</exclusions>").unwrap();
        assert!(old_pos < new_pos && new_pos < close_pos, "新 exclusion 追加在既有项之后、闭合标签之前");
    }

    #[test]
    fn missing_dependency_block_is_err() {
        let err = add_exclusion(POM, "org.nope", "lib-nope", "g", "a").unwrap_err();
        assert!(err.contains("org.nope:lib-nope"));
    }

    #[test]
    fn crlf_pom_lines_still_match() {
        let pom_crlf = POM.replace('\n', "\r\n");
        let updated = add_exclusion(&pom_crlf, "org.alpha", "lib-a", "org.gamma", "lib-c").unwrap();
        assert!(updated.contains("<exclusion>"));
        // 既有行的 \r 原样保留(join("\n") 不重写老行)
        assert!(updated.contains("<version>1.0</version>\r"));
    }
}

// ---- Maven 模块扫描与构建(自 src-tauri commands.rs 迁入,逻辑不变) ----

#[derive(Serialize)]
pub struct MavenModule {
    pub name: String,
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
    pub packaging: String,
    pub pom_path: String,
    pub modules: Vec<String>,
}

pub async fn parse_maven_modules(project_path: String) -> Result<Vec<MavenModule>, String> {
    on_worker(move || {
        let mut modules = Vec::new();

        for entry in walkdir::WalkDir::new(&project_path)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_name() == "pom.xml" {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    if let Some(module) =
                        parse_pom(&content, entry.path().to_string_lossy().to_string())
                    {
                        modules.push(module);
                    }
                }
            }
        }

        Ok(modules)
    })
    .await
}

fn local_tag_name(raw: &[u8]) -> String {
    let full = String::from_utf8_lossy(raw).to_string();
    full.rsplit_once(':')
        .map_or(full.clone(), |(_, local)| local.to_string())
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
                let tag = local_tag_name(e.name().as_ref());
                if tag == "parent" {
                    in_parent = true;
                }
                if tag == "modules" {
                    in_modules = true;
                }
                current_tag = tag;
            }
            Ok(Event::End(e)) => {
                let tag = local_tag_name(e.name().as_ref());
                if tag == "parent" {
                    in_parent = false;
                }
                if tag == "modules" {
                    in_modules = false;
                }
                depth -= 1;
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().trim().to_string();
                if !in_parent && depth == 2 {
                    match current_tag.as_str() {
                        "groupId" => group_id = text.clone(),
                        "artifactId" => artifact_id = text.clone(),
                        "version" => version = text.clone(),
                        "packaging" => packaging = text.clone(),
                        _ => {}
                    }
                }
                if in_modules && current_tag == "module" && !text.is_empty() {
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

/// 流式跑 mvn:stdout/stderr 逐行经 EventSink 推送,结束推 MavenDone。
/// (低频构建场景;高频洪峰的批量收编是 UI 侧职责,见 RFC v2 §3)
pub fn run_maven_command(
    project_path: String,
    goals: Vec<String>,
    events: Arc<dyn EventSink>,
) -> Result<(), String> {
    let mut child = Command::new("mvn")
        .args(&goals)
        .current_dir(&project_path)
        .env("PATH", crate::lsp::augmented_path())
        .env("JAVA_HOME", crate::lsp::java_home())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run mvn: {}", e))?;

    let stdout = child.stdout.take().ok_or("Failed to capture mvn stdout")?;
    let stderr = child.stderr.take().ok_or("Failed to capture mvn stderr")?;

    let ev_out = events.clone();
    let ev_err = events.clone();

    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            ev_out.emit(CoreEvent::MavenOutput(line));
        }
    });

    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            ev_err.emit(CoreEvent::MavenOutput(line));
        }
    });

    std::thread::spawn(move || {
        let status = child.wait();
        let code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        events.emit(CoreEvent::MavenDone(code));
    });

    Ok(())
}
