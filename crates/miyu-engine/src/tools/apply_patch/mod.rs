use super::{ToolProgress, ToolRegistry, ToolSpec};
use crate::tools::patch_preview::write_with_patch_preview;
use anyhow::{bail, Result};
use miyu_base::i18n::text as t;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

/// 三个存储域各一件补丁工具(08-21 二次裁定):`edit`=文件系统、`artifact`=
/// Artifact 库、`kb`=知识库。第一版把三域折进 edit 的路径前缀,实测模型对
/// "存进知识库"想不起 kb: 前缀,反被 remember_fact 的名字劫走——工具名才是
/// 最强的能力广告,域内聚合、域间分名。
pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new_with_progress(
        "edit",
        "Apply a batch patch to files. Prefer this for complex edits and multiple changes in the same file.",
        patch_parameters(),
        move |args, progress| async move {
            progress.report(format!(
                "__tool_phase__~ {}",
                t("prepare patch", "准备修改")
            ));
            tokio::task::yield_now().await;
            edit_filesystem(args, progress)
        },
    ).writes());
}

/// 知识库补丁工具。写入全部路由 KnowledgeBase(元数据/语义索引不绕过)。
pub fn register_kb(
    registry: &mut ToolRegistry,
    config: miyu_base::config::AppConfig,
    paths: miyu_base::paths::MiyuPaths,
) {
    registry.register(ToolSpec::new_with_progress(
        "kb",
        "Write knowledge-base files: create, update, or delete via patch. Paths are knowledge-base relative. The knowledge base holds persistent reference documents, not Miyu's memories (those go through remember_fact). Read entries with read using kb: paths; search with search_knowledge_base.",
        patch_parameters(),
        move |args, progress| {
            let config = config.clone();
            let paths = paths.clone();
            async move {
                progress.report(format!(
                    "__tool_phase__~ {}",
                    t("prepare patch", "准备修改")
                ));
                tokio::task::yield_now().await;
                apply_kb_patch(args, progress, &config, &paths)
            }
        },
    ).writes());
}

/// Artifact 库补丁工具(WebUI 会话注册)。
pub fn register_artifact(registry: &mut ToolRegistry, root: PathBuf, session_id: &str) {
    let session_id = session_id.to_string();
    registry.register(ToolSpec::new_with_progress(
        "artifact",
        "Create or edit deliverable files in the WebUI Artifact workspace via patch. Paths are plain artifact file names. Use for reports, documents, and standalone files the user should inspect; publish an existing local file with present_artifact; read entries with read using artifact: paths.",
        patch_parameters(),
        move |args, progress| {
            let root = root.clone();
            let session_id = session_id.clone();
            async move {
                progress.report(format!(
                    "__tool_phase__~ {}",
                    t("prepare patch", "准备修改")
                ));
                tokio::task::yield_now().await;
                apply_artifact_patch(args, progress, &root, &session_id)
            }
        },
    ).presentation());
}

fn patch_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "patchText": {
                "type": "string",
                "description": "Full patch text wrapped in *** Begin Patch / *** End Patch."
            }
        },
        "required": ["patchText"],
        "additionalProperties": false
    })
}

fn apply_patch(args: Value, progress: ToolProgress) -> Result<String> {
    apply_patch_with(args, progress, path_arg, false)
}

fn apply_artifact_patch(
    args: Value,
    progress: ToolProgress,
    root: &Path,
    session_id: &str,
) -> Result<String> {
    let session_dir = ensure_artifact_session_dir(root, session_id)?;
    apply_patch_with(
        args,
        progress,
        |value| {
            // artifact: 前缀可写可不写:工具名已选域,前缀只是容错。
            let name = value.strip_prefix("artifact:").unwrap_or(value);
            artifact_patch_path(&session_dir, name)
        },
        true,
    )
}

mod hunks;
mod parse;
#[cfg(any(test, feature = "testkit"))]
mod test_support;

use self::hunks::*;
use self::parse::*;

