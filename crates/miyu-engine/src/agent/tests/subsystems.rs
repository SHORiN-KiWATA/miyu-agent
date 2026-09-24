//! 子系统启用快照在 Agent 里的落点:清单关着的子系统在构造与 `prepare_for_turn`
//! 两条路上都不构造。

use super::shared::*;
use crate::agent::*;
use miyu_base::config::{AppConfig, PersonaManifest};

fn write_manifest(config: &AppConfig, paths: &MiyuPaths, body: &str) {
    let path = PersonaManifest::manifest_path(config, paths, &config.active_persona_scope());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn build_agent(config: AppConfig, paths: &MiyuPaths, mode: PersonaLane) -> Agent {
    let state = StateStore::new(paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, paths).unwrap();
    Agent::new(config, paths, state, client, ToolRegistry::new(), mode).unwrap()
}

fn system_prompt_of(agent: &Agent) -> String {
    let messages = agent.chat_messages("current", "你好").unwrap().0;
    assert_eq!(messages[0].role, "system");
    chat_message_text(&messages[0]).unwrap()
}

/// 09-16 之前:构造期看清单(没前言),`prepare_for_turn` 重组系统提示词时只看
/// 机器配置——清单关着记忆的人格从第一回合起前言又回来了。
#[test]
fn persona_with_memory_off_never_gets_the_memory_preamble() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    assert!(config.memory_config().enabled, "机器侧记忆默认开着");
    write_manifest(&config, &paths, "[subsystems]\nmemory = false\n");
    let mut agent = build_agent(config, &paths, PersonaLane::Active);
    assert!(
        !system_prompt_of(&agent).contains("<associative-memory>"),
        "构造期就不该有前言"
    );
    agent.prepare_for_turn().unwrap();
    assert!(
        !system_prompt_of(&agent).contains("<associative-memory>"),
        "prepare_for_turn 重组后前言不该回来"
    );
    assert!(!agent.core.subsystems.memory);
}

/// dev 走 core_only 清单 + `dev_scoped` 配置:快照一件都不构造。
#[test]
fn dev_agent_resolves_an_empty_subsystem_set() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.voice.enabled = true;
    config.prompt.persona_reminder = true;
    let agent = build_agent(config, &paths, PersonaLane::Dev);
    assert!(
        agent.core.subsystems.is_empty(),
        "{:?}",
        agent.core.subsystems
    );
}

/// 默认人格、机器全开:两条路上前言都在(没有清单文件时按内置默认全开)。
#[test]
fn default_persona_keeps_the_preamble_on_both_paths() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut agent = build_agent(AppConfig::default(), &paths, PersonaLane::Active);
    assert!(system_prompt_of(&agent).contains("<associative-memory>"));
    agent.prepare_for_turn().unwrap();
    assert!(system_prompt_of(&agent).contains("<associative-memory>"));
}
