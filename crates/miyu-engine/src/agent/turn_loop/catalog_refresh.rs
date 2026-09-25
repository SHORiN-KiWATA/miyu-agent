//! 每一轮模型请求之前的工具目录整备:脚本 / 技能目录热刷新,以及这一轮真正发给
//! 模型的工具定义(按加载模式给全量或桩)。09-17 从 `chat_with_tools` 里抽出。

use crate::agent::*;

impl Agent {
    /// 脚本目录与技能目录按指纹热刷新;指纹没变一次锁都不拿。失败只告警,不中断回合。
    pub(super) async fn refresh_tool_catalogs(&mut self) {
        // 脚本目录刷新独立于 skills.enabled(09-05):此前套在 skills 开关里,
        // 关掉技能就没人再热加载脚本了。指纹没变一次锁都不拿。dev 工具面没有
        // 脚本,不刷。
        if !self.core.dev {
            let current_fingerprint = self.tools.lock().unwrap().script_catalog_fingerprint();
            let config = self.core.config.clone();
            let paths = self.core.paths.clone();
            let refresh = tokio::task::spawn_blocking(move || {
                tools::prepare_script_refresh(current_fingerprint, &config, &paths)
                    .map(|snapshot| (snapshot, paths))
            })
            .await;
            match refresh {
                Ok(Ok((Some(snapshot), paths))) => {
                    let mut registry = self.tools.lock().unwrap();
                    tools::apply_script_refresh(&mut registry, &paths, snapshot);
                    tools::register_script_display_names(&registry);
                }
                Ok(Ok((None, _))) => {}
                Ok(Err(error)) => {
                    tracing::warn!(error = %error, "failed to refresh Miyu script tools")
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Miyu script refresh worker stopped")
                }
            }
        }

        if self.core.config.skills.enabled {
            let current_fingerprint = {
                let registry = self.tools.lock().unwrap();
                registry
                    .contains("load_skill")
                    .then(|| registry.skill_catalog_fingerprint())
            };
            if let Some(current_fingerprint) = current_fingerprint {
                let config = self.core.config.clone();
                let paths = self.core.paths.clone();
                let refresh = tokio::task::spawn_blocking(move || {
                    tools::prepare_skill_refresh(current_fingerprint, &config, &paths)
                })
                .await;
                match refresh {
                    Ok(Ok(Some(snapshot))) => {
                        let mut registry = self.tools.lock().unwrap();
                        tools::apply_skill_refresh(&mut registry, snapshot);
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(error = %error, "failed to refresh Miyu skill catalog")
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "Miyu skill catalog worker stopped")
                    }
                }
            }
        }
    }

    /// 这一轮发给模型的工具定义:工具关着或复读保险丝熔断时一个不给。
    pub(in crate::agent) fn round_tool_definitions(
        &self,
        withhold: bool,
    ) -> Vec<miyu_core::llm::ToolDefinition> {
        if self.core.tools_enabled && !withhold {
            let mut tools = self.tools.lock().unwrap();
            self.enforce_turn_restrictions(&mut tools);
            // 有效模式按候选模型池解析(模型级覆盖,任一成员要 full 则整池
            // full)——约束解码型模型吃不下空壳 stub(09-01)。
            tools.request_definitions(tools::is_stub_loading_mode(
                &tools::effective_tools_loading_mode(&self.core.config),
            ))
        } else {
            Vec::new()
        }
    }

    /// 把这一轮登记的单轮覆盖项(工具白名单、不写记忆)再落一遍到自己的工具表上。
    ///
    /// 回合装配已经按它裁过,但 Agent 自己还会晚注册几件——本会话用量
    /// (`bind_session_usage`,09-22)、带图时的看图工具——它们绕过了那一道,普通
    /// 模型的 `miyu ask --no-tools` 照样拿得到(09-23 CLI 黑盒)。每次取定义前过一遍,
    /// 谁什么时候注册进来都拦得住;摘掉之后模型硬调也只会得到「没有这件工具」。
    /// 不带覆盖项时什么都不做,工具表一个字节都不动。
    pub(in crate::agent) fn enforce_turn_restrictions(&self, tools: &mut tools::ToolRegistry) {
        let restrictions =
            miyu_base::host_ports::live_turn_tool_restrictions(&self.state.session_id());
        tools::apply_turn_restrictions(tools, &restrictions);
    }
}
