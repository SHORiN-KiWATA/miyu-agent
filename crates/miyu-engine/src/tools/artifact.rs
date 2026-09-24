use super::{ToolProgress, ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use miyu_base::paths::MiyuPaths;
use serde_json::{json, Value};
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::path::{Component, Path};

pub(super) const MAX_ARTIFACT_BYTES: usize = 20 * 1024 * 1024;

/// 网页会话系统侧的 artifact 用法(每请求组装、不化石,AGENTS §1.4)。点名的工具必须在网页
/// 会话的工具面上真实存在:08-22 工具面统一删掉 create_artifact / read_artifact 之后,这里照旧
/// 让模型调它们,一个月没人发现(守卫:`webui_artifact_prompts_name_only_registered_tools`)。
pub const WEBUI_ARTIFACT_POLICY: &str = "<artifact-policy>\n\
You are working in the Miyu WebUI and have artifact presentation tools.\n\
- When the user explicitly asks for a report, document, web page, table, data file, standalone code file, or another downloadable deliverable, you must create or present an artifact.\n\
- For text deliverables you write yourself, prefer the artifact tool. The file name must carry the correct extension.\n\
- For files already produced by commands or other tools, call present_artifact.\n\
- The latest <artifact-workspace> block lists this session's artifact files. Do not glob the managed directory or guess ~/.miyu paths.\n\
- To update an existing artifact, read it first with read and the path artifact:<name>. Then make targeted edits with the artifact tool and the bare file name. Do not rewrite the whole file unless the user explicitly asks for a full rewrite.\n\
- Publish only after the content is complete and self-checked. Do not publish ordinary project source edits, config changes, test fixtures, or short answers as artifacts.\n\
- The artifact is part of the answer. After publishing succeeds, tell the user briefly in text.\n\
</artifact-policy>";

/// 网页会话回合尾巴里 artifact 清单块的开头标签。它同时是回合尾巴去重的钥匙
/// (`agent::STATE_SNAPSHOT_TAGS`):清单与对话里最近一份相同就不再重发。所以块只在
/// 这里拼,宿主只管调用——标签两处各写一份,改了一处去重就悄悄失效。
pub const ARTIFACT_WORKSPACE_TAG: &str = "<artifact-workspace>";

/// 网页会话每轮附在回合尾巴里的 artifact 清单块。清单随 artifact 增删而变,所以走尾巴。
/// 块里只陈述清单这个事实:尾巴会化石回放,用法写在 system 侧的 `WEBUI_ARTIFACT_POLICY`。
pub fn webui_artifact_workspace_block(manifest: &str) -> String {
    format!("{ARTIFACT_WORKSPACE_TAG}\n{manifest}\n</artifact-workspace>")
}

/// artifact 库根:成员回合落在自己家里(`home/<user>/artifacts`),与 KB 的
/// `kb_root_for` 同一套口径——`MiyuPaths::artifacts_dir()` 是 admin_owned,永远指
/// 管理员的家,成员用它就会去读写管理员的 artifacts(09-11 实测:成员读
/// `artifact:x.svg` 报「outside your workspace」,因为解析到了 home/shorin)。
/// 管理员 / 无成员身份时原样走默认。
pub fn artifacts_root(config: &miyu_base::config::AppConfig, paths: &MiyuPaths) -> PathBuf {
    match config.member_home_dir() {
        Some(home) => home.join("artifacts"),
        None => paths.artifacts_dir(),
    }
}

// 08-21 二次裁定:Artifact 写入独立成 `artifact` 补丁工具(域名即广告),
// 读取走 read 的 artifact: 前缀;发布仍是 present_artifact。
pub fn register_webui(registry: &mut ToolRegistry, artifacts_root: PathBuf, session_id: &str) {
    register_present(registry);
    super::apply_patch::register_artifact(registry, artifacts_root, session_id);
}

pub fn managed_manifest(root: &Path, session_id: &str) -> Result<String> {
    validate_session_id(session_id)?;
    let session_dir = root.join(session_id);
    let mut entries = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&session_dir) else {
        return Ok("(no managed artifacts yet)".to_string());
    };
    for entry in read_dir.flatten() {
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.chars().any(char::is_control) {
            continue;
        }
        entries.push((name, metadata.len()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    if entries.is_empty() {
        return Ok("(no managed artifacts yet)".to_string());
    }
    let mut output = String::from("Managed artifact files in this session:");
    for (name, size) in entries.into_iter().take(100) {
        output.push_str(&format!("\n- {name} ({size} bytes)"));
    }
    Ok(output)
}

fn register_present(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new_with_progress(
        "present_artifact",
        "Publish a completed file to the WebUI preview workspace. Use this only for files the user should inspect as a deliverable, not routine source edits.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path to publish."
                },
                "title": {
                    "type": "string",
                    "description": "Optional display title."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        |args, progress| async move { present_artifact(args, progress) },
    ).presentation());
}

fn present_artifact(args: Value, progress: ToolProgress) -> Result<String> {
    let raw_path = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if raw_path.is_empty() {
        bail!("path is required");
    }
    let path = expand_path(raw_path);
    miyu_base::sandbox::guard_read(&path)?;
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        bail!("artifact path is not a file: {}", path.display());
    }
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    progress.report_artifact(path.clone(), title.clone());
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact")
        .to_string();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "path": path,
        "filename": filename,
        "title": title,
        "published": true
    }))?)
}

