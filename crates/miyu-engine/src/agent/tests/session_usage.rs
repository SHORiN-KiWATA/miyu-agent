//! 「本会话用量」挂在 Agent 层:判据是会话取值器给不给得出数,不是工具面里
//! 有没有全局那件。dev 的 `core_only` 清单没有 `usage_query` 插件,正是这条
//! 分叉的样本(用户 09-23:写代码时全局账不影响任何决策)。

use super::shared::*;
use crate::agent::*;
use miyu_base::config::AppConfig;

fn agent_with(mode: PersonaLane, config: AppConfig, paths: &MiyuPaths) -> Agent {
    let state = StateStore::new(paths).unwrap();
    state.init_files().unwrap();
    let client =
        OpenAiCompatibleClient::new(config.provider(None).unwrap(), &config, paths).unwrap();
    let tools = crate::tools::build_tool_registry(&config, paths, mode, false).unwrap();
    Agent::new(config, paths, state, client, tools, mode).unwrap()
}

/// 退回修复前(以 `query_system_token_usage` 在不在为前置)这条会红:dev 面
/// 没有全局那件,本会话那件就也不装——而它恰恰是 dev 里唯一问得到「这个
/// 会话烧了多少」的途径。
#[test]
fn dev_agent_gets_the_session_usage_tool_without_the_global_one() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let tools =
        crate::tools::build_tool_registry(&config, &paths, PersonaLane::Dev, false).unwrap();
    assert!(
        !tools.contains("query_system_token_usage"),
        "dev 面本来就没有全局那件,这是用例的前提"
    );
    let agent = agent_with(PersonaLane::Dev, config, &paths);
    let names = agent.tools.lock().unwrap().tool_names();
    assert!(
        names.contains(&"query_session_token_usage".to_string()),
        "dev 会话问不到本会话用量: {names:?}"
    );
}

/// 原判据的覆盖面不能丢:全局那件开着的人格,两件都在。
#[test]
fn normal_agent_keeps_both_usage_tools() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let agent = agent_with(PersonaLane::Active, AppConfig::default(), &paths);
    let names = agent.tools.lock().unwrap().tool_names();
    for name in ["query_system_token_usage", "query_session_token_usage"] {
        assert!(names.contains(&name.to_string()), "缺 {name}: {names:?}");
    }
}
