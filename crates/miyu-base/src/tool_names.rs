//! 工具名的纯判定。
//!
//! 「哪些名字是命令类工具」「带后缀的事件名归到哪个工具 id」都只认字符串,
//! 不碰注册表、不碰配置。渲染层要它做命令块判定,中转线桥要它判 tool 输出形态,
//! 工具层自己也要它——住在工具层就逼着中转线反过来 use 工具层。放在基础层(09-16)。
//!
//! `tools` 那两处留了 `pub(crate) use crate::tool_names::…` 的再导出,
//! `miyu_engine::tools::is_command_tool` / `tool_event_base_name` 老路径一字未改。

/// 命令类工具:输出按命令块渲染,中转线也按它判 tool 输出形态。工具名是 core
/// 的事实,渲染层与中转线都只引用这里(09-16 起 agent 不再反向 use render)。
pub fn is_command_tool(name: &str) -> bool {
    COMMAND_TOOLS.contains(&name)
}

/// 命令类工具的名字(`web/app.js` 里抄了一份,单测盯着两边一致)。
pub const COMMAND_TOOLS: &[&str] = &["run_command", "Bash"];

/// 事件名 → 工具底名:`divine:塔罗` 这类带后缀的事件名归到注册的工具 id 上
/// (09-16 从 render 搬来:这是工具元数据,渲染层只是消费者)。
pub fn tool_event_base_name(name: &str) -> &str {
    if name.starts_with("divine:") {
        "divine"
    } else if name.starts_with("use_meme:") {
        "use_meme"
    } else if name.starts_with("load_skill:") {
        "load_skill"
    } else if name.starts_with("load_tools:") {
        "load_tools"
    } else if name.starts_with("subagent:") {
        "subagent"
    // 改名前的事件名(task:<描述>)还留在历史记录里。
    } else if name.starts_with("task:") {
        "task"
    } else {
        name
    }
}

/// 改磁盘文件的工具:Miyu 自己的 `edit`(旧名 `apply_patch` 等还留在历史里),加上
/// 中转线那头的——claude 的 `Edit`/`Write`/`MultiEdit`/`NotebookEdit`,agy 的
/// `write_to_file`/`replace_file_content`/`multi_replace_file_content`,codex 的
/// `file_change` 在桥上已折成 `edit`。知识库、artifact 的补丁编辑与删文件不算:
/// 摘要里的 edits 只数改了磁盘文件的那几步(用户 09-24 拍板)。
pub const FILE_EDIT_TOOLS: &[&str] = &[
    "edit",
    "apply_patch",
    "write_file",
    "edit_file",
    "edit_string",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "write_to_file",
    "replace_file_content",
    "multi_replace_file_content",
];

/// 折叠摘要行里一步工具归哪一类(09-24:`Ran 3 commands · 2 edits · 4 tools`)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Command,
    FileEdit,
    Other,
}

pub fn tool_kind(name: &str) -> ToolKind {
    let name = tool_event_base_name(name);
    if is_command_tool(name) {
        ToolKind::Command
    } else if FILE_EDIT_TOOLS.contains(&name) {
        ToolKind::FileEdit
    } else {
        ToolKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 浏览器调不了 Rust,`web/app.js` 里抄了一份名单。两边一走样,网页收缩行就和
    /// 终端数得不一样(同一轮一边说 `Ran 2 commands`、一边说 `2 tools`)。
    #[test]
    fn web_fold_summary_lists_match_rust() {
        let app =
            std::fs::read_to_string(std::path::Path::new(crate::WORKSPACE_ROOT).join("web/app.js"))
                .expect("读 web/app.js");
        let list = |name: &str| -> Vec<String> {
            let head = format!("const {name} = [");
            let start = app
                .find(&head)
                .unwrap_or_else(|| panic!("app.js 里没有 {name}"));
            let body = &app[start + head.len()..];
            let body = &body[..body.find("];").expect("名单没收尾")];
            body.split(',')
                .map(|item| item.trim().trim_matches('"').to_string())
                .filter(|item| !item.is_empty())
                .collect()
        };
        assert_eq!(list("COMMAND_TOOLS"), COMMAND_TOOLS);
        assert_eq!(list("FILE_EDIT_TOOLS"), FILE_EDIT_TOOLS);
    }

    #[test]
    fn kinds_follow_the_base_name() {
        assert_eq!(tool_kind("run_command"), ToolKind::Command);
        assert_eq!(tool_kind("Bash"), ToolKind::Command);
        assert_eq!(tool_kind("edit"), ToolKind::FileEdit);
        assert_eq!(tool_kind("replace_file_content"), ToolKind::FileEdit);
        // 知识库、artifact 的补丁编辑与删文件不算改文件(用户 09-24 拍板)。
        for other in ["kb", "artifact", "trash_path", "read", "subagent:查资料"] {
            assert_eq!(tool_kind(other), ToolKind::Other, "{other}");
        }
    }
}