// Edit/Read 统一后生产路径走 edit 的 artifact: 命名空间;本函数仅测试
// 保留(路径逃逸/权限语义的回归靠它,底层 managed_file_path 与生产共享)。

pub(in crate::tools) fn managed_file_path(
    root: &Path,
    session_id: &str,
    filename: &str,
) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    let session_dir = root.join(session_id);
    let session_canonical = session_dir.canonicalize().with_context(|| {
        format!(
            "Artifact workspace does not exist: {}",
            session_dir.display()
        )
    })?;
    let path = session_dir.join(filename);
    let metadata = std::fs::symlink_metadata(&path)
        .with_context(|| format!("Artifact does not exist: {filename}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Artifact is not a regular file: {filename}");
    }
    let canonical = path.canonicalize()?;
    if canonical.parent() != Some(session_canonical.as_path()) {
        bail!("Artifact path escaped its managed workspace");
    }
    Ok(canonical)
}

fn validate_session_id(session_id: &str) -> Result<()> {
    let mut components = Path::new(session_id).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!("invalid session id for Artifact workspace");
    }
    Ok(())
}

fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    let path = std::path::Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        miyu_base::workspace::effective_workdir().join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 网页会话的 artifact 提示词只点名网页会话工具面上真有的工具。snake_case 的词都当
    /// 工具名查;`artifact`、`read` 这类单词名另外点名核对。
    #[test]
    fn webui_artifact_prompts_name_only_registered_tools() {
        let temp = tempfile::tempdir().unwrap();
        let paths = super::super::tests::test_paths(temp.path());
        let config = miyu_base::config::AppConfig::default();
        let mut registry = super::super::build_tool_registry(
            &config,
            &paths,
            miyu_base::config::PersonaLane::Active,
            false,
        )
        .unwrap();
        super::super::register_webui_artifact_tools(&mut registry, &config, &paths, "sess_test");
        let prompts = format!(
            "{WEBUI_ARTIFACT_POLICY}\n{}",
            webui_artifact_workspace_block("(no managed artifacts yet)")
        );
        let named = prompts
            .split(|c: char| !(c.is_ascii_lowercase() || c == '_'))
            .filter(|word| word.contains('_') && !word.starts_with('_') && !word.ends_with('_'))
            .chain(["artifact", "read"]);
        for name in named {
            assert!(
                registry.contains(name),
                "prompt names a missing tool: {name}"
            );
        }
    }

    #[test]
    fn managed_artifact_is_private_and_published() {
        let temp = tempfile::tempdir().unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let output = create_artifact(
            json!({"filename":"report.md", "content":"# Report\n", "title":"Report"}),
            ToolProgress::new(sender),
            temp.path(),
            "sess_test",
        )
        .unwrap();
        let payload: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(payload["published"], true);
        let path = temp.path().join("sess_test/report.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Report\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(matches!(
            receiver.try_recv().unwrap(),
            super::super::ToolProgressEvent::Artifact { path: event_path, .. } if event_path == path
        ));
    }

    #[test]
    fn managed_artifact_rejects_path_escape() {
        let temp = tempfile::tempdir().unwrap();
        for filename in ["../report.md", "/tmp/report.md", "nested/report.md", ""] {
            assert!(create_artifact(
                json!({"filename":filename, "content":"content"}),
                ToolProgress::default(),
                temp.path(),
                "sess_test",
            )
            .is_err());
        }
    }

    #[test]
    fn managed_artifact_can_be_read() {
        let temp = tempfile::tempdir().unwrap();
        create_artifact(
            json!({
                "filename":"deployment-plan.md",
                "content":"# Plan\n\n## Rollback\nRestore the previous release.\n"
            }),
            ToolProgress::default(),
            temp.path(),
            "sess_test",
        )
        .unwrap();

        let read = read_artifact(
            json!({"filename":"deployment-plan.md"}),
            temp.path(),
            "sess_test",
        )
        .unwrap();
        assert!(read.contains("3: ## Rollback"));
    }

    #[test]
    fn artifact_manifest_lists_names_without_file_contents() {
        let temp = tempfile::tempdir().unwrap();
        let paths = MiyuPaths {
            root_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            config_file: temp.path().join("config/config.jsonc"),
            skills_dir: temp.path().join("config/skills"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
            state_dir: temp.path().join("state"),
            pictures_dir: temp.path().join("pictures"),
            fish_hook_file: temp.path().join("fish"),
            bash_hook_file: temp.path().join("bash"),
            zsh_hook_file: temp.path().join("zsh"),
            scripts_dir: temp.path().join("scripts"),
            system_scripts_dir: temp.path().join("system-scripts"),
        };
        create_artifact(
            json!({"filename":"secret-report.md", "content":"private body"}),
            ToolProgress::default(),
            &paths.artifacts_dir(),
            "sess_test",
        )
        .unwrap();
        let manifest = managed_manifest(&paths.artifacts_dir(), "sess_test").unwrap();
        assert!(manifest.contains("secret-report.md"));
        assert!(!manifest.contains("private body"));
    }

    #[test]
    fn artifacts_root_prefers_member_home_over_admin() {
        let temp = tempfile::tempdir().unwrap();
        let paths = MiyuPaths {
            root_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            config_file: temp.path().join("config/config.jsonc"),
            skills_dir: temp.path().join("config/skills"),
            data_dir: temp.path().join("data"),
            cache_dir: temp.path().join("cache"),
            state_dir: temp.path().join("state"),
            pictures_dir: temp.path().join("pictures"),
            fish_hook_file: temp.path().join("fish"),
            bash_hook_file: temp.path().join("bash"),
            zsh_hook_file: temp.path().join("zsh"),
            scripts_dir: temp.path().join("scripts"),
            system_scripts_dir: temp.path().join("system-scripts"),
        };
        // 无成员身份:走默认(admin_owned 回退到 data/artifacts)。
        let mut config = miyu_base::config::AppConfig::default();
        assert_eq!(artifacts_root(&config, &paths), paths.artifacts_dir());
        // 成员:落到自己家里的 artifacts,不再借用管理员目录(09-11 修复)。
        config.accounts.home_dir = Some("/tmp/miyu-member-xyz".to_string());
        assert_eq!(
            artifacts_root(&config, &paths),
            std::path::PathBuf::from("/tmp/miyu-member-xyz/artifacts")
        );
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
