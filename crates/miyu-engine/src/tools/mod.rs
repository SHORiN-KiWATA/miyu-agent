mod alarm;
mod apply_patch;
mod archlinux;
mod artifact;
mod share_file;
pub use share_file::set_share_url_bases;
mod ask_question;
mod builtin_plugins;
mod command_guard;
mod compose;
mod compose_core;
mod compose_providers;
mod cross_hints;
mod default_tools;
pub use default_tools::TOOL_SUMMARY_PREFIX;
pub(crate) mod exchange_rate;
pub mod goal;
mod html_conversion;
pub(crate) use miyu_base::http_response;
mod image_generation;
pub mod jobs;
pub mod knowledge_base;
mod ledger;
mod load_tools;
mod mcp;
pub mod memes;
mod memory;
pub mod net_guard;
mod patch_preview;
pub mod platform_outreach;
mod readable_names;
use miyu_base::config::PersonaLane;
#[cfg(test)]
use readable_names::builtin_readable_group_name;
pub(crate) use readable_names::builtin_readable_tool_name;
pub use readable_names::{batch_preparing_phase, preparing_phase, readable_tool_name};
mod registry;
#[cfg(test)]
mod relay_tests;
mod scripts;
mod session_scope;
pub use session_scope::{apply_session_kind_scope, apply_turn_restrictions};
mod skills;
pub mod subagent;
/// 渲染层要认它:认不出的子代理标记不能原样打到屏幕上(hosts 那边的兜底分支)。
pub use subagent::{is_subagent_marker, SUBAGENT_SESSION_EXCLUDED, SUBAGENT_SESSION_MARKER};
pub(crate) use subagent::{peek_subagent_trace, record_subagent_trace, take_subagent_trace};
pub use voice_chat::TOOL_NAME as END_VOICE_CHAT_TOOL;
pub mod subagent_runner;
mod todowrite;
pub(crate) mod voice_chat;
pub(crate) mod voice_speak;
pub use todowrite::{clear_session_todos, session_todos};
pub mod tool_descriptions;
pub use tool_descriptions::tool_event_base_name;
mod tool_display;
pub use tool_display::*;
pub mod usage_query;
pub mod vision;
mod web;
// 平台回合要换一份结果指令(register_platform),所以和 vision/platform_outreach
// 一样对场所层开放。
pub mod web_images;

use miyu_base::config::{AppConfig, PersonaManifest};
use miyu_base::paths::MiyuPaths;
use std::collections::HashMap;
use std::sync::RwLock;

#[cfg(test)]
pub(crate) use registry::empty_parameters;
#[allow(unused_imports)]
pub use registry::{
    CommandOutputStream, GuardCtx, ScriptScope, ToolFuture, ToolGuard, ToolPermission,
    ToolProgress, ToolProgressEvent, ToolRegistry, ToolSpec, ToolTrust,
};
pub use scripts::{
    apply_script_refresh, builtin_scripts_dir, list_global_scripts, list_scripts_for_features,
    list_scripts_with_origin, prepare_script_refresh, scripts_dashboard_delete,
    scripts_dashboard_disable, scripts_dashboard_enable, scripts_dashboard_overview,
    scripts_dashboard_register, scripts_dashboard_source,
};
pub(crate) use skills::{apply_skill_refresh, prepare_skill_refresh};
pub use web::search_for_webui;

/// 把「一串字符串」参数收成 Vec，容忍模型真会传的几种形状。
///
/// stub 加载模式下模型看到的只有一句摘要和宽松参数壳，没取契约就调用时很容
/// 易把数组写成「数组的 JSON 字符串」——实测 mimo-v2.5 在 `reference_images`
/// 上传的就是 `"[\"/path.png\"]"`。只认真数组会让这类调用**静默**失效：参数
/// 明明传了，行为却像没传，排查时要靠返回体里的计数才能发现。
///
/// 收下：真数组、单个字符串、字符串里装的 JSON 数组。空白项一律丢弃。
pub fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    use serde_json::Value;
    let Some(value) = value else {
        return Vec::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Vec::new();
            }
            if text.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                    return string_list(Some(&parsed));
                }
            }
            vec![text.to_string()]
        }
        _ => Vec::new(),
    }
}

pub fn register_ask_question(registry: &mut ToolRegistry) {
    ask_question::register(registry);
}

