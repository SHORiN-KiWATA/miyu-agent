//! 工具插件（网页、图像、记忆库、汇率……）的配置。
//!
//! 每个插件一个结构体加一份 `Default`，字段的默认值走 [`super::defaults`]。
//! 这些结构体和插件本身的代码是分开的：配置能被读写、迁移、在 TUI 里编辑，不
//! 需要把插件加载起来。

use crate::config::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    #[serde(default)]
    pub web: WebPluginConfig,
    #[serde(default)]
    pub web_images: WebImagesPluginConfig,
    #[serde(default)]
    pub vision: VisionPluginConfig,
    #[serde(default)]
    pub exchange_rate: ExchangeRatePluginConfig,
    #[serde(default)]
    pub image_generation: ImageGenerationPluginConfig,
    #[serde(default)]
    pub print_image: PrintImagePluginConfig,
    #[serde(default)]
    pub memes: MemesPluginConfig,
    #[serde(default)]
    pub knowledge_base: KnowledgeBasePluginConfig,
    #[serde(default = "default_archlinux_plugin")]
    pub archlinux: PluginEnabledConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub file_sharing: FileSharingPluginConfig,
    #[serde(default)]
    pub claude_code: ClaudeCodePluginConfig,
    #[serde(default)]
    pub antigravity: AntigravityPluginConfig,
    #[serde(default)]
    pub codex: CodexPluginConfig,
    #[serde(default)]
    pub codebuddy: CodeBuddyPluginConfig,
}

/// 本机 Claude Code CLI 接入:`claude-code` 供应商协议的运行参数。CLI 用
/// 用户既有的订阅登录态,Miyu 不经手任何凭据。(早期还有一件 `claude_code`
/// 委托工具共用这份配置,08-21 已删;它专用的 timeout/max_output 字段随之
/// 退役,存量配置里的同名键会被忽略。)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeCodePluginConfig {
    /// 空 = 从 PATH 解析 `claude`。
    #[serde(default)]
    pub binary: String,
    /// 原生工具开启时的 --permission-mode。无头模式没有交互审批,默认
    /// bypassPermissions 让 Bash 可用;改 acceptEdits 则只自动放行文件编辑、
    /// 命令被拒。
    #[serde(default = "default_claude_code_permission_mode")]
    pub permission_mode: String,
    /// 哪些模式的会话让 claude 用自带原生工具(Bash/Edit/Read…):
    /// off/dev/normal/all。原生工具在 claude 训练分布内,编码能力最强;
    /// 经桥的 Miyu 工具反正不走 Miyu 渲染管线,所以默认 all。
    #[serde(default = "default_claude_code_native_tools")]
    pub native_tools: String,
    /// 哪些模式的会话把 Miyu 工具经 MCP 桥挂给 claude(记忆/生图/表情包等
    /// claude 没有的能力):off/dev/normal/all,默认 all。两套同开时与原生
    /// 重复的 Miyu 工具被剔除,原生优先。
    #[serde(default = "default_claude_code_miyu_tools")]
    pub miyu_tools: String,
    /// 供应商中转模式的流空闲看门狗（秒）：这么久没有任何输出就杀进程。
    #[serde(default = "default_claude_code_idle_timeout_seconds")]
    pub idle_timeout_seconds: u64,
    /// 从子进程环境剥离 ANTHROPIC_API_KEY/ANTHROPIC_AUTH_TOKEN，强制走订阅
    /// 登录态而不是按量计费的 API key。
    #[serde(default = "default_true")]
    pub prefer_subscription: bool,
}

impl Default for ClaudeCodePluginConfig {
    fn default() -> Self {
        Self {
            binary: String::new(),
            permission_mode: default_claude_code_permission_mode(),
            native_tools: default_claude_code_native_tools(),
            miyu_tools: default_claude_code_miyu_tools(),
            idle_timeout_seconds: default_claude_code_idle_timeout_seconds(),
            prefer_subscription: true,
        }
    }
}

