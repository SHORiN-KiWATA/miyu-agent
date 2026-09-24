//! Agent 的构造与逐回合配置。
//!
//! `set_*` 那一族是「这一回合的外部条件」：平台身份、上下文图片、记忆开关。
//! 分开设而不是塞进构造函数，是因为它们来自不同的调用方，而且不是每回合都有。
//!
//! `start_cache_keepalive` 定期发极小请求续上供应商的前缀缓存——缓存有 TTL，
//! 过期就从 token 0 重算。

use crate::agent::*;
use miyu_base::config::PersonaManifest;

/// 会按会话状态增删的工具(dev 专用):建表时判据还不存在,原件抓在
/// `Agent` 上,每回合由 `apply_situational_tools` 决定挂不挂。
const SITUATIONAL_TOOLS: &[&str] = &["load_tools"];

fn situational_tool_specs(tools: &ToolRegistry, dev: bool) -> Vec<Arc<crate::tools::ToolSpec>> {
    if !dev {
        return Vec::new();
    }
    SITUATIONAL_TOOLS
        .iter()
        .filter_map(|name| tools.shared(name))
        .collect()
}

/// Agent 的装配档位(09-18 子代理会话化)。`Persona` 是普通会话;`Subagent` 是子会话
/// 的素档,见 [`Agent::new_with_profile`]。dev 不在这根轴上——它由 [`PersonaLane`] 折出。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentProfile {
    Persona,
    Subagent,
}

impl Agent {
    pub fn new(
        config: AppConfig,
        paths: &MiyuPaths,
        state: StateStore,
        client: OpenAiCompatibleClient,
        tools: ToolRegistry,
        lane: PersonaLane,
    ) -> Result<Self> {
        Self::new_for_audience(
            config,
            paths,
            state,
            client,
            tools,
            lane,
            PromptAudience::Owner,
        )
    }

    pub fn new_for_audience(
        config: AppConfig,
        paths: &MiyuPaths,
        state: StateStore,
        client: OpenAiCompatibleClient,
        tools: ToolRegistry,
        lane: PersonaLane,
        prompt_audience: PromptAudience,
    ) -> Result<Self> {
        Self::new_with_profile(
            config,
            paths,
            state,
            client,
            tools,
            lane,
            prompt_audience,
            AgentProfile::Persona,
        )
    }

