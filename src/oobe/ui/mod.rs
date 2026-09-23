//! 引导向导的状态机。画面在 [`draw`]，内容在 [`build`]，按键在 [`keys`]。
//!
//! 四条要紧的设计（沿用 `testkit/oobe-demo`）：
//!
//! - **色彩与字符集全程降级**。见 `terminal::palette`：真彩 / 256 / 16 / 不上色。
//! - **编辑态是显式的**。光标停在输入框上不等于在打字，要 `⏎` 才进去。
//!   进不去就不打字，`jk` 就永远能导航。
//! - **光标是真终端光标**。输入法候选框靠终端上报的位置定位，画假光标它只能
//!   丢在屏幕角落。
//! - **进出编辑态要管输入法**。出来时关掉（否则 `jk` 被吃），进去时恢复上次状态。

pub(super) mod build;
pub(super) mod draw;
pub(super) mod keys;
pub(super) mod widgets;

use super::apply;
use super::probe::{Facts, Fcitx, Ime, Loader};
use super::providers::{CatalogJob, Prefetch, ProviderOption};
use miyu_base::config::feature_catalog::{self, FeatureItem, FeatureKind};
use miyu_base::config::{AppConfig, ProviderConfig};
use miyu_base::paths::MiyuPaths;
use miyu_base::terminal::palette::{Depth, Theme};
use miyu_base::terminal::starfield::BannerArt;
use widgets::Cx;

pub(super) const STEPS: [&str; 6] = ["人格", "功能", "认识你", "终端", "沙盒", "模型"];

// ── 开场时间轴（30ms 一帧）──
/// 纯星空，先让它闪一会儿。
pub(super) const FORM_AT: usize = 40;
/// 字母凝聚完、副标题出来。
pub(super) const SUBTITLE_AT: usize = 132;
/// 扫光起点。
pub(super) const GLINT_AT: usize = 130;
/// 提示淡入。
pub(super) const HINT_AT: usize = 150;
pub(super) const INTRO_END: usize = 240;

/// 引导怎么收场。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Outcome {
    /// 六屏走完，供应商已写好。
    Completed,
    /// Ctrl+S 跳过：只置完成标志，别的一个字不写。
    Skipped,
    /// 用户选了「进入设置界面」：调用方去开完整设置。
    OpenSettings,
    /// 开场就退出（Esc / Ctrl+C）：什么都不写，下次还会再来。
    Aborted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Screen {
    Welcome,
    Persona,
    Features,
    Identity,
    ShellHook,
    /// 默认以沙盒运行吗(09-23)。
    Sandbox,
    Provider,
}

impl Screen {
    pub fn step(self) -> Option<usize> {
        match self {
            Screen::Persona => Some(0),
            Screen::Features => Some(1),
            Screen::Identity => Some(2),
            Screen::ShellHook => Some(3),
            Screen::Sandbox => Some(4),
            Screen::Provider => Some(5),
            Screen::Welcome => None,
        }
    }

    pub fn prev(self) -> Screen {
        match self {
            Screen::Welcome | Screen::Persona => Screen::Welcome,
            Screen::Features => Screen::Persona,
            Screen::Identity => Screen::Features,
            Screen::ShellHook => Screen::Identity,
            Screen::Sandbox => Screen::ShellHook,
            Screen::Provider => Screen::Sandbox,
        }
    }
}

/// 接模型那一屏的子状态。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Prov {
    Pick,
    Form,
    /// 自己填端点。单独一屏——它是个「进去填」的动作，不是供应商列表里的一行，
    /// 内联展开会让那一行既像选项又像输入框。
    CustomEp,
    Fetching,
    PickModel,
}

/// 拉目录之前人在哪一屏:Esc 要退回去的地方。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Origin {
    Pick,
    Form,
    Custom,
}

pub(super) mod pfocus {
    pub const BUILTIN: usize = 0;
    pub const CUSTOM: usize = 1;
    pub const NAME: usize = 2;
    pub const SETTING: usize = 3;
    pub const GO: usize = 4;
}