/// 本机 Antigravity CLI(`agy`)接入:`antigravity` 供应商协议的运行参数。
/// CLI 用用户既有的 Google 登录态,Miyu 不经手任何凭据。与 claude-code 的
/// 差异:人格经全局自定义代理文件(`~/.gemini/config/agents/miyu/agent.md`)
/// 替换默认提示词,Miyu 工具经全局 mcp_config.json 的 `miyu` 条目挂桥。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AntigravityPluginConfig {
    /// 空 = 从 PATH 解析 `agy`。
    #[serde(default)]
    pub binary: String,
    /// 哪些模式的会话让 agy 用自带原生工具(run_command/view_file/…):
    /// off/dev/normal/all。原生工具吃订阅额度,默认 all。
    #[serde(default = "default_antigravity_native_tools")]
    pub native_tools: String,
    /// 哪些模式的会话把 Miyu 工具经 MCP 桥挂给 agy:off/dev/normal/all。
    /// 两套同开时与原生重复的 Miyu 工具被剔除,原生优先。
    #[serde(default = "default_antigravity_miyu_tools")]
    pub miyu_tools: String,
    /// 桥上的 Miyu 工具按 eager 注册(以 `mcp_miyu_<name>` 原生名直接可调,
    /// schema 进系统提示词);关掉则走 agy 的懒加载(模型先读 schema 文件再经
    /// `call_mcp_tool` 调用,省 token 但每件工具多一跳)。
    #[serde(default = "default_true")]
    pub miyu_tools_eager: bool,
    /// 流空闲看门狗(秒):这么久没有任何输出就杀进程。
    #[serde(default = "default_antigravity_idle_timeout_seconds")]
    pub idle_timeout_seconds: u64,
    /// agy 自己的 `--print-timeout`(秒):整轮上限。默认 5 分钟不够跑长命令,
    /// 这里给 24 小时,真正的活性判定交给看门狗。
    #[serde(default = "default_antigravity_print_timeout_seconds")]
    pub print_timeout_seconds: u64,
    /// 同一条会话连续几轮复用一个 agy 进程(省掉每轮 5 秒左右的冷启动:登录 +
    /// 挂会话)。工具面/人格/模型一变就换新进程;关掉则每轮一个新进程。
    #[serde(default = "default_true")]
    pub reuse_process: bool,
    /// 常驻的 agy 进程闲置多久回收(秒)。
    #[serde(default = "default_antigravity_reuse_idle_seconds")]
    pub reuse_idle_seconds: u64,
}

impl Default for AntigravityPluginConfig {
    fn default() -> Self {
        Self {
            binary: String::new(),
            native_tools: default_antigravity_native_tools(),
            miyu_tools: default_antigravity_miyu_tools(),
            miyu_tools_eager: true,
            idle_timeout_seconds: default_antigravity_idle_timeout_seconds(),
            print_timeout_seconds: default_antigravity_print_timeout_seconds(),
            reuse_process: true,
            reuse_idle_seconds: default_antigravity_reuse_idle_seconds(),
        }
    }
}

/// 本机 OpenAI Codex CLI 接入:`codex` 供应商协议的运行参数。CLI 用用户既有
/// 的 ChatGPT 登录态,Miyu 不经手凭据。所有配置逐进程经 `-c` 注入,不碰用户
/// 的 ~/.codex/config.toml。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexPluginConfig {
    /// 空 = 从 PATH 解析 `codex`。
    #[serde(default)]
    pub binary: String,
    /// 哪些模式的会话让 codex 用自带原生工具(shell/apply_patch/web_search):
    /// off/dev/normal/all,默认 all。
    #[serde(default = "default_codex_native_tools")]
    pub native_tools: String,
    /// 哪些模式的会话把 Miyu 工具经 MCP 桥挂给 codex:off/dev/normal/all。
    #[serde(default = "default_codex_miyu_tools")]
    pub miyu_tools: String,
    /// codex 沙箱:danger-full-access(默认,与另两条线的全放行同义)/
    /// workspace-write / read-only。
    #[serde(default = "default_codex_sandbox_mode")]
    pub sandbox_mode: String,
    /// 不加载用户自己的 ~/.codex/config.toml(登录态照常):默认开,免得用户
    /// 的 MCP 服务器/规则混进中转工具面。
    #[serde(default = "default_true")]
    pub ignore_user_config: bool,
    /// 流空闲看门狗(秒)。
    #[serde(default = "default_codex_idle_timeout_seconds")]
    pub idle_timeout_seconds: u64,
}

impl Default for CodexPluginConfig {
    fn default() -> Self {
        Self {
            binary: String::new(),
            native_tools: default_codex_native_tools(),
            miyu_tools: default_codex_miyu_tools(),
            sandbox_mode: default_codex_sandbox_mode(),
            ignore_user_config: true,
            idle_timeout_seconds: default_codex_idle_timeout_seconds(),
        }
    }
}