    /// 带档位的构造(09-18 子代理会话化):`AgentProfile::Subagent` 是子会话的素档——
    /// 非 dev 时系统提示词换成通用子代理那份、五个子系统整套不构造(记忆不建库、
    /// 不注入、不写日记;人格提醒/语音/情绪关;技能不注册)、预设对话跳过。工具面
    /// 由场所按父会话的车道减排除表给,这里不管。dev 子代理 = dev 人格,与 dev 会话
    /// 逐字节同源,前缀缓存共享。
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_profile(
        config: AppConfig,
        paths: &MiyuPaths,
        state: StateStore,
        client: OpenAiCompatibleClient,
        tools: ToolRegistry,
        lane: PersonaLane,
        prompt_audience: PromptAudience,
        profile: AgentProfile,
    ) -> Result<Self> {
        let subagent = profile == AgentProfile::Subagent;
        // Construction is side-effect free (aside from idempotent memory
        // init) so concurrent turns can each build their own Agent; startup
        // maintenance (prompt-change reset, stale-turn recovery) lives in
        // `prepare_for_turn`.
        // 场所的人格车道在这里折成人格面:dev 走保留人格 "dev" 的作用域,记忆
        // 整套在这里关掉(09-09),派生目录也随之隔离,免得日后重开时读到默认
        // 人格的库。往下的一切只看 `dev` 与人格清单,不再看模式。
        let dev = lane.is_dev();
        let config = if dev { config.dev_scoped() } else { config };
        // claude-code 中转的双四档工具作用域按人格面裁决;其他协议无感。
        let client = client.with_claude_code_dev_mode(dev);
        // opencode Zen 的会话头按这个走:一次对话对应服务端一个会话。
        let client = client.with_zen_session(&state.session_id());
        // 记账日志的会话归属(与 zen 的请求头分开,语义各管各的)。
        let client = client.with_log_session(&state.session_id());
        let base_system_prompt = persona_system_prompt(
            &config,
            paths,
            dev,
            subagent,
            prompt_audience,
            user_profile_applies(prompt_audience, false),
        )?;
        // 子系统启用快照:人格清单 × 机器配置,构造期折一次,各挂接点只看它。
        // 子会话档一位不开:子代理的记忆/人格提醒/语音/情绪都不该有(老循环本就没有,
        // 而且它写进记忆库的东西会污染主会话的记忆)。
        let subsystems = if subagent {
            miyu_base::config::EnabledSubsystems::default()
        } else {
            PersonaManifest::load(&config, paths, &config.active_persona_scope())
                .enabled_subsystems(&config)
        };
        let system_prompt = with_memory_preamble(
            with_host_environment(
                base_system_prompt,
                prompt_audience,
                paths,
                &config,
                dev,
                subsystems.voice,
                false,
            ),
            subsystems.memory,
        );
        let tools_enabled = config.tools.enabled;
        let max_tool_rounds = config.tools.max_rounds;
        // dev 无人格:预设对话整套跳过。子会话档同理。
        let preset_dialogs = if dev || subagent {
            Vec::new()
        } else {
            persona_hint::load_dialogs(&config, paths, &config.active_persona_scope())
        };
        let situational_tools = situational_tool_specs(&tools, dev);
        // 会话标记与 memory_origin 同源:日记记的和事实记的必须是同一个
        // 会话,否则会话级重置只清得掉一半。
        let memory_origin = MemoryOrigin::local(state.session_id().to_string());
        let memory = MemoryStore::new(&config, paths).with_session_id(&memory_origin.session_id);
        // 记忆关着就不建库(dev 走这条):`init`/`identity` 都会顺手创建
        // 库文件并跑一次衰减,而这条路上没有任何东西会去读它。库身份只被
        // 写日记/redo 的一致性校验用,那两处在关闭时先一步早退。
        // 记忆子系统按 persona 清单构造:清单关着就不建库、不注入、不写日记。
        let (memory_database_id, memory_generation) = if subsystems.memory {
            memory.init()?;
            memory.identity()?
        } else {
            (String::new(), 0)
        };
        let on_overflow = config.context.on_overflow.clone();
        // 「这个会话烧了多少」得等会话定下来才知道，而工具表是装配层按人格建
        // 的、那时还没有会话。这里把取值器塞进去（同名覆盖，工具名与描述不变，
        // 清单一个字节都不动）。闭包捕获的是 `StateStore` 而不是会话 id ——
        // 它的会话是共享可换的，`/session` 切过去之后所有持有者一起换，跨回合
        // 复用的 Agent 也照样问得出当前那条（用户 09-22 提议走工具，不往提示词
        // 里塞常驻字段）。
        let mut tools = tools;
        let turn_usage = TurnUsageMirror::default();
        bind_session_usage(&mut tools, &state, &client, &config, &turn_usage);
        Ok(Self {
            state,
            client,
            system_prompt,
            input: TurnInput {
                memory_content: None,
                suppress_session_history: false,
                runtime_system_context: Vec::new(),
                turn_system_context: Vec::new(),
                system_prompt_override: None,
                context_window_override: None,
                turn_display_content: None,
                attachment_run_id: None,
                image_platform: None,
                image_platform_label: None,
                platform_context: None,
                context_images: Vec::new(),
                context_files: Vec::new(),
            },
            core: CoreTurnSnapshot {
                dev,
                subagent,
                prompt_audience,
                paths: paths.clone(),
                subsystems,
                trim_at_ratio: config.context.trim_at_ratio,
                compact_at_ratio: config.context.effective_compact_at_ratio(),
                trim_batch_ratio: config.context.trim_batch_ratio,
                tools_enabled,
                max_tool_rounds,
                on_overflow,
                spinner_interval: miyu_base::terminal::SPINNER_INTERVAL,
                config,
            },
            tools: Arc::new(Mutex::new(tools)),
            situational_tools,
            memory: MemorySubsystem {
                store: memory,
                organizer: None,
                origin: memory_origin,
                database_id: memory_database_id,
                generation: memory_generation,
            },
            runtime: TurnRuntime {
                persona_reminder: None,
                turn_usage,
                last_request_snapshot: None,
                pending_remote_tool_calls: std::sync::Mutex::new(Vec::new()),
                last_request_endpoint: None,
                keepalive_cancel: None,
                consecutive_compacts: std::sync::atomic::AtomicU32::new(0),
                compact_stuck: std::sync::atomic::AtomicBool::new(false),
                last_compact_max_seq: std::sync::atomic::AtomicI64::new(-1),
                rapid_compacts: std::sync::atomic::AtomicU32::new(0),
            },
            preset_dialogs,
        })
    }