pub(super) struct App {
    pub theme: Theme,
    pub art: BannerArt,
    pub config: AppConfig,
    pub paths: MiyuPaths,
    pub loader: Loader,
    pub facts: Facts,
    pub fcitx: Fcitx,
    pub prefetch: Prefetch,
    pub screen: Screen,
    pub outcome: Option<Outcome>,

    pub editing: bool,
    pub ime_name: Option<Ime>,
    pub ime_setting: Option<Ime>,
    pub ime_identity: Option<Ime>,
    pub ime_key: Option<Ime>,

    // 人格
    pub persona_custom: bool,
    pub focus: usize,
    pub name: String,
    pub setting: String,
    /// 人格屏保存后的 scope；功能屏按它读写 persona.toml。
    pub persona_scope: String,

    // 功能
    pub feats: Vec<FeatureItem>,
    pub feat_cur: usize,
    pub feats_for: Option<String>,

    // 认识你
    pub identity: String,

    // 终端
    pub shell_cur: usize,
    /// 沙盒屏的光标:0 = 开启,1 = 不开。
    pub sandbox_cur: usize,
    /// (shell 名, 已装)。
    pub shells: Vec<(&'static str, bool)>,

    // 模型
    pub prov: Prov,
    pub prov_cur: usize,
    pub options: Vec<ProviderOption>,
    /// 选中的预设,等着填 key。
    pub preset: Option<ProviderConfig>,
    /// 预设支持公共密钥时(opencode Zen):用免费额度还是自己的 key。
    pub public_quota: bool,
    /// 拉目录是从哪屏发起的。
    pub origin: Origin,
    pub ep_name: String,
    pub ep_id: String,
    pub ep_url: String,
    pub ep_proto: usize,
    pub api_key: String,
    pub form_focus: usize,
    pub fetch: Option<CatalogJob>,
    /// 正在拉目录 / 等着选模型的那条供应商。
    pub pending: Option<ProviderConfig>,
    pub models: Vec<String>,
    /// 当前高亮在**筛过的**列表里的位置（见 [`App::visible_models`]）。
    pub model_cur: usize,
    /// 模型列表的 vim 式搜索：`/` 开始打字，Enter 收起输入框留着筛选，Esc 清掉。
    pub model_query: String,
    pub model_search: bool,

    /// 一句红字提示（保存失败、拉目录失败）。换屏即清。
    pub notice: Option<String>,

    pub scroll: usize,
    pub tick: usize,
    pub intro: usize,
    /// 换屏后过了几帧。内容按这个逐行落下。
    pub fade: usize,
    /// 本帧光标该摆哪。**不交给 ratatui**——见主循环里的说明。
    pub caret_screen: Option<(u16, u16)>,
}

impl App {
    pub fn new(config: AppConfig, paths: MiyuPaths) -> Self {
        let fcitx = Fcitx::probe();
        let theme = Theme::detect();
        let depth_label = match theme.depth {
            Depth::True => "真彩",
            Depth::X256 => "256 色",
            Depth::Ansi16 => "16 色",
            Depth::Mono => "无色",
        };
        // 已经有人格/自述的机器（跑过一半、或手动 `miyu oobe`）：预填，别让人重敲。
        let existing = apply::current_persona(&config, &paths);
        let identity = apply::current_identity(&config, &paths);
        let (persona_custom, name, setting) = match existing {
            Some((name, prompt)) => (true, name, prompt),
            None => (false, String::new(), String::new()),
        };
        Self {
            theme,
            art: BannerArt::builtin(theme.ascii),
            config,
            paths,
            loader: Loader::new(depth_label),
            facts: Facts::default(),
            fcitx,
            prefetch: Prefetch::default(),
            screen: Screen::Welcome,
            outcome: None,
            editing: false,
            ime_name: None,
            ime_setting: None,
            ime_identity: None,
            ime_key: Some(Ime::Off),
            persona_custom,
            focus: if persona_custom {
                pfocus::CUSTOM
            } else {
                pfocus::BUILTIN
            },
            name,
            setting,
            persona_scope: String::new(),
            feats: Vec::new(),
            feat_cur: 0,
            feats_for: None,
            identity,
            shell_cur: 0,
            sandbox_cur: 0,
            shells: Vec::new(),
            prov: Prov::Pick,
            prov_cur: 0,
            options: Vec::new(),
            preset: None,
            public_quota: true,
            origin: Origin::Pick,
            ep_name: String::new(),
            ep_id: String::new(),
            ep_url: String::new(),
            ep_proto: 0,
            api_key: String::new(),
            form_focus: 0,
            fetch: None,
            pending: None,
            models: Vec::new(),
            model_cur: 0,
            model_query: String::new(),
            model_search: false,
            notice: None,
            scroll: 0,
            tick: 0,
            intro: 0,
            fade: 99,
            caret_screen: None,
        }
    }