/// 本机 CodeBuddy CLI(`codebuddy` / `cbc`)接入:`codebuddy` 供应商协议的运行
/// 参数。CLI 用用户既有的腾讯登录态(`apiKeySource: copilot.tencent.com`),
/// Miyu 不经手任何凭据。
///
/// CodeBuddy 是 Claude Code 的分叉:09-20 实测 stream-json 事件逐字段一致
/// (`system/init` → `stream_event`/`content_block_delta`/`thinking_delta` →
/// `result`),`--system-prompt` / `--tools ""` / `--strict-mcp-config` /
/// `--input-format stream-json` 全都认,所以事件解析整条复用 claude-code 的。
/// 差别只有两个 flag:它**没有** `--effort`(思考档)也**没有**
/// `--no-session-persistence`(一次性会话),拼参数时跳过。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBuddyPluginConfig {
    /// 空 = 从 PATH 解析 `codebuddy`。
    #[serde(default)]
    pub binary: String,
    /// 原生工具开启时的 --permission-mode。同 claude-code:无头模式没有交互
    /// 审批,默认 bypassPermissions 让 Bash 可用。
    #[serde(default = "default_codebuddy_permission_mode")]
    pub permission_mode: String,
    /// 哪些模式的会话让 codebuddy 用自带原生工具(Bash/Edit/Read…):
    /// off/dev/normal/all,默认 all。
    #[serde(default = "default_codebuddy_native_tools")]
    pub native_tools: String,
    /// 哪些模式的会话把 Miyu 工具经 MCP 桥挂给 codebuddy:off/dev/normal/all。
    #[serde(default = "default_codebuddy_miyu_tools")]
    pub miyu_tools: String,
    /// 流空闲看门狗(秒)。
    #[serde(default = "default_codebuddy_idle_timeout_seconds")]
    pub idle_timeout_seconds: u64,
}

impl Default for CodeBuddyPluginConfig {
    fn default() -> Self {
        Self {
            binary: String::new(),
            permission_mode: default_codebuddy_permission_mode(),
            native_tools: default_codebuddy_native_tools(),
            miyu_tools: default_codebuddy_miyu_tools(),
            idle_timeout_seconds: default_codebuddy_idle_timeout_seconds(),
        }
    }
}

/// WebUI 文件分享（`share_file` 工具与 `/api/shared` 路由）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSharingPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 单文件大小上限（字节）。0 = 不限制；快照复制前另做磁盘余量检查。
    #[serde(default)]
    pub max_shared_file_bytes: u64,
}

impl Default for FileSharingPluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_shared_file_bytes: 0,
        }
    }
}

/// 这台机器是不是 Arch 系。
///
/// `/etc/arch-release` 是 Arch 自己放的；EndeavourOS 一类衍生版也有。Manjaro
/// 没有那个文件但有 `pacman`，所以两条判据取并集。非 Linux 直接 false——macOS
/// 上再怎么找也不会有 pacman。
///
/// 只算一次:配置每次加载都要问它,而这事在进程生命周期内不会变。
pub(crate) fn arch_host() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return false;
        }
        if std::path::Path::new("/etc/arch-release").exists() {
            return true;
        }
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join("pacman").is_file()))
            .unwrap_or(false)
    })
}