static SCRIPT_DISPLAY_NAMES: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// 合并登记,不整表替换:同一个 daemon 里 normal / dev / 受限三张表先后都会
/// 登记(TurnResourceCache 一次建三张),dev 表没有脚本、受限表只有 external
/// 脚本,后登记的把先登记的冲掉,WebUI 里属主脚本就显示成裸 id(09-10 沙盒
/// 实测 battery_care / gpustoggle)。名字只增不减:改名后旧名残留无害。
pub fn register_script_display_names(registry: &ToolRegistry) {
    let mut guard = SCRIPT_DISPLAY_NAMES.write().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    for name in registry.tool_names() {
        if let Some(dn) = registry.display_name(&name) {
            map.insert(name, dn);
        }
    }
}

/// 客户端（全屏 TUI、`miyu "…"` 那条 shellhook 路）从 daemon 的事件里学到的显示名。
///
/// 脚本的显示名只在 daemon 里登记过（[`register_script_display_names`]），客户端
/// 进程里那张表是空的——时间线上脚本因此显示成裸 id（用户 09-17：「scripts 接口
/// 接进去的脚本在 timeline 里都没有渲染成显示名称，shellhook 和 TUI 看到了这个
/// 问题，webui 没有」：WebUI 用的是事件里 daemon 算好的 `display_name`）。事件里
/// 带了就记一笔，之后 [`readable_tool_name`] 认得。内建工具不受影响：它们先按内建
/// 表翻，这张表只兜底。
pub fn register_display_name(name: &str, display_name: &str) {
    let name = name.trim();
    let display_name = display_name.trim();
    if name.is_empty() || display_name.is_empty() || name == display_name {
        return;
    }
    if let Ok(mut guard) = SCRIPT_DISPLAY_NAMES.write() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(name.to_string(), display_name.to_string());
    }
}

pub fn clear_aur_review_state(paths: &MiyuPaths) -> anyhow::Result<()> {
    archlinux::aur_review::clear_aur_review_state(paths)
}

/// AUR 装包互斥:review 与 install 不同轮,逼一次"给用户看过再装"的确认。
/// 原为 chat_with_tools 循环里的硬编码特判,迁入 guard 层后对所有分发路径
/// (主循环/子代理/工具桥)一致生效。
pub(crate) fn aur_review_install_guard() -> ToolGuard {
    std::sync::Arc::new(|tool, _args, ctx| {
        (tool.name == "install_aur_package"
            && ctx
                .used_tools
                .iter()
                .any(|name| name == "review_aur_package"))
        .then(|| {
            "install_aur_package cannot run in the same turn as review_aur_package; \
             ask the user to confirm installation first"
                .to_string()
        })
    })
}

/// run_command 命令拒绝子串(config.tools.command_deny)。命中即拒,
/// 防提示注入与模型手滑;拒绝以 tool error 回给模型,轮次存活。
pub(crate) fn command_deny_guard(patterns: Vec<String>) -> ToolGuard {
    std::sync::Arc::new(move |tool, args, _ctx| {
        if tool.name != "run_command" {
            return None;
        }
        let command = args.get("command").and_then(serde_json::Value::as_str)?;
        patterns
            .iter()
            .find(|pattern| !pattern.is_empty() && command.contains(pattern.as_str()))
            .map(|pattern| {
                format!("command contains the denied pattern `{pattern}` and was rejected")
            })
    })
}

/// 清单声明的前置工具(`Requires: a, b` → ToolSpec::requires_prior):本回合
/// 先调用过其中之一才放行。数据驱动,脚本与插件不必各写一个 guard 闭包。
pub(crate) fn requires_prior_guard() -> ToolGuard {
    std::sync::Arc::new(|tool, _args, ctx| {
        if tool.requires_prior.is_empty() {
            return None;
        }
        let satisfied = ctx
            .used_tools
            .iter()
            .any(|used| used != &tool.name && tool.requires_prior.iter().any(|req| req == used));
        (!satisfied).then(|| {
            format!(
                "{} requires calling {} earlier in this turn first",
                tool.name,
                tool.requires_prior.join(" or ")
            )
        })
    })
}

fn install_builtin_guards(registry: &mut ToolRegistry, config: &AppConfig) {
    registry.add_guard(aur_review_install_guard());
    // 高危命令拦截排在子串名单前面:两个闸都命中时,先答的那个决定模型看到
    // 什么。判定、取舍与开关语义见 command_guard 模块头。
    registry.add_guard(command_guard::rm_guard(
        config.tools.block_dangerous_commands,
    ));
    registry.add_guard(command_deny_guard(config.tools.command_deny.clone()));
    registry.add_guard(requires_prior_guard());
}

/// 命令类工具的判定已下沉到 `miyu_base::tool_names`(基础层):中转线桥也要用它,
/// 而中转线不许引用工具层。老路径 `crate::tools::is_command_tool` 保持不变(09-16)。
pub use miyu_base::tool_names::is_command_tool;

