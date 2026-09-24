//! 「本会话用量」挂在 Agent 层:判据是会话取值器给不给得出数,不是工具面里
//! 有没有全局那件。dev 的 `core_only` 清单没有 `usage_query` 插件,正是这条
//! 分叉的样本(用户 09-23:写代码时全局账不影响任何决策)。

use super::shared::*;
use crate::agent::*;
use miyu_base::config::AppConfig;

fn agent_with(mode: PersonaLane, config: AppConfig, paths: &MiyuPaths) -> Agent {
    let state = StateStore::new(paths).unwrap();
    state.init_files().unwrap();
    agent_on(mode, config, paths, state)
}

/// 挂在一条自己的会话上。单轮限制的登记表是进程级的、按会话号分：拿人人都有的
/// `default` 登记，同一进程里并发跑的别的用例取工具定义时也会读到这份限制（09-24）。
fn agent_on_own_session(mode: PersonaLane, config: AppConfig, paths: &MiyuPaths) -> Agent {
    let state = StateStore::new(paths).unwrap();
    state.init_files().unwrap();
    let own = state
        .create_session("default", "restricted turn", "user", None)
        .unwrap();
    agent_on(mode, config, paths, state.pinned(&own.session_id))
}

fn agent_on(mode: PersonaLane, config: AppConfig, paths: &MiyuPaths, state: StateStore) -> Agent {
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

/// 单轮覆盖项(09-23):本会话用量是 Agent 自己晚注册的,回合装配那道裁剪拦不住它,
/// 普通模型的 `miyu ask --no-tools` 原来照样把它发给模型(CLI 黑盒「--no-tools →
/// tools 空」一直红)。Agent 取定义前按登记表再落一遍。
#[test]
fn a_restricted_turn_does_not_leak_the_late_registered_usage_tool() {
    use miyu_base::host_ports::{LiveTurnToolRestrictionsGuard, TurnToolRestrictions};
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let agent = agent_on_own_session(PersonaLane::Active, AppConfig::default(), &paths);
    let session = agent.state.session_id();
    assert!(!session.is_empty(), "用例前提:Agent 绑着会话");
    let names = |agent: &Agent| {
        agent
            .live_tool_definitions()
            .unwrap()
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>()
    };
    assert!(names(&agent).contains(&"query_session_token_usage".to_string()));
    {
        let _turn = LiveTurnToolRestrictionsGuard::register(
            &session,
            TurnToolRestrictions {
                allowlist: Some(Vec::new()),
                no_memory_writes: false,
            },
        );
        assert_eq!(names(&agent), Vec::<String>::new());
    }
    let agent = agent_on_own_session(PersonaLane::Active, AppConfig::default(), &paths);
    let session = agent.state.session_id();
    let _turn = LiveTurnToolRestrictionsGuard::register(
        &session,
        TurnToolRestrictions {
            allowlist: Some(vec!["read".into()]),
            no_memory_writes: false,
        },
    );
    let left = names(&agent);
    assert!(
        !left.contains(&"query_session_token_usage".to_string()),
        "{left:?}"
    );
    assert!(left.iter().all(|name| name == "read"), "{left:?}");
}

/// 回合中途问(它只会在回合中途被调):这一轮已经发出去的请求要算进累计,上下文要是
/// 这一次请求的——和 footer 同一个数。退回修复前(只读库里落了账的)这条会红:头一轮
/// 问,累计是 0,还说「还没有落账的用量」(用户 09-24)。
#[tokio::test]
async fn the_session_usage_tool_counts_the_turn_in_progress() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let agent = agent_with(PersonaLane::Active, AppConfig::default(), &paths);
    agent.runtime.turn_usage.set(TurnTokens {
        total: 4_321,
        prompt: 4_000,
        cache_read: 3_000,
    });
    agent.runtime.turn_usage.set_context(4_100);
    let output = {
        let registry = agent.tools.lock().unwrap();
        registry
            .call("query_session_token_usage", "{}")
            .await
            .unwrap()
    };
    assert!(!output.contains("还没有"), "{output}");
    assert!(
        output.contains("这个会话累计 **"),
        "spent must include the turn so far: {output}"
    );
    assert!(
        output.contains("当前上下文 **"),
        "context must be this turn's latest request: {output}"
    );
}