/// Arch 那套工具的默认开关**跟着宿主走**。
///
/// 09-22 真机实测:macOS 上 `archlinux_news` / `archlinux_official_package_query`
/// / `archwiki_query` / `aur` / `crack_search` 五件全在工具清单里,而 `aur` 那件
/// 要 pacman / makepkg,在那台机器上必然失败。原来的判据是纯配置开关
/// (`PluginEnabledConfig` 默认 true),完全不看发行版——而 `installed` 这个字段
/// 的注释写的就是「机器级开关:本机装了 / 开了没有」。
///
/// 用户在「人格和功能」里主动勾上时写的是显式 `true`,读回来照样注册——默认关
/// 不等于不让开(用户 09-22 拍板)。
fn default_archlinux_plugin() -> PluginEnabledConfig {
    PluginEnabledConfig {
        enabled: arch_host(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEnabledConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub tavily_api_keys: Vec<String>,
    #[serde(default)]
    pub firecrawl_api_keys: Vec<String>,
    #[serde(default)]
    pub anysearch_api_keys: Vec<String>,
    /// Exa 无需 key 也可用（走官方 MCP 免费额度）；配置 key 后走 REST API
    #[serde(default)]
    pub exa_api_keys: Vec<String>,
    #[serde(default)]
    pub searxng_base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebImagesPluginConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_images_source_mode")]
    pub source_mode: String,
    #[serde(default = "default_web_images_max_results")]
    pub max_results: usize,
    #[serde(default = "default_web_images_max_download_mb")]
    pub max_download_mb: f64,
    #[serde(default = "default_true")]
    pub safe_search: bool,
    // 09-23 撤掉的 `vision_screening_enabled`(下载后视觉审核)不留字段:老配置
    // 里写着也照常解析(serde 默认忽略未知键),下次保存时自然消失。
    #[serde(default = "default_web_images_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub prefer_current_multimodal_model: bool,
    #[serde(default)]
    pub vision_provider_id: String,
    #[serde(default)]
    pub vision_model: String,
    /// 视频分析的显式路由(08-22)。留空=自动挑 models.dev 标了 video 输入
    /// 能力的启用模型;两项须同时配置才生效。
    #[serde(default)]
    pub video_provider_id: String,
    #[serde(default)]
    pub video_model: String,
    #[serde(default = "default_vision_response_header_timeout")]
    pub response_header_timeout_seconds: u64,
    #[serde(default = "default_vision_stream_idle_timeout")]
    pub stream_idle_timeout_seconds: u64,
    #[serde(default = "default_vision_image_timeout")]
    pub image_timeout_seconds: u64,
    #[serde(default = "default_true")]
    pub preview_with_chafa: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeRatePluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_true")]
    pub free_fallback_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_image_generation_provider_type")]
    pub provider_type: String,
    #[serde(default = "default_openai_images_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default = "default_image_generation_model")]
    pub model: String,
    #[serde(default = "default_image_generation_aspect_ratio")]
    pub default_aspect_ratio: String,
    #[serde(default = "default_image_generation_resolution")]
    pub default_resolution: String,
    #[serde(default = "default_image_generation_output_dir")]
    pub output_dir: String,
    #[serde(default)]
    pub auto_print: bool,
    #[serde(default = "default_image_generation_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintImagePluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_print_image_width_percent")]
    pub width_percent: u8,
    #[serde(default = "default_print_image_height_percent")]
    pub height_percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemesPluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub persona_libraries: HashMap<String, String>,
    #[serde(default = "default_memes_width_percent")]
    pub width_percent: u8,
    #[serde(default = "default_memes_height_percent")]
    pub height_percent: u8,
    #[serde(default = "default_memes_max_image_mb")]
    pub max_image_mb: u64,
    #[serde(default = "default_memes_search_max_results")]
    pub search_max_results: usize,
    #[serde(default)]
    pub allow_gif_animation: bool,
    /// 终端/WebUI 会话的自动提示发送表情,默认开。
    #[serde(default = "default_true")]
    pub auto_send_enabled: bool,
    /// 通讯平台会话的自动提示发送表情:与终端/WebUI 的 auto_send_enabled
    /// 独立,默认开——表情包本来就是平台聊天的语言。
    #[serde(default = "default_true")]
    pub auto_send_platform_enabled: bool,
    #[serde(default = "default_memes_auto_send_probability")]
    pub auto_send_probability: f32,
}

/// 手动模型价格(每 1M tokens):目录查不到价的中转/赠送端点用它,
/// 设了就覆盖 models.dev 的价目。缓存价缺省时按输入价计。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelCostConfig {
    #[serde(default)]
    pub currency: CostCurrency,
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, alias = "cache", skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
}

/// 手动价格的币种。统计聚合统一折算成 USD 展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CostCurrency {
    #[default]
    #[serde(rename = "USD", alias = "usd")]
    Usd,
    #[serde(rename = "CNY", alias = "cny", alias = "rmb", alias = "¥")]
    Cny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBasePluginConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub data_dir: String,
    #[serde(default = "default_kb_max_search_results")]
    pub max_search_results: usize,
    #[serde(default = "default_kb_snippet_context_chars")]
    pub snippet_context_chars: usize,
    #[serde(default = "default_kb_proximity_window_chars")]
    pub proximity_window_chars: usize,
    #[serde(default = "default_kb_max_read_lines")]
    pub max_read_lines: usize,
    #[serde(default = "default_kb_max_file_size_kb")]
    pub max_file_size_kb: usize,
    #[serde(default = "default_kb_allowed_extensions")]
    pub allowed_extensions: String,
    #[serde(default = "default_kb_allowed_filenames")]
    pub allowed_filenames: String,
    #[serde(default = "default_true")]
    pub upload_tool_enabled: bool,
    #[serde(default = "default_true")]
    pub embedding_enabled: bool,
    #[serde(default)]
    pub embedding_provider_id: String,
    #[serde(default)]
    pub embedding_model: String,
    #[serde(default = "default_kb_semantic_chunk_chars")]
    pub semantic_chunk_chars: usize,
    #[serde(default = "default_kb_semantic_chunk_overlap")]
    pub semantic_chunk_overlap: usize,
    #[serde(default = "default_kb_semantic_top_k")]
    pub semantic_top_k: usize,
    #[serde(default = "default_kb_semantic_min_score")]
    pub semantic_min_score: f32,
    #[serde(default = "default_kb_keyword_strong_score_threshold")]
    pub keyword_strong_score_threshold: f32,
    #[serde(default = "default_kb_embedding_timeout_seconds")]
    pub embedding_timeout_seconds: u64,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            file_sharing: FileSharingPluginConfig::default(),
            web: WebPluginConfig::default(),
            web_images: WebImagesPluginConfig::default(),
            vision: VisionPluginConfig::default(),
            exchange_rate: ExchangeRatePluginConfig::default(),
            image_generation: ImageGenerationPluginConfig::default(),
            print_image: PrintImagePluginConfig::default(),
            memes: MemesPluginConfig::default(),
            knowledge_base: KnowledgeBasePluginConfig::default(),
            archlinux: default_archlinux_plugin(),
            memory: MemoryConfig::default(),
            claude_code: ClaudeCodePluginConfig::default(),
            antigravity: AntigravityPluginConfig::default(),
            codex: CodexPluginConfig::default(),
            codebuddy: CodeBuddyPluginConfig::default(),
        }
    }
}