    /// 填 key 屏:支持公共密钥的预设第 0 行是开关,key 字段与「获取模型列表」各往下挪一行。
    pub fn form_has_public(&self) -> bool {
        self.preset
            .as_ref()
            .is_some_and(super::providers::public_quota_available)
    }

    pub fn form_key_index(&self) -> usize {
        usize::from(self.form_has_public())
    }

    pub fn form_action_index(&self) -> usize {
        self.form_key_index() + 1
    }

    pub fn cx(&self) -> Cx {
        Cx { theme: self.theme }
    }

    pub fn ai_name(&self) -> String {
        if self.persona_custom && !self.name.trim().is_empty() {
            self.name.trim().to_string()
        } else {
            "Miyu".into()
        }
    }

    pub fn on_count(&self) -> usize {
        self.feats.iter().filter(|item| item.on).count()
    }

    /// 探测跑完之后要做的事：提前拉 CLI 目录、列 shell。
    pub fn facts_ready(&mut self) {
        if !self.loader.done() || !self.facts.clis.is_empty() || !self.shells.is_empty() {
            return;
        }
        self.facts = self.loader.facts.clone();
        self.shells = self
            .facts
            .shells
            .iter()
            .map(|shell| (*shell, apply::hook_installed(&self.paths, shell)))
            .collect();
        // 已经装过的 shell 排前面并默认停在「保持」上——不重复写。
        for option in super::providers::options(&self.config, |bin| self.facts.has(bin)) {
            match option.choice {
                super::providers::Choice::Cli(provider) => {
                    // claude 的目录是预置表，不用拉。
                    if !provider.is_claude_code() {
                        self.prefetch.start(&self.config, &provider);
                    }
                }
                // 免 key 的预设(opencode Zen)要联网,也提前拉。
                super::providers::Choice::Preset { provider, .. }
                    if super::providers::public_quota_available(&provider) =>
                {
                    self.prefetch.start(&self.config, &provider);
                }
                _ => {}
            }
        }
    }

    pub fn skip_intro(&mut self) {
        self.intro = INTRO_END;
        while self.loader.advance() {}
        self.facts_ready();
    }

    /// 功能表按当前人格摆一遍（人格屏保存后 scope 才定）。
    pub fn load_features(&mut self) {
        if self.feats_for.as_deref() == Some(self.persona_scope.as_str()) {
            return;
        }
        let manifest = apply::current_manifest(&self.config, &self.paths);
        let default_persona = miyu_core::skills::is_default_persona(&self.config);
        let sources = crate::feature_sources::collect(&self.config, &self.paths);
        self.feats = feature_catalog::catalog(
            &manifest,
            &sources,
            default_persona,
            feature_catalog::CatalogScope::Onboarding,
            Some(&self.config),
        );
        self.feat_cur = 0;
        self.feats_for = Some(self.persona_scope.clone());
    }

    pub fn section_of(kind: FeatureKind) -> &'static str {
        match kind {
            // 机器级那一档引导里不摆（`CatalogScope::Onboarding`），真到了这儿
            // 也归「内置功能」。
            FeatureKind::Machine | FeatureKind::Subsystem | FeatureKind::Plugin => "内置功能",
            FeatureKind::Script => "脚本",
            FeatureKind::Skill => "技能",
            FeatureKind::Mcp => "MCP 服务器",
        }
    }

