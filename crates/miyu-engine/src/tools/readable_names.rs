use miyu_base::i18n::{is_zh, text as t};

use super::{tool_event_base_name, SCRIPT_DISPLAY_NAMES};

pub fn readable_tool_name(name: &str) -> String {
    if let Some(display_name) = builtin_readable_tool_name(name) {
        return display_name.to_string();
    }
    // 脚本表里登记的、或客户端从 daemon 事件里学来的（`register_display_name`）：
    // **整名优先**。`load_tools:某脚本` 这种带前缀的事件名 daemon 那边已经拼好了
    // 「加载：显示名」，客户端照单全收——它自己拆前缀再去查脚本名的话，那一刻
    // 脚本名还没学到（脚本自己的 tool.started 在后面），显示成「加载：裸 id」。
    if let Some(display_name) = learned_display_name(name) {
        return display_name;
    }
    if let Some(skill) = name.strip_prefix("load_skill:") {
        return if is_zh() {
            format!("加载技能：{skill}")
        } else {
            format!("Load skill: {skill}")
        };
    }
    if let Some(tools) = name.strip_prefix("load_tools:") {
        let targets = tools
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        let display = targets
            .iter()
            .map(|name| readable_load_target_name(name))
            .collect::<Vec<_>>()
            .join(if is_zh() { "、" } else { ", " });
        return if is_zh() {
            format!("加载：{display}")
        } else {
            format!("Load: {display}")
        };
    }
    // `use_meme:search` / `subagent:xxx` 这类带 action 后缀的事件名，按基名取友好名。
    // 漏了这一步就一路落到最后的 `name.to_string()`，UI 上显示成裸的
    // `use_meme:search`——同一个工具有没有后缀，显示名不该差这么远。
    let base = tool_event_base_name(name);
    if base != name {
        if let Some(display_name) = builtin_readable_tool_name(base) {
            return display_name.to_string();
        }
        if let Some(display_name) = learned_display_name(base) {
            return display_name;
        }
    }
    name.to_string()
}

/// 脚本表 / 事件里学来的显示名。见 `tools::register_script_display_names` 与
/// `tools::register_display_name`。
fn learned_display_name(name: &str) -> Option<String> {
    let guard = SCRIPT_DISPLAY_NAMES.read().ok()?;
    guard.as_ref()?.get(name).cloned()
}

fn readable_load_target_name(name: &str) -> String {
    if let Some(group) = name.strip_prefix("group:") {
        return builtin_readable_group_name(group)
            .map(str::to_string)
            .unwrap_or_else(|| format!("group:{group}"));
    }
    readable_tool_name(name)
}

/// Phase text for the "still receiving arguments" hint, or `None` for tools
/// that stream too fast to be worth one.
///
/// The tool name is decoded from the stream well before its arguments finish,
/// so this is what keeps a multi-kilobyte patch or file write from looking
/// frozen. Deliberately a short list: flashing a hint for a `read_file` whose
/// arguments arrive in one chunk is noise.
pub fn preparing_phase(name: &str) -> Option<&'static str> {
    Some(match name {
        "edit"
        | "artifact"
        | "kb"
        | "apply_patch"
        | "apply_artifact_patch"
        | "create_artifact"
        | "write_file"
        | "edit_file"
        | "edit_string" => t("Preparing edit", "准备编辑"),
        "run_command" => t("Preparing command", "准备执行"),
        // claude 原生工具(claude-code 中转,原名不剥):同一张表,否则中转
        // 线的 RemoteToolPreparing 只剩批量兜底。
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => t("Preparing edit", "准备编辑"),
        "Bash" => t("Preparing command", "准备执行"),
        "Task" | "Agent" => t("Preparing task", "准备任务"),
        "TodoWrite" => t("Preparing list", "准备清单"),
        "AskUserQuestion" => t("Preparing question", "准备问题"),
        // 批量删的参数是一整串路径,条数一多就是几百字节,正好落在
        // 「工具名已解码、参数还在流」的那个窗口里。
        "trash_path" => t("Preparing delete", "准备删除"),
        // A subagent brief is long, and its own timed block only appears once
        // the arguments have all arrived.
        "subagent" => t("Preparing task", "准备任务"),
        "ask_question" => t("Preparing question", "准备问题"),
        // 整张清单都在参数里,条目一多就是几百字节,和批量删是同一个窗口。
        "todowrite" => t("Preparing list", "准备清单"),
        // 给别的会话递的话在参数里,长消息也是几百字节往上(09-23)。
        "send_to_other_running_session" => t("Preparing message", "准备消息"),
        _ => return None,
    })
}