    /// daemon 内跑的回合调用：SpinnerTick 出不了进程（event_map 丢弃，
    /// REPL/一次性会话的动画由 CLI 本地定时器驱动），只剩 journal 尾部
    /// 冲刷的兜底作用，降到 200ms。终端直连（CLI direct）不要调，动画
    /// 帧率靠 40ms。
    pub fn with_headless_pacing(mut self) -> Self {
        self.core.spinner_interval = std::time::Duration::from_millis(200);
        self
    }

    pub fn cancel_cache_keepalive(&mut self) {
        if let Some(cancel) = self.runtime.keepalive_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Starts the idle keepalive loop for the last request prefix. No-op when
    /// disabled or when no snapshot exists.
    pub(in crate::agent) fn start_cache_keepalive(&mut self) {
        self.cancel_cache_keepalive();
        let interval = self.core.config.cache.keepalive_seconds;
        if interval == 0 {
            return;
        }
        let Some((messages, tools)) = self.runtime.last_request_snapshot.clone() else {
            return;
        };
        let endpoint_hint = self.runtime.last_request_endpoint.clone();
        let max_pings = self.core.config.cache.keepalive_max_pings;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.runtime.keepalive_cancel = Some(cancel.clone());
        let client = self.client.clone();
        let state = self.state.clone();
        let usage_source = self.usage_source().to_string();
        tokio::spawn(async move {
            for ping in 0..max_pings {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                match client
                    .cache_keepalive(messages.clone(), tools.clone(), endpoint_hint.as_ref())
                    .await
                {
                    Ok(Some(usage)) => {
                        tracing::info!(
                            ping = ping + 1,
                            prompt_tokens = usage.prompt_tokens,
                            cache_read = usage.cache_read_tokens,
                            "cache keepalive ping"
                        );
                        let meta = miyu_core::state::UsageMeta {
                            source: &usage_source,
                            provider: Some(client.provider_id()),
                            model: None,
                            kind: None,
                        };
                        let _ = state.add_auxiliary_usage(&usage, meta);
                    }
                    Ok(None) => return, // protocol without keepalive support
                    Err(error) => {
                        tracing::warn!(error = %error, "cache keepalive ping failed");
                        return;
                    }
                }
            }
        });
    }

    pub(in crate::agent) fn usage_source(&self) -> &str {
        self.input
            .platform_context
            .as_ref()
            .map(|context| context.platform_name())
            .unwrap_or("agent")
    }

    pub fn prepare_for_turn(&mut self) -> Result<()> {
        // (档案进不进由 user_profile_applies 决定:平台回合不进。)
        let persona_prompt = persona_system_prompt(
            &self.core.config,
            &self.core.paths,
            self.core.dev,
            self.core.subagent,
            self.core.prompt_audience,
            user_profile_applies(
                self.core.prompt_audience,
                self.input.platform_context.is_some(),
            ),
        )?;
        {
            // 指纹永远按人格提示词算,不看整体替换的覆盖:覆盖是回合级
            // 瞬态,进指纹会让每个带覆盖的回合都翻转一次指纹文件。
            let fingerprint_prompt = if self.core.dev || self.core.subagent {
                persona_prompt.clone()
            } else {
                self.core.config.base_system_prompt(&self.core.paths)?
            };
            let compatible_previous = matches!(self.core.prompt_audience, PromptAudience::Owner)
                .then_some(persona_prompt.as_str());
            self.state.reset_if_prompt_changed_with_compatible(
                &fingerprint_prompt,
                compatible_previous,
            )?;
            self.state.recover_stale_turns()?;
        }
        self.system_prompt = self.assemble_system_prompt(persona_prompt);
        self.apply_situational_tools();
        Ok(())
    }

    /// 情境化工具的回合级增删(dev 专用,09-09)。
    ///
    /// 判据要会话状态,而工具表在 daemon 启动时就建好了,只能在每回合装配
    /// 时决定。工具目录是缓存前缀的一部分,改它=整条前缀作废,所以判据
    /// 必须挑在「本轮前缀反正已经断了」的时刻才翻转。
    ///
    /// `load_tools`:full 档下模型压根看不见它,也就不会调;留着的唯一理由
    /// 是历史里已有调用记录(会话中途从需加载模型切过来)时它不能变成未知
    /// 工具——而换档本身就换掉了整个工具面。
    ///
    /// 摘掉的工具原件留在 `situational_tools` 里:REPL 的 Agent 跨回合复用,
    /// 判据翻真时得原样放回去,而 spec 里裹着闭包,重建不如留原件。
    fn apply_situational_tools(&self) {
        if !self.core.dev || self.situational_tools.is_empty() {
            return;
        }
        let stub_mode =
            tools::is_stub_loading_mode(&tools::effective_tools_loading_mode(&self.core.config));
        // 判据取不到就当「保留」:少一件工具只是省字节,凭空少一件却会让
        // 模型照着历史撞未知工具,两种错的代价不对称。
        let session_loaded_anything = self
            .state
            .load_session_loaded_tools()
            .map(|loaded| !loaded.is_empty())
            .unwrap_or(true);
        let mut registry = self.tools.lock().unwrap();
        for spec in &self.situational_tools {
            let keep = match spec.name.as_str() {
                "load_tools" => stub_mode || session_loaded_anything,
                _ => true,
            };
            if keep {
                registry.register_shared(spec.clone());
            } else {
                registry.unregister(&spec.name);
            }
        }
    }

    /// 人格提示词之上的固定叠加顺序(顺序即缓存前缀,见 prompt 模块头)。
    /// 有整体替换覆盖时:覆盖文本顶掉人格提示词与属主主机环境块;
    /// 运行时追加段与记忆前言照旧。
    fn assemble_system_prompt(&self, persona_prompt: String) -> String {
        match &self.input.system_prompt_override {
            Some(override_prompt) => with_memory_preamble(
                with_runtime_system_context(
                    override_prompt.clone(),
                    &self.input.runtime_system_context,
                ),
                self.core.subsystems.memory,
            ),
            None => with_memory_preamble(
                with_host_environment(
                    with_runtime_system_context(persona_prompt, &self.input.runtime_system_context),
                    self.core.prompt_audience,
                    &self.core.paths,
                    &self.core.config,
                    self.core.dev,
                    self.core.subsystems.voice,
                    self.input.platform_context.is_some(),
                ),
                // 清单 × 机器配置的快照,与构造期同一判据(09-16 之前这里只看机器
                // 配置,清单关着记忆的人格第一回合起前言又回来了)。
                self.core.subsystems.memory,
            ),
        }
    }

    /// 程序驱动 CLI 的整体替换提示词;`prepare_for_turn` 之前调用才生效。
    pub fn set_system_prompt_override(&mut self, prompt: String) {
        let prompt = prompt.trim().to_string();
        self.input.system_prompt_override = (!prompt.is_empty()).then_some(prompt);
    }

    /// 程序驱动 CLI 的本回合上下文窗口;0 视作不覆盖。
    pub fn set_context_window_override(&mut self, window: usize) {
        self.input.context_window_override = (window > 0).then_some(window);
    }

    pub fn set_runtime_system_context(&mut self, context: Vec<String>) -> Result<()> {
        self.input.runtime_system_context = context
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        self.refresh_system_prompt()
    }

    /// Per-message transport blocks that ride the turn tail (after the user
    /// message) instead of the system prompt. No prompt refresh needed: they
    /// are consumed at message-assembly time.
    /// Raw input for the memory diary; `None` falls back to the turn content.
    pub fn set_memory_content(&mut self, content: Option<String>) {
        self.input.memory_content = content.filter(|text| !text.trim().is_empty());
    }

    pub fn set_turn_system_context(&mut self, context: Vec<String>) {
        self.input.turn_system_context = context
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
    }

    pub fn set_memory_writes_enabled(&mut self, enabled: bool) {
        self.memory.store.set_writes_enabled(enabled);
    }

    pub fn set_memory_organizer(&mut self, organizer: MemoryOrganizerHandle) {
        self.memory.organizer = Some(organizer);
    }

    pub fn set_memory_origin(&mut self, origin: MemoryOrigin) {
        self.memory.store.set_session_id(&origin.session_id);
        self.memory.origin = origin;
    }

    pub fn set_memory_request_context(
        &mut self,
        access: MemoryAccess,
        writer_principal: Option<String>,
        writer_display_name: impl Into<String>,
    ) {
        self.memory
            .store
            .set_request_context(access, writer_principal, writer_display_name);
    }

    pub fn set_image_platform(&mut self, platform: &str, display_name: &str) {
        let platform = platform
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .collect::<String>();
        self.input.image_platform = (!platform.is_empty()).then_some(platform);
        self.input.image_platform_label = self.input.image_platform.as_ref().and_then(|_| {
            (!display_name.trim().is_empty()).then(|| display_name.trim().to_string())
        });
    }

    pub fn set_platform_context_images(
        &mut self,
        context: Arc<dyn PlatformTurn>,
        images: Vec<PlatformContextImageRef>,
    ) {
        self.input.platform_context = Some(context);
        self.input.context_images = images;
    }

    pub fn set_platform_context_files(
        &mut self,
        context: Arc<dyn PlatformTurn>,
        files: Vec<PlatformContextFileRef>,
    ) {
        self.input.platform_context = Some(context.clone());
        self.input.context_files = files.clone();
        if self.core.tools_enabled {
            let mut tools = self.tools.lock().unwrap();
            context.register_file_reader(&mut tools, files);
        }
    }

    pub fn set_turn_persistence(
        &mut self,
        display_content: String,
        attachment_run_id: Option<String>,
    ) {
        self.input.turn_display_content = Some(display_content);
        self.input.attachment_run_id = attachment_run_id;
    }

    pub fn set_session_history_suppressed(&mut self, suppressed: bool) {
        self.input.suppress_session_history = suppressed;
    }

    /// Rebuilds the system prompt for the current persona face without running
    /// turn-entry maintenance. Used for mid-turn mode switches, where
    /// `reset_if_prompt_changed` must never fire (it would wipe the very
    /// turn that is running).
    pub(in crate::agent) fn refresh_system_prompt(&mut self) -> Result<()> {
        let persona_prompt = persona_system_prompt(
            &self.core.config,
            &self.core.paths,
            self.core.dev,
            self.core.subagent,
            self.core.prompt_audience,
            user_profile_applies(
                self.core.prompt_audience,
                self.input.platform_context.is_some(),
            ),
        )?;
        self.system_prompt = self.assemble_system_prompt(persona_prompt);
        Ok(())
    }

    /// 回答场所「现在走哪条人格车道」:由人格面折回。
    pub fn persona_lane(&self) -> PersonaLane {
        PersonaLane::from_dev(self.core.dev)
    }

    pub fn context_window(&self) -> Option<usize> {
        if let Some(window) = self.input.context_window_override {
            return Some(window);
        }
        self.client.context_window(&self.core.config).ok().flatten()
    }

    /// 上面那个数是不是猜的。猜的时候 footer 不能拿它算百分比。
    pub fn context_window_assumed(&self) -> bool {
        if self.input.context_window_override.is_some() {
            return false;
        }
        matches!(
            self.client
                .context_window_with_source(&self.core.config)
                .ok()
                .flatten(),
            Some((_, miyu_base::config::ContextWindowSource::Assumed))
        )
    }

    /// Same Σ with the prompt and cache-read halves its cache rate needs.
    pub fn conversation_usage_token_totals(&self) -> Result<TurnTokens> {
        self.state.session_cumulative_token_totals()
    }

    pub(in crate::agent) fn tool_definition_tokens(&self) -> usize {
        let tools = self.tools.lock().unwrap();
        // footer 与溢出记账必须数「真发出去的那一份」：走同一个出口，
        // 否则 full 档会把不再发送的 load_tools 也算进上下文。
        let definitions = tools.request_definitions(tools::is_stub_loading_mode(
            &tools::effective_tools_loading_mode(&self.core.config),
        ));
        estimate_tool_definition_tokens(&definitions)
    }

    /// 回合中途场所换了会话形态(REPL `/dev` 开关):折成人格面、换工具面、重算
    /// 预设对话。config 不在这里重新作用域化——记忆库与派生目录是构造期定的,
    /// 回合里换库会让日记落到另一个命名空间。
    pub fn switch_lane(&mut self, lane: PersonaLane, tools: ToolRegistry) {
        self.core.dev = lane.is_dev();
        // 情境化工具的原件跟着新表走:两张表是分别建的,拿旧表的 Arc 去
        // 新表上放回,等于把上一人格面的工具塞进这一面。
        self.situational_tools = situational_tool_specs(&tools, self.core.dev);
        self.tools = Arc::new(Mutex::new(tools));
        // 预设对话跟人格走:Normal↔Dev 切换后必须重算,否则 Dev 带着
        // 人格 dialogs(违反"Dev 无人格"),Dev→Normal 则永远没有。
        self.refresh_preset_dialogs();
    }

    pub(in crate::agent) fn refresh_preset_dialogs(&mut self) {
        // dev 无人格:预设对话整套跳过(与构造期一致);子会话档同理。
        self.preset_dialogs = if self.core.dev || self.core.subagent {
            Vec::new()
        } else {
            persona_hint::load_dialogs(
                &self.core.config,
                &self.core.paths,
                &self.core.config.active_persona_scope(),
            )
        };
    }

    pub fn replace_client(&mut self, client: OpenAiCompatibleClient) {
        self.client = client;
    }

    pub fn cloned_client(&self) -> OpenAiCompatibleClient {
        self.client.clone()
    }

    pub fn reload_config(
        &mut self,
        config: AppConfig,
        client: OpenAiCompatibleClient,
    ) -> Result<()> {
        self.core.config = config;
        self.client = client;
        self.core.tools_enabled = self.core.config.tools.enabled;
        self.core.max_tool_rounds = self.core.config.tools.max_rounds;
        self.core.trim_at_ratio = self.core.config.context.trim_at_ratio;
        self.core.compact_at_ratio = self.core.config.context.effective_compact_at_ratio();
        self.core.trim_batch_ratio = self.core.config.context.trim_batch_ratio;
        self.core.on_overflow = self.core.config.context.on_overflow.clone();
        self.core.subsystems = PersonaManifest::load(
            &self.core.config,
            &self.core.paths,
            &self.core.config.active_persona_scope(),
        )
        .enabled_subsystems(&self.core.config);
        let (access, writer_principal, writer_display_name) = self.memory.store.request_context();
        self.memory.store = MemoryStore::new(&self.core.config, &self.core.paths)
            .with_request_context(access, writer_principal, writer_display_name)
            .with_session_id(&self.memory.origin.session_id);
        // 与构造期同一判据:清单关着的人格重载后同样不建库。
        (self.memory.database_id, self.memory.generation) = if self.core.subsystems.memory {
            self.memory.store.init()?;
            self.memory.store.identity()?
        } else {
            (String::new(), 0)
        };
        self.refresh_preset_dialogs();
        self.prepare_for_turn()
    }

    /// 平台(QQ 等)回合的工具轮数上限(platforms.max_tool_rounds,默认 32,
    /// 0=不限):平台回合失控时没人守在终端里按停——真机 web_search 同
    /// query 222 连就是这么烧起来的。
    pub fn cap_tool_rounds_for_platform(&mut self) {
        let cap = self.core.config.platforms.max_tool_rounds;
        if cap > 0 {
            self.core.max_tool_rounds = cap;
        }
    }
}

/// 属主/成员档案进不进系统提示词:终端(Owner)与 WebUI(External 且没有平台
/// 上下文)进;QQ 等平台回合与内部回合不进。
pub(in crate::agent) fn user_profile_applies(
    audience: PromptAudience,
    platform_turn: bool,
) -> bool {
    !platform_turn && matches!(audience, PromptAudience::Owner | PromptAudience::External)
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;

/// 装上「本会话用量」那件工具。
///
/// 它要的数只有回合开始时才知道（会话是那时才定的，装配层只认人格与工具面），
/// 所以在这儿注册。判据只有一条：会话取值器给得出数就装——本会话的账不跟
/// 全局那件走（dev 的 `core_only` 没有全局那件，这条账照样要能问）。
fn bind_session_usage(
    tools: &mut ToolRegistry,
    state: &StateStore,
    client: &OpenAiCompatibleClient,
    config: &miyu_base::config::AppConfig,
    turn_usage: &TurnUsageMirror,
) {
    let state = state.clone();
    let window_config = config.clone();
    let window_client = client.clone();
    let live = turn_usage.clone();
    let session: crate::tools::usage_query::SessionUsageFn = Arc::new(move || {
        // 窗口跟着当前端点走，和 footer 上那个数同一个来源。
        let context_window = window_client.context_window(&window_config).ok().flatten();
        // 回合中途被调（它只会在回合中途被调）：库里的数都要等这一轮跑完才落账，
        // 照库读就少了这一轮——头一轮问，累计是 0（09-24）。和 footer 一个算法：
        // 已落账的 + 这一轮到目前为止的；上下文取这一轮最后一次请求的。
        let mut spent = state.session_cumulative_token_totals().unwrap_or_default();
        spent.add(live.get());
        Some(crate::tools::usage_query::SessionUsage {
            context_tokens: live
                .context()
                .or_else(|| state.latest_context_end_tokens().ok().flatten()),
            context_window,
            spent,
            turns: state
                .load_visible_turns()
                .map(|turns| turns.len())
                .unwrap_or(0),
        })
    });
    // 取值器给得出数就装，给不出来（平台工具集、还没绑会话）才不装。
    if session().is_some() {
        crate::tools::usage_query::register_session(tools, session);
    }
}