/// edit 只管文件系统;带前缀的补丁给出指路错误而不是静默走错域。
fn edit_filesystem(args: Value, progress: ToolProgress) -> Result<String> {
    let patch_text = args
        .get("patchText")
        .or_else(|| args.get("patch_text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match detect_patch_namespace(&patch_text)? {
        PatchNamespace::Filesystem => apply_patch(args, progress),
        PatchNamespace::Artifact => {
            bail!("artifact files are edited with the `artifact` tool; edit only touches the filesystem")
        }
        PatchNamespace::KnowledgeBase => {
            bail!("knowledge-base files are edited with the `kb` tool; edit only touches the filesystem")
        }
    }
}

/// kb: 命名空间:解析与预检复用补丁引擎(读的是知识库里的真实文件),写入
/// 全部路由回 KnowledgeBase——直接 fs 写会绕过元数据库与语义索引。
fn apply_kb_patch(
    args: Value,
    progress: ToolProgress,
    config: &miyu_base::config::AppConfig,
    paths: &miyu_base::paths::MiyuPaths,
) -> Result<String> {
    if !config.plugins.knowledge_base.enabled {
        bail!("knowledge base plugin is disabled");
    }
    let kb = crate::tools::knowledge_base::KnowledgeBase::new(config.clone(), paths.clone())?;
    kb.init()?;
    let patch_text = args
        .get("patchText")
        .or_else(|| args.get("patch_text"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("patchText is required"))?;
    let rel_by_path = std::cell::RefCell::new(HashMap::<PathBuf, String>::new());
    let operations = parse_patch_with(patch_text, &|value: &str| {
        let rel = value.strip_prefix("kb:").unwrap_or(value);
        let path = kb.safe_file_path(rel)?;
        rel_by_path
            .borrow_mut()
            .insert(path.clone(), rel.to_string());
        Ok(path)
    })?;
    if operations.is_empty() {
        bail!("patch rejected: empty patch")
    }
    let changes = preflight_operations(operations)?;
    let rel_by_path = rel_by_path.into_inner();
    let mut files = Vec::new();
    for change in changes {
        let rel = rel_by_path
            .get(&change.path)
            .cloned()
            .unwrap_or_else(|| change.path.display().to_string());
        match change.kind {
            ChangeKind::Delete => {
                kb.remove(&rel)?;
                report_delete_preview(&progress, &change.path, &change.before)?;
            }
            ChangeKind::Add | ChangeKind::Update => {
                // 旧 upload 工具的内容守卫(技能/人格/记忆类内容不进知识库)
                // 原样保留,别因换了入口就放开。
                crate::tools::knowledge_base::reject_non_kb_upload(&change.after, "", &rel)?;
                let temp = tempfile::NamedTempFile::new()?;
                std::fs::write(temp.path(), change.after.as_bytes())?;
                kb.import_file(temp.path(), &rel)?;
                let diff = crate::tools::patch_preview::patch_result_json(
                    &change.path,
                    &change.before,
                    &change.after,
                );
                let payload = serde_json::to_string(&json!({
                    "path": format!("kb:{rel}"),
                    "diff": diff,
                }))?;
                progress.report(format!("__patch_preview__{payload}"));
            }
        }
        files.push(json!({
            "path": format!("kb:{rel}"),
            "operation": change.kind.as_str(),
        }));
    }
    kb.spawn_embedding_reindex()?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "operation": "kb",
        "files_changed": files.len(),
        "files": files,
    }))?)
}

