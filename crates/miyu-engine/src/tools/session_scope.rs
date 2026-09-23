//! 按会话种类收工具面：回合装配与 MCP 桥共用的一道裁剪。
//!
//! 中转线（claude-code 等）忽略请求里的 tools，工具只从 MCP 桥拿，而桥另建一份
//! registry。这几道裁剪原来只写在回合装配里（`web/turns/task.rs`），桥上一道也没有：
//! 语音唤醒开着时每条会话都挂着 end_voice_chat，子代理在中转线上拿得到排除表里的
//! 工具和 ask_question，孙代理还能再开子代理（09-23）。和平台那道
//! `apply_platform_turn_scope` 是同一个教训：两边各写一遍就会漂。

use super::{register_ask_question, ToolRegistry, END_VOICE_CHAT_TOOL, SUBAGENT_SESSION_EXCLUDED};
use miyu_core::state::{SessionRecord, SUBAGENT_SESSION_KIND, VOICE_SESSION_KIND};

/// 孙代理（深度 2）再摘掉开子代理的这两件：树深写死到 2（09-18 会话化）。
const GRANDCHILD_EXCLUDED: [&str; 2] = ["subagent", "send_subagent_message"];

/// 把会话种类决定的增删落到 `registry` 上（发给模型的定义按名字排序，字节只看最后
/// 剩下哪几件，与增删先后无关）：
///
/// 1. end_voice_chat 只留给唤醒对话那条会话。别的会话里它是每回合都读一遍、却永远
///    不会调用的常驻工具，机器级开关（`config.voice.enabled`）管不到这一层。一条会话
///    的种类终身不变，所以这条分叉不会让 tools 数组在会话中途变字节。
/// 2. 子代理会话减排除表，孙代理再摘掉开子代理的那两件。
/// 3. ask_question 只给有人来答的会话：平台回合不给，子代理也不给（它的回话对象是
///    父回合）。
///
/// `session` 为 None（查不到会话记录）时按普通会话处理。
pub fn apply_session_kind_scope(
    registry: &mut ToolRegistry,
    session: Option<&SessionRecord>,
    platform: bool,
    tools_enabled: bool,
) {
    if session.is_none_or(|record| record.kind != VOICE_SESSION_KIND) {
        registry.unregister(END_VOICE_CHAT_TOOL);
    }
    let subagent = session.filter(|record| record.kind == SUBAGENT_SESSION_KIND);
    if let Some(record) = subagent {
        for name in SUBAGENT_SESSION_EXCLUDED {
            registry.unregister(name);
        }
        if record.depth >= 2 {
            for name in GRANDCHILD_EXCLUDED {
                registry.unregister(name);
            }
        }
    }
    if !platform && tools_enabled && subagent.is_none() {
        register_ask_question(registry);
    }
}

/// 把这一轮的单轮覆盖项（工具白名单、不写记忆）落到 `registry` 上。
///
/// 和上面那道一样，回合装配与 MCP 桥共用：桥原来一样都不认，中转线的模型照样拿到
/// 全部工具，`--no-memory` 的回合还摆着 remember_fact（09-23）。回合那边按
/// 「先摘 remember_fact、再按白名单留」写过一遍，这里是同一个集合。
pub fn apply_turn_restrictions(
    registry: &mut ToolRegistry,
    restrictions: &miyu_base::host_ports::TurnToolRestrictions,
) {
    if restrictions.is_empty() {
        return;
    }
    for name in registry.tool_names() {
        if !restrictions.allows(&name) {
            registry.unregister(&name);
        }
    }
}