    pub fn field_here(&self) -> bool {
        match self.screen {
            Screen::Persona => matches!(self.focus, pfocus::NAME | pfocus::SETTING),
            Screen::Identity => self.focus == 0,
            Screen::Provider if self.prov == Prov::Form => self.form_focus == self.form_key_index(),
            Screen::Provider if self.prov == Prov::CustomEp => self.focus < 4,
            _ => false,
        }
    }

    pub fn buf(&mut self) -> Option<&mut String> {
        match self.screen {
            Screen::Persona if self.focus == pfocus::NAME => Some(&mut self.name),
            Screen::Persona if self.focus == pfocus::SETTING => Some(&mut self.setting),
            Screen::Identity => Some(&mut self.identity),
            Screen::Provider if self.prov == Prov::Form => Some(&mut self.api_key),
            Screen::Provider if self.prov == Prov::CustomEp => match self.focus {
                0 => Some(&mut self.ep_name),
                1 => Some(&mut self.ep_id),
                2 => Some(&mut self.ep_url),
                _ => Some(&mut self.api_key),
            },
            _ => None,
        }
    }

    fn ime_slot(&mut self) -> &mut Option<Ime> {
        match self.screen {
            Screen::Persona if self.focus == pfocus::NAME => &mut self.ime_name,
            Screen::Persona => &mut self.ime_setting,
            Screen::Identity => &mut self.ime_identity,
            _ => &mut self.ime_key,
        }
    }

    pub fn enter_edit(&mut self) {
        self.editing = true;
        if let Some(state) = *self.ime_slot() {
            self.fcitx.set(state);
        }
    }

    pub fn leave_edit(&mut self) {
        if let Some(now) = self.fcitx.state() {
            *self.ime_slot() = Some(now);
        }
        self.fcitx.set(Ime::Off);
        self.editing = false;
    }

    pub fn goto(&mut self, screen: Screen) {
        if self.editing {
            self.leave_edit();
        }
        self.screen = screen;
        self.scroll = 0;
        self.fade = 0;
        self.notice = None;
        self.focus = match screen {
            Screen::Persona if self.persona_custom => pfocus::CUSTOM,
            _ => 0,
        };
        if screen == Screen::Features {
            self.load_features();
        }
        if screen == Screen::ShellHook {
            // 已装过的 shell：光标停在它上面，回车 = 保持。
            self.shell_cur = self
                .shells
                .iter()
                .position(|(_, installed)| *installed)
                .unwrap_or(0);
        }
        if screen == Screen::Sandbox {
            // 光标停在配置里现在的值上:新装是开着的,回来改的看到的是上次选的。
            self.sandbox_cur = usize::from(!self.config.tools.sandbox.default_enabled);
        }
        if screen == Screen::Provider {
            self.prov = Prov::Pick;
            self.options = super::providers::options(&self.config, |bin| self.facts.has(bin));
            self.prov_cur = self.prov_cur.min(self.options.len().saturating_sub(1));
        }
    }