impl Default for PluginEnabledConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
        }
    }
}

impl Default for WebPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            max_results: default_web_search_max_results(),
            tavily_api_keys: Vec::new(),
            firecrawl_api_keys: Vec::new(),
            anysearch_api_keys: Vec::new(),
            exa_api_keys: Vec::new(),
            searxng_base_url: String::new(),
        }
    }
}

impl Default for WebImagesPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            source_mode: default_web_images_source_mode(),
            max_results: default_web_images_max_results(),
            max_download_mb: default_web_images_max_download_mb(),
            safe_search: default_true(),
            timeout_seconds: default_web_images_timeout(),
        }
    }
}

impl Default for VisionPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            prefer_current_multimodal_model: default_true(),
            vision_provider_id: String::new(),
            vision_model: String::new(),
            video_provider_id: String::new(),
            video_model: String::new(),
            response_header_timeout_seconds: default_vision_response_header_timeout(),
            stream_idle_timeout_seconds: default_vision_stream_idle_timeout(),
            image_timeout_seconds: default_vision_image_timeout(),
            preview_with_chafa: default_true(),
        }
    }
}

impl Default for ExchangeRatePluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: String::new(),
            free_fallback_enabled: default_true(),
        }
    }
}

impl Default for ImageGenerationPluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider_type: default_image_generation_provider_type(),
            base_url: default_openai_images_base_url(),
            api_keys: Vec::new(),
            model: default_image_generation_model(),
            default_aspect_ratio: default_image_generation_aspect_ratio(),
            default_resolution: default_image_generation_resolution(),
            output_dir: default_image_generation_output_dir(),
            auto_print: default_true(),
            timeout_seconds: default_image_generation_timeout(),
        }
    }
}

impl Default for PrintImagePluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            width_percent: default_print_image_width_percent(),
            height_percent: default_print_image_height_percent(),
        }
    }
}

impl Default for MemesPluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            persona_libraries: HashMap::new(),
            width_percent: default_memes_width_percent(),
            height_percent: default_memes_height_percent(),
            max_image_mb: default_memes_max_image_mb(),
            search_max_results: default_memes_search_max_results(),
            allow_gif_animation: false,
            auto_send_enabled: true,
            auto_send_platform_enabled: true,
            auto_send_probability: default_memes_auto_send_probability(),
        }
    }
}