/// 场所声明的两件事之一:谁在说话。Owner=属主类入口(终端、本机 WebUI、
/// 语音);External=不可信入口(QQ 群、远端 WebUI 成员),只拿 `Trust: external`
/// 的工具。判官/子代理走的是 `PromptAudience::Internal`,工具面与 Owner 相同,
/// 这里不另设档位(09-16 删掉从未构造过的 `Internal` 变体)。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceTrust {
    Owner,
    External,
}

/// 场所:信任 + 能力。能力位今天只有「能弹问题」(ask_question 需要面板);
/// 浏览器(artifact/share)那两件仍由 WebUI 层按会话追加,因为要会话 id 与库。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Surface {
    pub trust: SurfaceTrust,
    pub interactive_questions: bool,
}

impl Surface {
    pub const fn owner(interactive_questions: bool) -> Self {
        Self {
            trust: SurfaceTrust::Owner,
            interactive_questions,
        }
    }

    pub const fn external() -> Self {
        Self {
            trust: SurfaceTrust::External,
            interactive_questions: false,
        }
    }
}

pub use compose::compose_registry;

/// 不可信场所的工具面(旧 `restricted_platform_registry`):同一条流水线,
/// 末尾按 `Trust: external` 筛。以前是一张硬编码白名单,现在权限位写在每件
/// 工具自己的清单里(内置在 descriptions/*.json,脚本在头部)。
pub fn restricted_platform_registry(config: &AppConfig, paths: &MiyuPaths) -> ToolRegistry {
    let manifest = PersonaManifest::load(config, paths, &config.active_persona_scope());
    compose_registry(config, paths, &manifest, Surface::external())
}

pub fn register_webui_artifact_tools(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    session_id: &str,
) {
    artifact::register_webui(
        registry,
        artifact::artifacts_root(config, paths),
        session_id,
    );
}

/// WebUI 文件分享工具。与 artifact 演示区解耦，单独注册。
pub fn register_webui_share_tools(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    store: miyu_core::state::StateStore,
) {
    share_file::register_webui(registry, config, store);
}

pub fn webui_artifact_manifest(
    config: &AppConfig,
    paths: &MiyuPaths,
    session_id: &str,
) -> anyhow::Result<String> {
    artifact::managed_manifest(&artifact::artifacts_root(config, paths), session_id)
}

pub fn rescope_platform_memory_tools(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    paths: &MiyuPaths,
    context: &dyn miyu_base::platform_types::PlatformToolContext,
    readonly: bool,
) {
    // 记忆按 persona 清单构造:清单关着的人格在平台回合也不能被这里补回三件记忆工具。
    if !config.tools.enabled
        || !PersonaManifest::load(config, paths, &config.active_persona_scope())
            .enabled_subsystems(config)
            .memory
    {
        return;
    }
    for name in ["remember_fact", "search_evicted_context", "recall_memories"] {
        registry.unregister(name);
    }
    let principal = context.principal().stable_key();
    let access = if context.is_admin() {
        miyu_core::memory::MemoryAccess::Privileged
    } else {
        miyu_core::memory::MemoryAccess::principal(principal.clone())
    };
    if readonly {
        memory::register_readonly_with_context(
            registry,
            config.clone(),
            paths.clone(),
            access,
            Some(principal),
            context.sender_display_name(),
        );
    } else {
        memory::register_with_context(
            registry,
            config.clone(),
            paths.clone(),
            access,
            Some(principal),
            context.sender_display_name(),
        );
    }
}

/// Stub loading mode (v7 §八点七): every lazy tool stays registered as a
/// permanently visible stub (real name + one-line summary + permissive
/// parameter shell), so the provider-visible tools array is byte-constant for
/// the whole session; full contracts are fetched on demand through
/// `load_tools` as a tool result that rides the conversation tail.
///
/// "hybrid"/"lazy"(按已加载集合增长声明数组的旧档)09-01 删除,历史配置值
/// 按「需加载」处理——它们同属懒加载家族,悄悄升成 full 会让旧配置的工具面
/// 字节数翻好几倍。
pub fn is_stub_loading_mode(mode: &str) -> bool {
    matches!(mode.trim(), "stub" | "hybrid" | "lazy")
}