/// 同一条消息里第二个及以后的工具调用用的提示。
///
/// 单看每个工具都不够"慢"到值得提示，但 N 个调用的参数是接连流完的，
/// 合起来的静默窗口和一次大 patch 一样长。此时具体是哪个工具已经不重要
/// 了——重要的是让用户知道后面还有。
pub fn batch_preparing_phase() -> &'static str {
    t("Preparing tools", "准备工具")
}

/// 内建工具的双语显示名。`descriptions/*.json` 里的 `display_name` 是中文单槽,
/// 英文界面拿它就会端出中文名(09-14 A/B:英文目录里 36 个工具名是中文),所以
/// `ToolSpec` 也从这张表取名 —— 渲染层与注册表从此说同一个名字。
pub(crate) fn builtin_readable_tool_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "run_command" => t("Run command", "运行命令"),
        "job" => t("Background jobs", "后台任务"),
        "edit" | "apply_patch" => t("Edit files", "编辑文件"),
        "kb" => t("Edit knowledge base", "编辑知识库"),
        "artifact" | "apply_artifact_patch" => t("Edit preview file", "修改预览文件"),
        "create_artifact" => t("Create preview file", "创建预览文件"),
        "read_artifact" => t("Read preview file", "读取预览文件"),
        "present_artifact" => t("Preview file", "预览文件"),
        "ask_question" => t("Ask user", "询问用户"),
        // "task" 是 09-11 改名前的旧名:历史记录里存着的调用照样要显示成
        // 「子代理」,不然翻旧会话看到的是裸工具名。
        "subagent" | "task" => t("Subagent", "子代理"),
        "send_subagent_message" => t("Message subagent", "给子代理留言"),
        "send_to_other_running_session" => t("Message another session", "给其他会话发送消息"),
        // 显示名的真相源是这张表，不是 `ToolSpec::with_display_name`——那一份
        // 只进工具目录，时间线画的是这里（09-22：这两件工具界面上是裸 id）。
        // `query_token_usage` 是 09-22 拆成两件之前的旧名，同 "task" 的理由
        // 留着：历史记录里存着的调用照样要显示成中文。
        "query_system_token_usage" | "query_token_usage" => t("Token usage", "词元用量"),
        "query_session_token_usage" => t("Session token usage", "本会话用量"),
        "read" | "read_file" => t("Read file", "读取文件"),
        "write_file" => t("Write file", "写入文件"),
        "edit_file" => t("Edit file", "编辑文件"),
        "edit_string" => t("Edit string", "字符串编辑"),
        "list_directory" => t("List directory", "列目录"),
        "create_directory" => t("Create directory", "创建目录"),
        "trash_path" => t("Move to trash", "移入回收站"),
        "glob" => t("Find files", "查找文件"),
        "grep" => t("Search text", "搜索文本"),
        "get_current_directory" => t("Current directory", "当前目录"),
        "get_current_time" => t("Current time", "当前时间"),
        "check_os_info" => t("System information", "查看系统信息"),
        "web_search" => t("Web search", "网络搜索"),
        "web_fetch" => t("Fetch webpage", "读取网页"),
        "search_web_images" => t("Search images", "搜索图片"),
        "share_file" => t("Share file", "分享文件"),
        "analyze_image" | "vision_analyze" => t("Visual analysis", "视觉分析"),
        "print_image" => t("Display image", "显示图片"),
        "generate_image" => t("Generate image", "生成图片"),
        "use_meme" => t("Meme", "表情包"),
        "manage_meme" => t("Manage memes", "管理表情包"),
        "end_voice_chat" => t("End voice chat", "结束语音对话"),
        "speak" => t("Speak", "说话"),
        "send_qq_message" => t("Send to QQ", "发送到 QQ"),
        "qq_contacts" => t("QQ contacts", "查 QQ 联系人"),
        "send_voice_message" => t("Send voice message", "发送语音"),
        "sponsor" => t("Sponsorships", "赞助记账"),
        "upload_knowledge_base_file" | "upload_text_to_knowledge_base" => {
            t("Import knowledge base", "导入知识库")
        }
        "read_knowledge_base_file" => t("Read knowledge base", "读取知识库"),
        "search_knowledge_base" => t("Search knowledge base", "搜索知识库"),
        "edit_knowledge_base_file" => t("Edit knowledge base", "编辑知识库"),
        "remove_knowledge_base_file" => t("Remove from knowledge base", "移除知识库"),
        "list_knowledge_base_files" => t("List knowledge base", "列出知识库"),
        "alarm" => t("Alarms", "闹钟"),
        "remember_fact" => t("Remember fact", "记录记忆"),
        "search_evicted_context" => t("Search old context", "搜索旧上下文"),
        "recall_memory" | "recall_memories" => t("Recall memories", "召回记忆"),
        "forget_memory" | "forget_memories" => t("Forget memories", "删除记忆"),
        "list_memory" | "list_memories" => t("List memories", "列出记忆"),
        "aur" => t("AUR query", "AUR 查询"),
        "archlinux_official_package_query" => t("Query Arch package", "查询 Arch 官方包"),
        "archwiki_query" => t("Query ArchWiki", "查询 ArchWiki"),
        "archlinux_news" => t("Arch news", "Arch 新闻"),
        "exchange_rate" | "get_exchange_rate" => t("Exchange rates", "汇率查询"),
        "load_skill" => t("Load skill", "加载技能"),
        "manage_skill" => t("Manage skills", "管理技能"),
        "load_tools" => t("Load", "加载"),
        "ledger" => t("Ledger", "记账"),
        "manage_ledger" => t("Manage ledger", "账本管理"),
        "manage_script" => t("Manage scripts", "管理脚本"),
        "todowrite" => t("Todo list", "任务列表"),
        "goal" => t("Long-task goal", "长任务目标"),
        "review_aur_package" => t("Review AUR package", "审查 AUR 包"),
        "install_aur_package" => t("Install AUR package", "安装 AUR 包"),
        _ => return None,
    })
}