impl MemesPluginConfig {
    pub fn library_for_persona(&self, persona: &str) -> String {
        if persona.trim().is_empty() {
            return self
                .persona_libraries
                .get("default")
                .cloned()
                .unwrap_or_else(|| "miyu".to_string());
        }
        let persona = persona_scope_name(persona);
        self.persona_libraries
            .get(&persona)
            .cloned()
            .unwrap_or(persona)
    }
}

impl Default for KnowledgeBasePluginConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            data_dir: String::new(),
            max_search_results: default_kb_max_search_results(),
            snippet_context_chars: default_kb_snippet_context_chars(),
            proximity_window_chars: default_kb_proximity_window_chars(),
            max_read_lines: default_kb_max_read_lines(),
            max_file_size_kb: default_kb_max_file_size_kb(),
            allowed_extensions: default_kb_allowed_extensions(),
            allowed_filenames: default_kb_allowed_filenames(),
            upload_tool_enabled: default_true(),
            embedding_enabled: true,
            embedding_provider_id: String::new(),
            embedding_model: String::new(),
            semantic_chunk_chars: default_kb_semantic_chunk_chars(),
            semantic_chunk_overlap: default_kb_semantic_chunk_overlap(),
            semantic_top_k: default_kb_semantic_top_k(),
            semantic_min_score: default_kb_semantic_min_score(),
            keyword_strong_score_threshold: default_kb_keyword_strong_score_threshold(),
            embedding_timeout_seconds: default_kb_embedding_timeout_seconds(),
        }
    }
}

/// Returns the old absolute directory when the value was rewritten, so the
/// caller can carry any files across; `None` when nothing matched.
pub(crate) fn remap_managed_output_dir(
    value: &mut String,
    legacy_roots: &[PathBuf],
    destination_root: &Path,
    home: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let trimmed = value.trim();
    let expanded = trimmed
        .strip_prefix("~/")
        .map(|relative| home.join(relative))
        .unwrap_or_else(|| PathBuf::from(trimmed));
    for legacy_root in legacy_roots {
        let Ok(relative) = expanded.strip_prefix(legacy_root) else {
            continue;
        };
        let destination = destination_root.join(relative);
        *value = destination.display().to_string();
        return Some((expanded, destination));
    }
    None
}

/// Carries files left behind at a remapped output directory over to the new
/// one. Best effort: a file that cannot be moved is left where it is rather
/// than failing a config load over it.
pub(crate) fn relocate_managed_output(from: &Path, to: &Path) {
    if from == to || !from.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    let mut moved = 0usize;
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        if target.exists() {
            continue;
        }
        if std::fs::create_dir_all(to).is_err() {
            return;
        }
        if std::fs::rename(entry.path(), &target).is_ok() {
            moved += 1;
        }
    }
    if moved > 0 {
        // Only prunes when it empties out; anything left is someone else's.
        let _ = std::fs::remove_dir(from);
        tracing::info!(
            from = %from.display(),
            to = %to.display(),
            moved,
            "{}",
            crate::i18n::text(
                "moved files from a stale managed output directory",
                "已把过时输出目录里的文件搬到新位置",
            )
        );
    }
}

#[cfg(test)]
mod arch_plugin_tests {
    use super::*;

    /// 没写过这一项时跟着宿主走：Arch 上开，别处关。
    #[test]
    fn an_absent_key_follows_the_host() {
        let plugins: PluginsConfig = serde_json::from_str("{}").expect("empty object parses");
        assert_eq!(plugins.archlinux.enabled, arch_host());
    }

    /// **用户主动勾上就得算数**，哪怕这台机器不是 Arch（用户 09-22 拍板：
    /// 「除非用户自己在『人格和功能』菜单里主动开了」）。
    #[test]
    fn an_explicit_opt_in_wins_over_the_host_default() {
        let plugins: PluginsConfig =
            serde_json::from_str(r#"{"archlinux":{"enabled":true}}"#).expect("parses");
        assert!(plugins.archlinux.enabled);
    }

    /// 反过来也要算数：Arch 上主动关掉的不能被默认值顶回来。
    #[test]
    fn an_explicit_opt_out_is_kept() {
        let plugins: PluginsConfig =
            serde_json::from_str(r#"{"archlinux":{"enabled":false}}"#).expect("parses");
        assert!(!plugins.archlinux.enabled);
    }

    /// 非 Linux 上不去翻 PATH 找 pacman——那儿根本不会有。
    #[test]
    fn a_non_linux_host_is_never_arch() {
        if !cfg!(target_os = "linux") {
            assert!(!arch_host());
        }
    }
}