fn apply_patch_with<F>(
    args: Value,
    progress: ToolProgress,
    resolve_path: F,
    managed_artifact: bool,
) -> Result<String>
where
    F: Fn(&str) -> Result<PathBuf>,
{
    let patch_text = args
        .get("patchText")
        .or_else(|| args.get("patch_text"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("patchText is required"))?;
    let operations = parse_patch_with(patch_text, &resolve_path)?;
    if operations.is_empty() {
        bail!("patch rejected: empty patch")
    }
    let changes = preflight_operations(operations)?;
    if managed_artifact {
        for change in &changes {
            if change.after.len() > super::artifact::MAX_ARTIFACT_BYTES {
                bail!(
                    "Artifact exceeds the {} byte limit: {}",
                    super::artifact::MAX_ARTIFACT_BYTES,
                    change.path.display()
                )
            }
        }
    }

    for change in &changes {
        progress.report(format!(
            "__tool_phase__~ {} {}",
            t("prepare patch", "准备修改"),
            display_path_for_progress(&change.path)
        ));
    }

    let mut files = Vec::new();
    let mut written: Vec<String> = Vec::new();
    for change in changes {
        let applied = (|| -> Result<()> {
            match change.kind {
                ChangeKind::Delete => {
                    if managed_artifact {
                        std::fs::remove_file(&change.path)?;
                    } else {
                        // 文件系统里的删除进回收站（09-24 B11）：rm 拦截和 trash_path
                        // 的前提都是「删除必须能挽回」，补丁里一个 Delete File 却是永久删除。
                        move_to_trash(&change.path)?;
                    }
                    report_delete_preview(&progress, &change.path, &change.before)?;
                }
                ChangeKind::Add | ChangeKind::Update => {
                    write_with_patch_preview(
                        &change.path,
                        &change.before,
                        &change.after,
                        &progress,
                        Map::new(),
                    )?;
                    if managed_artifact {
                        std::fs::set_permissions(
                            &change.path,
                            std::fs::Permissions::from_mode(0o600),
                        )?;
                        progress.report_artifact(change.path.clone(), String::new());
                    }
                }
            }
            Ok(())
        })();
        // 多文件补丁写到一半失败时，说清哪些已经写进去了（09-24 B10）：不说的话，
        // 模型会把整份补丁重打一遍，已经改过的那几处就对不上了。
        if let Err(error) = applied {
            if written.is_empty() {
                return Err(error);
            }
            bail!(
                "{error:#}. Already applied before this failure: {}",
                written.join(", ")
            );
        }
        written.push(display_path_for_progress(&change.path));
        let reported_path = if managed_artifact {
            change
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string()
        } else {
            display_path_for_progress(&change.path)
        };
        files.push(json!({
            "path": reported_path,
            "operation": change.kind.as_str(),
        }));
    }

    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "operation": if managed_artifact { "apply_artifact_patch" } else { "apply_patch" },
        "files_changed": files.len(),
        "files": files,
    }))?)
}

/// 挪进系统回收站。测试构建里不碰真回收站：记下路径、直接删掉（`test_support`）。
fn move_to_trash(path: &Path) -> Result<()> {
    #[cfg(any(test, feature = "testkit"))]
    return test_support::fake_trash(path);
    #[cfg(not(any(test, feature = "testkit")))]
    trash::delete(path)
        .map_err(|err| anyhow::anyhow!("failed to move {} to the Trash: {err}", path.display()))
}

fn report_delete_preview(progress: &ToolProgress, path: &Path, before: &str) -> Result<()> {
    let diff = crate::tools::patch_preview::patch_result_json(path, before, "");
    let payload = serde_json::to_string(&json!({
        "path": display_path_for_progress(path),
        "diff": diff,
    }))?;
    progress.report(format!("__patch_preview__{payload}"));
    Ok(())
}

#[derive(Debug, Clone)]
enum Operation {
    Add {
        path: PathBuf,
        lines: Vec<String>,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        move_to: Option<PathBuf>,
        hunks: Vec<Hunk>,
    },
}

#[derive(Debug, Clone)]
struct Hunk {
    context: Option<String>,
    end_of_file: bool,
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum HunkLine {
    Context(String),
    Delete(String),
    Insert(String),
}

#[derive(Debug, Clone, Copy)]
enum ChangeKind {
    Add,
    Update,
    Delete,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

struct FileChange {
    path: PathBuf,
    before: String,
    after: String,
    kind: ChangeKind,
}

fn path_arg(value: &str) -> Result<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        bail!("path is required")
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return Ok(home.join(rest));
        }
    }
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        miyu_base::workspace::effective_workdir().join(path)
    };
    miyu_base::sandbox::guard_write(&path)?;
    Ok(path)
}

fn ensure_artifact_session_dir(root: &Path, session_id: &str) -> Result<PathBuf> {
    validate_single_component(session_id, "session id")?;
    ensure_private_directory(root)?;
    let session_dir = root.join(session_id);
    ensure_private_directory(&session_dir)?;
    Ok(session_dir)
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "Artifact workspace path is not a directory: {}",
                path.display()
            );
        }
    } else {
        std::fs::create_dir_all(path)?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn artifact_patch_path(session_dir: &Path, value: &str) -> Result<PathBuf> {
    let value = value.trim();
    validate_single_component(value, "Artifact file name")?;
    if value.chars().count() > 180 || value.chars().any(char::is_control) {
        bail!("Artifact file name is invalid")
    }
    let path = session_dir.join(value);
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("Artifact patch target is not a regular file: {value}")
        }
    }
    Ok(path)
}

fn validate_single_component(value: &str, label: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!("{label} must be a single safe path component")
    }
    Ok(())
}

fn display_path_for_progress(path: &Path) -> String {
    crate::tools::patch_preview::display_path(path)
}

#[cfg(test)]
mod tests;
