//! 请求形状量尺(core/normal 重构的字节回归):把 Agent 装出来的 system 提示词、
//! 消息序列、工具名单原样落成 JSON,重构前后各跑一次做 diff。
//!
//! 带主机环境块(主机名、cwd 等),机器相关,所以不进仓库夹具;输出路径由
//! `MIYU_REQUEST_SHAPE_OUT` 给,运行时戳(`<runtime now=…>`)归一成常量。
//! 覆盖的是稳定前缀 + 历史 + 当前输入 + 这一轮真发出去的工具定义(描述与 schema
//! 全文),另有两张沙盒脸钉住 `<sandbox>` 尾巴(开着第一次说、说过之后关了补一句)。
//! 回合尾注入里要真模型才跑的(联想记忆、人格提醒、表情包提醒)不在这里。
//!
//! ```text
//! MIYU_REQUEST_SHAPE_OUT=/tmp/before.json cargo test --lib request_shape_probe -- --ignored
//! ```

use super::shared::*;
use crate::agent::*;
use miyu_base::config::{AppConfig, PromptAudience};

fn normalize_runtime_stamp(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(start) = text.find("<runtime now=\"") {
                let after = start + "<runtime now=\"".len();
                if let Some(end) = text[after..].find('"') {
                    text.replace_range(after..after + end, "<NOW>");
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize_runtime_stamp),
        serde_json::Value::Object(map) => map.values_mut().for_each(normalize_runtime_stamp),
        _ => {}
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SandboxFace {
    None,
    /// 这一轮开着沙盒,历史里没说过。
    On,
    /// 上一轮说过沙盒,这一轮关了。
    OffAfterOn,
}

#[test]
#[ignore]
fn request_shape_probe() {
    let Ok(out) = std::env::var("MIYU_REQUEST_SHAPE_OUT") else {
        eprintln!("MIYU_REQUEST_SHAPE_OUT 未设置,量尺不落盘");
        return;
    };
    // 第四列:机器语音开关(唤醒对话)。语音协议段自 09-16 起跟语音子系统快照走,
    // 开着的那张脸就是 09-16 之前所有 owner 脸的字节。第五列:沙盒(09-25 起)。
    let faces = [
        (
            "normal-owner",
            PersonaLane::Active,
            PromptAudience::Owner,
            false,
            SandboxFace::None,
        ),
        (
            "normal-owner-voice",
            PersonaLane::Active,
            PromptAudience::Owner,
            true,
            SandboxFace::None,
        ),
        (
            "normal-external",
            PersonaLane::Active,
            PromptAudience::External,
            false,
            SandboxFace::None,
        ),
        (
            "normal-internal",
            PersonaLane::Active,
            PromptAudience::Internal,
            false,
            SandboxFace::None,
        ),
        (
            "dev-owner",
            PersonaLane::Dev,
            PromptAudience::Owner,
            false,
            SandboxFace::None,
        ),
        (
            "normal-owner-sandbox",
            PersonaLane::Active,
            PromptAudience::Owner,
            false,
            SandboxFace::On,
        ),
        (
            "normal-owner-sandbox-off",
            PersonaLane::Active,
            PromptAudience::Owner,
            false,
            SandboxFace::OffAfterOn,
        ),
    ];
    let mut report = serde_json::Map::new();
    for (label, mode, audience, voice, sandbox) in faces {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let mut config = AppConfig::default();
        config.voice.enabled = voice;
        // 一件全局技能:技能目录非空,才看得出它落在哪(09-25 起在回合尾巴里)。
        let skill = paths.skills_dir.join("probe-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: probe-skill\ndescription: Probe skill for the request shape.\n---\n\nBody.\n",
        )
        .unwrap();
        let state = StateStore::new(&paths).unwrap();
        state.init_files().unwrap();
        state.start_turn("turn_1", "第一问", 1).unwrap();
        state.complete_turn("turn_1", "第一答", None).unwrap();
        state.start_turn("turn_2", "第二问", 2).unwrap();
        state.complete_turn("turn_2", "第二答", None).unwrap();
        let policy = std::sync::Arc::new(miyu_base::sandbox::SandboxPolicy {
            root: temp.path().join("root"),
            writable_summary: vec!["root".into(), "/tmp".into()],
            readable_summary: vec!["root".into(), "/tmp".into(), "system dirs".into()],
            ..Default::default()
        });
        if sandbox == SandboxFace::OffAfterOn {
            // 上一轮开着沙盒、说过一次;这一轮关了。
            state
                .set_turn_context_messages(
                    "turn_2",
                    &[ChatMessage::turn_context(
                        miyu_base::host_info::sandbox_notice(&policy),
                    )],
                )
                .unwrap();
        }
        let client =
            OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, &paths).unwrap();
        let tools = crate::tools::build_tool_registry(&config, &paths, mode, true).unwrap();
        let mut agent =
            Agent::new_for_audience(config, &paths, state, client, tools, mode, audience).unwrap();
        agent.prepare_for_turn().unwrap();
        let scope = (sandbox == SandboxFace::On).then(|| policy.clone());
        let (messages, user_index) = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(miyu_base::sandbox::with_sandbox(scope, async {
                agent.chat_messages("current", "探针输入").unwrap()
            }));
        // 注册表的名单顺序不稳定(发给模型的定义按名排序),量尺也按名排。
        let mut tools = agent.tools.lock().unwrap().tool_names();
        tools.sort();
        let mut face = serde_json::json!({
            "system_prompt": agent.system_prompt,
            "messages": messages,
            "user_index": user_index,
            "tools": tools,
            "tool_definitions": agent.round_tool_definitions(false),
            "subsystems": agent.core.subsystems.ids(),
        });
        normalize_runtime_stamp(&mut face);
        // 临时家目录每次不同(主机环境块里的 miyu_home),归一成常量。
        let home = temp.path().display().to_string();
        let mut text = serde_json::to_string(&face).unwrap();
        text = text.replace(&home, "<HOME>");
        report.insert(label.to_string(), serde_json::from_str(&text).unwrap());
    }
    std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!("request shape written to {out}");
}