    /// 人格屏 → 功能屏：先落盘。
    pub fn commit_persona(&mut self) -> bool {
        let pick = if self.persona_custom {
            apply::PersonaPick::Custom {
                name: self.name.clone(),
                prompt: self.setting.clone(),
            }
        } else {
            apply::PersonaPick::Builtin
        };
        match apply::save_persona(&mut self.config, &self.paths, &pick) {
            Ok(scope) => {
                self.persona_scope = scope;
                true
            }
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                false
            }
        }
    }

    pub fn commit_features(&mut self) -> bool {
        let default_persona = miyu_core::skills::is_default_persona(&self.config);
        // 技能带路的脚本没有自己的行,白名单要按带路技能连动——重扫一次,
        // 与摆表用的是同一份外装件清单。
        let sources = crate::feature_sources::collect(&self.config, &self.paths);
        match apply::save_features(
            &mut self.config,
            &self.paths,
            &self.persona_scope,
            &self.feats,
            &sources,
            default_persona,
        ) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                false
            }
        }
    }

    pub fn commit_identity(&mut self) -> bool {
        match apply::save_identity(&self.config, &self.paths, &self.identity) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                false
            }
        }
    }

    /// 终端屏：选了没装过的 shell 才写。返回 `Ok(true)` 表示往终端打过字（要整屏重画）。
    pub fn commit_shell(&mut self) -> Result<bool, String> {
        let Some((shell, installed)) = self.shells.get(self.shell_cur).copied() else {
            return Ok(false);
        };
        if installed {
            return Ok(false);
        }
        apply::install_hook(&self.paths, shell).map_err(|error| format!("{error:#}"))?;
        if let Some(entry) = self.shells.get_mut(self.shell_cur) {
            entry.1 = true;
        }
        Ok(true)
    }

    /// 沙盒屏：选了就写（开/不开都写，回来改也是这一处）。
    pub fn commit_sandbox(&mut self) -> bool {
        match apply::save_sandbox_default(&mut self.config, &self.paths, self.sandbox_cur == 0) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(format!("{error:#}"));
                false
            }
        }
    }

    /// 选定模型：写配置、收场。
    pub fn commit_provider(&mut self, model: &str) {
        let Some(provider) = self.pending.take() else {
            return;
        };
        match apply::save_provider(&mut self.config, &self.paths, provider, model) {
            Ok(()) => self.finish(Outcome::Completed),
            Err(error) => self.notice = Some(format!("{error:#}")),
        }
    }

    pub fn finish(&mut self, outcome: Outcome) {
        if matches!(outcome, Outcome::Completed | Outcome::Skipped) {
            if let Err(error) = apply::mark_done(&mut self.config, &self.paths) {
                self.notice = Some(format!("{error:#}"));
                return;
            }
        }
        self.outcome = Some(outcome);
    }

    /// 开始拉某条供应商的目录（先看预取结果，没有再现拉）。
    pub fn start_fetch(&mut self, provider: ProviderConfig, origin: Origin) {
        self.origin = origin;
        self.prefetch.pump();
        self.pending = Some(provider.clone());
        self.models.clear();
        self.model_cur = 0;
        self.model_query.clear();
        self.model_search = false;
        if provider.is_claude_code() {
            self.models = provider
                .preset_model_catalog()
                .iter()
                .map(|name| name.to_string())
                .collect();
            self.pick_default_model(&provider);
            self.prov = Prov::PickModel;
            self.fade = 0;
            return;
        }
        if let Some(result) = self.prefetch.take(&provider.id) {
            self.finish_fetch(result);
            return;
        }
        if !self.prefetch.running(&provider.id) {
            self.fetch = Some(CatalogJob::spawn(&self.config, provider));
        }
        self.prov = Prov::Fetching;
        self.fade = 0;
    }

    /// 每帧问一次目录拉完没有。
    pub fn poll_fetch(&mut self) {
        if self.prov != Prov::Fetching {
            return;
        }
        self.prefetch.pump();
        let id = self.pending.as_ref().map(|provider| provider.id.clone());
        if let Some(result) = id.as_deref().and_then(|id| self.prefetch.take(id)) {
            self.finish_fetch(result);
            return;
        }
        if let Some(result) = self.fetch.as_ref().and_then(CatalogJob::poll) {
            self.fetch = None;
            self.finish_fetch(result);
        }
    }

    fn finish_fetch(&mut self, result: Result<Vec<String>, String>) {
        match result {
            Ok(models) if !models.is_empty() => {
                self.models = models;
                if let Some(provider) = self.pending.clone() {
                    self.pick_default_model(&provider);
                }
                self.prov = Prov::PickModel;
                self.fade = 0;
            }
            Ok(_) => {
                self.notice = Some("这个供应商没有返回任何模型".into());
                self.back_from_fetch();
            }
            Err(error) => {
                self.notice = Some(format!("拉模型列表失败: {error}"));
                self.back_from_fetch();
            }
        }
    }

    fn back_from_fetch(&mut self) {
        self.pending = None;
        self.prov = match self.origin {
            Origin::Form => Prov::Form,
            Origin::Custom => Prov::CustomEp,
            Origin::Pick => Prov::Pick,
        };
        self.fade = 0;
    }

    /// 搜索筛过的模型下标（空查询 = 全部）。大小写不敏感的子串匹配。
    pub fn visible_models(&self) -> Vec<usize> {
        filter_models(&self.models, &self.model_query)
    }

    /// 高亮着的那个模型名。
    pub fn selected_model(&self) -> Option<String> {
        let visible = self.visible_models();
        visible
            .get(self.model_cur)
            .and_then(|&index| self.models.get(index).cloned())
    }

    /// 筛选变短之后光标别掉到列表外面。
    pub fn clamp_model_cursor(&mut self) {
        self.model_cur = self
            .model_cur
            .min(self.visible_models().len().saturating_sub(1));
    }

    fn pick_default_model(&mut self, provider: &ProviderConfig) {
        let preferred = provider.default_model.trim();
        self.model_cur = self
            .models
            .iter()
            .position(|model| model == preferred)
            .unwrap_or(0);
    }
}