/// 本次请求的有效工具加载模式,按候选模型池解析。
///
/// 单成员规则:模型级覆盖(`provider.model_tools_loading_mode`)优先,缺项回退
/// 全局 `tools.loading_mode`。池级规则:任一成员要求 full 则整池 full——
/// 一次请求只有一张工具面,而命中主回合池里哪个模型(以及故障转移换给谁)是
/// 发送时才决定的,这张脸必须让池里任何成员都能用;full 全兼容,stub 只是省
/// token 的优化,「就高不就低」恒安全。
///
/// 只看**主回合池** `active_provider_models`——工具面是给主回合用的。多模态池
/// (`active_multimodal_provider_models`)只喂看图子分析(describe.rs),从不处理
/// 带工具的主回合;09-01 起初误把它并进候选,导致文本池是 opus(stub)、多模态池
/// 里配了 full 的 glm 时,每个 opus 回合都被拖成 full(用户实测暴露)。
///
/// 背景(09-01):约束解码型供应商(实测 bigmodel glm-5.3-flash)把工具参数
/// 生成硬限制在声明 schema 内,空壳 stub 让它永远只能发 `{}`,契约文本在
/// 对话里也救不回(裸 API 8/8 复现)。给这类模型配模型级 full,其余照旧 stub。
pub fn effective_tools_loading_mode(config: &AppConfig) -> String {
    let canonical = |mode: &str| {
        if mode.trim() == "full" {
            "full"
        } else {
            "stub"
        }
    };
    let global = canonical(&config.tools.loading_mode);
    let mut any = false;
    for entry in config.active_provider_models.iter().flatten() {
        any = true;
        let mode = config
            .providers
            .iter()
            .find(|provider| provider.id == entry.provider_id)
            .and_then(|provider| provider.model_tools_loading_mode.get(&entry.model))
            .map(|mode| canonical(mode))
            .unwrap_or(global);
        if mode == "full" {
            return "full".to_string();
        }
    }
    if any { "stub" } else { global }.to_string()
}

/// 按模式与配置组装工具注册表：REPL、daemon、WebUI、子代理都从这里拿。
///
/// 组装顺序有意义，不是随手排的：
///
/// 1. 先按模式选底座（`normal` 面向日常对话，`dev` 面向写代码），工具总开关
///    关掉时给一个空注册表而不是提前返回——调用方拿到的永远是同一个类型。
/// 2. 技能只在工具开着时注册；技能创作工具（`manage_skill`）只在 normal 模式
///    出现，dev 模式下模型该写代码不该写技能。
/// 3. `ask_question` 单独由调用方决定：daemon 与 WebUI 能弹面板，一次性
///    `miyu ask` 不能，所以它是参数而不是模式的函数。
/// 4. 最后登记脚本工具的显示名——这一步要在所有注册之后，否则新注册的脚本
///    在渲染层会显示成原始工具名。
///
/// 这个函数原本长在 `cli.rs` 里，于是 `web` 和 `tools` 都得反过来
/// `use crate::cli`，把两个底层模块钉死在最上层。它实际只依赖
/// tools/config/paths/agent，与 CLI 毫无关系，所以下沉到这里——拆分要断的
/// 两条边（`web→cli`、`tools→cli`）一次都断掉。
pub fn build_tool_registry(
    config: &AppConfig,
    paths: &MiyuPaths,
    lane: PersonaLane,
    interactive_questions: bool,
) -> anyhow::Result<ToolRegistry> {
    // 车道只说「哪个 persona」:Dev = 保留人格 "dev"(清单默认 core_only),
    // Active = 当前人格。真正裁决工具面的是 persona 清单。
    let persona = lane.scope(config);
    let mut registry = if config.tools.enabled {
        let manifest = PersonaManifest::load(config, paths, &persona);
        compose_registry(
            config,
            paths,
            &manifest,
            Surface::owner(interactive_questions),
        )
    } else {
        ToolRegistry::new()
    };
    // 开发模式的「发到 QQ」只给管理员、没有地址簿(用户 09-18 裁定);内置插件
    // 表不知道车道,装的是普通版,这里收窄。ws 没连上时本来就没装,不会多出来。
    if lane == PersonaLane::Dev {
        platform_outreach::restrict_to_admins(
            &mut registry,
            config,
            platform_outreach::Surface::Terminal,
        );
    }
    // 最后登记脚本工具的显示名——要在所有注册之后,否则新注册的脚本在渲染层
    // 会显示成原始工具名。
    register_script_display_names(&registry);
    Ok(registry)
}

#[cfg(test)]
mod compose_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tier_schema_probe;

/// 三张注册表的形状指纹(09-10 分层架构阶段 4 的安全网):名字 → 定义 JSON 的
/// sha256。三表合一之后 normal/dev/受限三个面的 tools 数组必须逐字节不变——
/// 这是 AGENTS §1.1 的前缀契约,也是 QQ 会话不冷启动的保证。刻意的变化要改
/// 夹具并在提交说明里写清楚。
#[cfg(test)]
mod shape_tests;

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