pub(super) fn builtin_readable_group_name(group: &str) -> Option<&'static str> {
    Some(match group {
        "acg" => t("ACG tools", "ACG 工具组"),
        "agent" => t("Subagent tools", "子代理工具组"),
        "alarms" => t("Alarm tools", "闹钟工具组"),
        "arch" => t("Arch / AUR tools", "Arch / AUR 工具组"),
        "dev" => t("Development tools", "开发修改工具组"),
        "dev-read" => t("Code search tools", "代码检索工具组"),
        "diagnostics" => t("Diagnostic tools", "诊断工具组"),
        "divination" => t("Divination tools", "玄学工具组"),
        "gaming" => t("Gaming tools", "游戏工具组"),
        "images" => t("Image tools", "图片工具组"),
        "knowledge" => t("Knowledge base tools", "知识库工具组"),
        "ledger" => t("Ledger tools", "记账工具组"),
        "knowledge-admin" => t("Knowledge base management", "知识库管理工具组"),
        "linux-docs" => t("Linux documentation", "Linux 文档工具组"),
        "memory" => t("Memory tools", "记忆工具组"),
        "memes" => t("Meme tools", "表情包工具组"),
        "planning" => t("Planning tools", "任务规划工具组"),
        "research" => t("Research tools", "研究工具组"),
        "scripts" => t("Script tools", "脚本工具组"),
        "scripting" => t("Script management", "脚本管理工具组"),
        "shell" => t("Shell tools", "Shell 工具组"),
        "shopping" => t("Shopping tools", "购物工具组"),
        "skills" => t("Skill tools", "技能工具组"),
        "systeminfo" => t("System information", "系统信息工具组"),
        "utility" => t("Utility tools", "实用工具组"),
        "web" => t("Web tools", "联网工具组"),
        _ => return None,
    })
}

#[cfg(test)]
mod display_name_tests {
    use super::readable_tool_name;

    /// 时间线上那一步的抬头走的是这张表。09-22 拆成两件工具时两个名字都不在
    /// 表里，界面上就是裸 id——`ToolSpec::with_display_name` 救不了，那一份只
    /// 进工具目录。旧名也得在：翻老会话时那些调用记录照样要显示成中文。
    #[test]
    fn both_token_usage_tools_have_a_readable_name() {
        for name in [
            "query_system_token_usage",
            "query_session_token_usage",
            "query_token_usage",
        ] {
            let display = readable_tool_name(name);
            assert_ne!(display, name, "{name} 显示成了裸 id");
            assert!(
                display
                    .chars()
                    .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)),
                "{name} 的显示名不是中文: {display}"
            );
        }
    }
}