/// 模型搜索：大小写不敏感的子串匹配，空查询 = 全部。返回原列表里的下标。
pub(super) fn filter_models(models: &[String], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    models
        .iter()
        .enumerate()
        .filter(|(_, model)| query.is_empty() || model.to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod feature_tests {
    use super::*;
    use miyu_base::config::McpServerConfig;

    /// 功能屏按真实配置摆表:机器级 MCP 开着就列出开着的服务器,关着的服务器与
    /// 机器级关着时整格都不出现。
    #[test]
    fn features_screen_lists_enabled_mcp_servers_only_when_mcp_is_on() {
        std::env::set_var("MIYU_OOBE_NO_IME", "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish/miyu.fish"),
            bash_hook_file: root.join("shell/bash-hook.sh"),
            zsh_hook_file: root.join("shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: std::path::PathBuf::new(),
        };
        let mut config = AppConfig::default();
        config.mcp.enabled = true;
        config.mcp.servers = vec![
            McpServerConfig {
                id: "named".into(),
                display_name: "有名字".into(),
                command: "echo".into(),
                args: vec!["a".into()],
                env: Default::default(),
                timeout_seconds: 30,
                enabled: true,
                capabilities: Vec::new(),
            },
            McpServerConfig {
                id: "bare".into(),
                display_name: String::new(),
                command: "echo".into(),
                args: Vec::new(),
                env: Default::default(),
                timeout_seconds: 30,
                enabled: true,
                capabilities: Vec::new(),
            },
            McpServerConfig {
                id: "off".into(),
                display_name: "关着".into(),
                command: "echo".into(),
                args: Vec::new(),
                env: Default::default(),
                timeout_seconds: 30,
                enabled: false,
                capabilities: Vec::new(),
            },
        ];
        let mut app = App::new(config.clone(), paths.clone());
        app.persona_scope = config.active_persona_scope();
        app.load_features();
        let mcp: Vec<(&str, &str, bool)> = app
            .feats
            .iter()
            .filter(|item| item.kind == FeatureKind::Mcp)
            .map(|item| (item.id.as_str(), item.name.as_str(), item.on))
            .collect();
        assert_eq!(mcp, [("named", "有名字", true), ("bare", "bare", true)]);
        assert_eq!(App::section_of(FeatureKind::Mcp), "MCP 服务器");

        let mut off = config;
        off.mcp.enabled = false;
        let mut app = App::new(off.clone(), paths);
        app.persona_scope = off.active_persona_scope();
        app.load_features();
        assert!(app.feats.iter().all(|item| item.kind != FeatureKind::Mcp));
    }
}

#[cfg(test)]
mod search_tests {
    use super::filter_models;

    #[test]
    fn model_search_is_a_case_insensitive_substring_filter() {
        let models: Vec<String> = ["glm-5.3-flash", "deepseek-v4-flash", "Qwen3.8-max"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(filter_models(&models, ""), vec![0, 1, 2]);
        assert_eq!(filter_models(&models, "  "), vec![0, 1, 2]);
        assert_eq!(filter_models(&models, "flash"), vec![0, 1]);
        assert_eq!(filter_models(&models, "QWEN"), vec![2]);
        assert_eq!(filter_models(&models, "nope"), Vec::<usize>::new());
    }
}
