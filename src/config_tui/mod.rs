mod antigravity_form;
mod claude_code_form;
mod codebuddy_form;
mod codex_form;
mod features;
mod pending;
mod personas;
mod platforms;
mod plugin_settings;
mod plugins;
mod providers;
mod real_context;
mod scheduled_messages;
mod settings;
mod tiers;
mod undo;
mod voice;
mod widgets;
use antigravity_form::*;
use claude_code_form::*;
use codebuddy_form::edit_codebuddy_provider_form;
use codex_form::*;
use features::*;
use pending::*;
use personas::*;
use platforms::*;
use plugin_settings::*;
use plugins::*;
use providers::*;
use real_context::*;
use scheduled_messages::*;
use settings::*;
use tiers::*;
use undo::*;
use voice::*;
use widgets::*;

use anyhow::{bail, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use miyu_base::config::{
    merge_group_join_approval_settings, merge_real_context_settings, ActiveProviderModelConfig,
    AppConfig, PlatformCommandPermission, PlatformConversationConfig, PlatformConversationKind,
    PlatformModelPoolInheritance, PlatformModelRoute, PlatformPersonaOverride, PlatformRateLimit,
    PlatformSessionLimits, ProviderConfig, QqGroupJoinApprovalGroupConfig,
    QqGroupJoinApprovalPluginSettings, QqMemeCollectorPluginSettings,
    QqMessageHistoryPluginSettings, RealContextIdentityMapping, RealContextPluginSettings,
    MAX_COMMAND_OUTPUT_LINES, MAX_CROSS_SESSION_PREVIEW_LINES, MAX_PLATFORM_COMMAND_PREFIX_CHARS,
    MAX_PLATFORM_SESSION_QUEUED, MAX_PLATFORM_SESSION_RUNNING, MAX_REPL_REPLAY_TURNS,
    MAX_THINKING_SCROLL_LINES, QQ_GROUP_JOIN_APPROVAL_PLUGIN_ID, QQ_MEME_COLLECTOR_PLUGIN_ID,
    QQ_MESSAGE_HISTORY_PLUGIN_ID, REAL_CONTEXT_PLUGIN_ID,
};
use miyu_base::default_models::{OPENCODE_DEFAULT_VISION_MODEL, OPENCODE_PROVIDER_ID};
use miyu_base::i18n::{is_zh, text as t};
use miyu_base::paths::MiyuPaths;
use miyu_core::llm::{
    thinking_variant_options_for_model, ThinkingVariantOptions, ThinkingVariantPreferences,
};
use miyu_core::state::StateStore;
use miyu_hosts::platforms::commands::{self, PlatformCommandDescriptor};
use miyu_hosts::platforms::plugins::{
    active_judgement_skip_ids, apply_active_judgement_skip_editor_changes,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

pub fn run(paths: &MiyuPaths) -> Result<bool> {
    // 全屏 REPL 里开设置:备用屏已经是它的,这里退了再进会闪一下 shell 画面。
    run_with(paths, !crate::cli::in_fullscreen())
}

/// 调用方自己管着备用屏(引导之后紧接着进全屏 REPL):只进不退。
pub fn run_embedded(paths: &MiyuPaths) -> Result<bool> {
    run_with(paths, false)
}

fn run_with(paths: &MiyuPaths, owns_alt_screen: bool) -> Result<bool> {
    AppConfig::init_files(paths)?;
    miyu_base::models_cache::try_load(paths);
    miyu_base::models_cache::spawn_background_refresh(paths.clone());
    let config = AppConfig::load_or_default(paths)?;
    let thinking_variants = ThinkingVariantPreferences::load(paths);
    TerminalSession::start(paths, owns_alt_screen)?.run(paths, config, thinking_variants)
}

struct TerminalSession {
    ui: Ui,
    /// 备用屏是自己进的就自己退;是别人(全屏 REPL / 引导)的就只擦干净还回去。
    owns_alt_screen: bool,
}

impl TerminalSession {
    fn start(paths: &MiyuPaths, owns_alt_screen: bool) -> Result<Self> {
        terminal::enable_raw_mode()?;
        // 独立 `miyu config` 没有 REPL 的挂断看门狗;不发 SIGHUP 的断开
        // (tmux kill-pane、SSH 掉线)会让 crossterm 对 HUP fd 全速自旋。
        crate::cli::spawn_hangup_watchdog();
        let mut stdout = io::stdout();
        if owns_alt_screen {
            execute!(stdout, EnterAlternateScreen, Hide)?;
        } else {
            execute!(
                stdout,
                Hide,
                terminal::Clear(terminal::ClearType::All),
                crossterm::cursor::MoveTo(0, 0)
            )?;
        }
        Ok(Self {
            ui: Ui::new(paths)?,
            owns_alt_screen,
        })
    }

    fn release(&mut self) {
        if self.owns_alt_screen {
            let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        }
        // 嵌在全屏 REPL / 引导里：画面原样留着、光标继续藏着。接手的一方会在一个
        // 同步块里整屏重画并把光标放回输入框。以前这里清屏 + 光标归零 + Show，
        // 用户看到的就是光标先瞬移到左上角、再瞬移到输入框（09-14 实测）。
        let _ = terminal::disable_raw_mode();
    }

    fn run(
        mut self,
        paths: &MiyuPaths,
        mut config: AppConfig,
        mut thinking_variants: ThinkingVariantPreferences,
    ) -> Result<bool> {
        let result = run_main_menu(&mut self.ui, paths, &mut config, &mut thinking_variants);
        self.release();
        result
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // `run` 已经还过一次;再还一次只是幂等的擦屏/退备用屏。
        self.release();
    }
}

/// 保存成功后把用量账本里改过名的供应商 id 一起改掉。
///
/// 不放在改 id 的那一刻:用户可能改完不保存就退出。TUI 是独立进程,daemon 可能
/// 同时在往账本追加——这是 `usage::record_usage_at` 注释里说过的跨进程竞态,
/// 接受。账本改失败不阻断保存,配置已经落盘了。
fn sync_usage_ledger_after_save(
    paths: &MiyuPaths,
    pristine_config: Option<&String>,
    config: &AppConfig,
) {
    let Some(before) = pristine_config.and_then(|raw| serde_json::from_str::<AppConfig>(raw).ok())
    else {
        return;
    };
    let path = paths
        .state_dir
        .join(miyu_core::state::usage::USAGE_HISTORY_FILE);
    for (old, new) in
        miyu_base::config::detect_provider_renames(&before.providers, &config.providers)
    {
        match miyu_core::state::usage::rename_provider(&path, &old, &new) {
            Ok(rows) => tracing::info!(
                old = %old,
                new = %new,
                rows,
                "{}",
                t(
                    "usage ledger provider renamed",
                    "用量账本供应商 id 已同步改名"
                )
            ),
            Err(error) => tracing::warn!(
                error = %error, old = %old, new = %new,
                "renaming usage ledger providers failed"
            ),
        }
    }
}

/// 退出时比的必须是「保存下去会写成什么」,不是内存里长什么样。
///
/// `AppConfig::memory` 是记忆配置的旧位置,带 `skip_serializing`:`to_string`
/// 里永远看不见它,而 `save()` 会把它折进 `plugins.memory` 再写盘。直接比
/// 序列化结果的话,只动到旧位置的修改就是「看不见的脏」——退出不提示保存,
/// 改动静默丢失。这里按 `save()` 的同一套折叠先归一再比。
fn dirty_snapshot(config: &AppConfig) -> Option<String> {
    let mut probe = config.clone();
    probe.plugins.memory = probe.memory_config().clone();
    probe.memory = miyu_base::config::MemoryConfig::default();
    serde_json::to_string(&probe).ok()
}

fn run_main_menu(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
    thinking_variants: &mut ThinkingVariantPreferences,
) -> Result<bool> {
    // Detects edits on quit; sub-menus mutate `config` in place without any
    // dirty flag of their own.
    let pristine_config = dirty_snapshot(config);
    // 人格清单与开发模式提示词是独立文件，攒着跟配置一起落盘（用户 09-20：
    // 别改一下就写一次，走同一个「保存并退出」）。
    let mut pending = PendingWrites::default();
    let mut selected = 0usize;
    loop {
        let active = active_label(config);
        let multimodal = active_multimodal_label(config);
        let options = [
            t("Providers and models", "供应商和模型").to_string(),
            format!(
                "{} ({}: {active})",
                t("Configure global text models", "配置全局文本模型"),
                t("Current", "当前")
            ),
            format!(
                "{} ({}: {multimodal})",
                t("Configure global multimodal models", "配置全局多模态模型"),
                t("Current", "当前")
            ),
            format!(
                "{} ({}: {})",
                t("Configure embedding model", "配置 Embedding 模型"),
                t("Current", "当前"),
                embedding_model_label(config)
            ),
            t("Configure tiered model pools", "配置分级模型池").to_string(),
            format!(
                "{} ({})",
                t("Persona & features", "人格和功能"),
                active_persona_label(config)
            ),
            format!(
                "{} ({})",
                t("IM platforms", "接入通讯平台"),
                platforms_label(config)
            ),
            t("Global settings", "全局参数设置").to_string(),
            format!(
                "{} ({}: {} · TTS: {})",
                t("Voice", "语音功能"),
                t("wake", "唤醒"),
                if config.voice.enabled {
                    t("on", "开")
                } else {
                    t("off", "关")
                },
                if config.voice.tts.enabled {
                    t("on", "开")
                } else {
                    t("off", "关")
                },
            ),
            t("Save and exit", "保存并退出").to_string(),
        ];
        draw_menu(ui, t(" CONFIG ", " 配置 "), &options, selected, "")?;

        match read_key(ui)? {
            KeyCode::Char('q') | KeyCode::Esc => {
                let snapshot = dirty_snapshot(config);
                let dirty = thinking_variants.is_dirty()
                    || snapshot.is_none()
                    || snapshot != pristine_config
                    || !pending.is_empty();
                if !dirty {
                    return Ok(false);
                }
                if confirm_save_on_exit(ui)? {
                    match config.save(paths) {
                        Ok(()) => {
                            thinking_variants.save(paths)?;
                            pending.flush(config, paths)?;
                            sync_usage_ledger_after_save(paths, pristine_config.as_ref(), config);
                            return Ok(true);
                        }
                        Err(error) => {
                            // 保存失败(如校验不过)不能崩出:崩出会丢掉本次
                            // 全部内存修改,留在菜单让用户改完再存。
                            show_tui_error(ui, &error)?;
                            continue;
                        }
                    }
                }
                return Ok(false);
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => {
                let outcome = match selected {
                    0 => ProviderBrowser::new(paths, config, thinking_variants).run(ui),
                    1 => select_active_provider(ui, config),
                    2 => select_active_multimodal_provider(ui, config),
                    3 => edit_embedding_model(ui, config),
                    4 => select_model_tiers(ui, config),
                    5 => edit_persona_menu(ui, paths, config, &mut pending),
                    6 => select_platforms(ui, paths, config),
                    7 => edit_settings(ui, config),
                    8 => edit_voice(ui, paths, config),
                    9 => match config.save(paths) {
                        Ok(()) => {
                            thinking_variants.save(paths)?;
                            pending.flush(config, paths)?;
                            sync_usage_ledger_after_save(paths, pristine_config.as_ref(), config);
                            return Ok(true);
                        }
                        Err(error) => Err(error),
                    },
                    _ => Ok(()),
                };
                if let Err(error) = outcome {
                    // 子界面的表单解析/保存错误只作废当次输入,config 的
                    // 内存态还在;显示错误后回主菜单,不让 TUI 整个崩出。
                    show_tui_error(ui, &error)?;
                }
            }
            _ => {}
        }
    }
}

impl<'a> ProviderBrowser<'a> {
    /// 状态行：说一声（金色）。
    fn note(&mut self, text: String) {
        self.status = text;
        self.status_error = false;
    }

    /// 状态行：出错了（暖红）。拉模型失败、删不掉的东西走这条。
    fn warn(&mut self, text: String) {
        self.status = text;
        self.status_error = true;
    }

    fn new(
        paths: &'a MiyuPaths,
        config: &'a mut AppConfig,
        thinking_variants: &'a mut ThinkingVariantPreferences,
    ) -> Self {
        Self {
            paths,
            config,
            thinking_variants,
            active_col: 0,
            provider_idx: 0,
            provider_scroll: 0,
            org_idx: 0,
            org_scroll: 0,
            model_idx: 0,
            model_scroll: 0,
            filter: String::new(),
            filter_mode: false,
            raw_models: Vec::new(),
            orgs: Vec::new(),
            models: Vec::new(),
            status: String::new(),
            status_error: false,
            loading: false,
            fetch_seq: 0,
            fetch_rx: None,
            undo: ConfigUndo::default(),
        }
    }

    fn run(mut self, ui: &mut Ui) -> Result<()> {
        self.refresh_models();
        loop {
            self.poll_fetch_result();
            self.draw(ui)?;
            match read_key_with_timeout(
                ui,
                if self.loading {
                    Some(Duration::from_millis(100))
                } else {
                    None
                },
            )? {
                None => continue,
                Some(key) => match key {
                    key if self.filter_mode => self.handle_filter_key(key),
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Left | KeyCode::Char('h') => self.move_left(),
                    KeyCode::Right | KeyCode::Char('l') => self.move_right(),
                    KeyCode::Up | KeyCode::Char('k') => self.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => self.move_down(),
                    KeyCode::Char('/') => {
                        self.filter_mode = true;
                        self.filter.clear();
                        self.rebuild_models();
                    }
                    KeyCode::Char('r') => self.refresh_models(),
                    KeyCode::Char('a') => self.add_provider(ui)?,
                    KeyCode::Char('n') => self.add_custom_model(ui)?,
                    // 模型列的 d 是"删这一行",不是"删供应商":列表几百行、
                    // 自定义模型只在最上面几行,同一个键按行改语义会让人在
                    // 光标差一行时删掉整个供应商。删供应商去左边两列。
                    KeyCode::Char('d') if self.active_col == 2 => self.delete_custom_model(),
                    KeyCode::Char('d') => self.delete_provider(),
                    KeyCode::Char('u') => self.undo_delete(),
                    KeyCode::Tab if self.active_col == 2 => self.toggle_model_activation(),
                    KeyCode::Enter | KeyCode::Char('i') => self.select_or_edit(ui)?,
                    _ => {}
                },
            }
        }
    }

    fn handle_filter_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => {
                self.filter_mode = false;
                self.filter.clear();
            }
            KeyCode::Enter => self.filter_mode = false,
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(ch) => self.filter.push(ch),
            _ => {}
        }
        self.rebuild_models();
    }

    fn move_left(&mut self) {
        self.active_col = self.active_col.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.active_col = (self.active_col + 1).min(2);
    }

    fn move_up(&mut self) {
        match self.active_col {
            0 => {
                self.provider_idx = self.provider_idx.saturating_sub(1);
                self.provider_scroll = column_scroll(
                    self.provider_idx,
                    self.provider_scroll,
                    column_visible_rows(),
                );
                self.refresh_models();
            }
            1 => {
                self.org_idx = self.org_idx.saturating_sub(1);
                self.org_scroll =
                    column_scroll(self.org_idx, self.org_scroll, column_visible_rows());
                self.rebuild_models();
            }
            2 => {
                self.model_idx = self.model_idx.saturating_sub(1);
                self.model_scroll =
                    column_scroll(self.model_idx, self.model_scroll, column_visible_rows());
            }
            _ => {}
        }
    }

    fn move_down(&mut self) {
        match self.active_col {
            0 => {
                self.provider_idx =
                    (self.provider_idx + 1).min(self.config.providers.len().saturating_sub(1));
                self.provider_scroll = column_scroll(
                    self.provider_idx,
                    self.provider_scroll,
                    column_visible_rows(),
                );
                self.refresh_models();
            }
            1 => {
                self.org_idx = (self.org_idx + 1).min(self.orgs.len().saturating_sub(1));
                self.org_scroll =
                    column_scroll(self.org_idx, self.org_scroll, column_visible_rows());
                self.rebuild_models();
            }
            2 => {
                self.model_idx = (self.model_idx + 1).min(self.models.len().saturating_sub(1));
                self.model_scroll =
                    column_scroll(self.model_idx, self.model_scroll, column_visible_rows());
            }
            _ => {}
        }
    }

    fn refresh_models(&mut self) {
        self.provider_idx = self
            .provider_idx
            .min(self.config.providers.len().saturating_sub(1));
        self.raw_models.clear();
        self.orgs = vec!["All".to_string()];
        self.models.clear();
        self.fetch_seq += 1;
        if let Some(provider) = self.config.providers.get(self.provider_idx).cloned() {
            let seq = self.fetch_seq;
            let cli_binary =
                miyu_base::provider_catalog::builtin_cli_binary(&self.config, &provider);
            let (tx, rx) = mpsc::channel();
            self.fetch_rx = Some(rx);
            self.loading = true;
            self.note(t("Fetching model list...", "正在获取模型列表...").to_string());
            std::thread::spawn(move || {
                let result =
                    miyu_base::provider_catalog::fetch_models(&provider, cli_binary.as_deref())
                        .map_err(|err| err.to_string());
                let _ = tx.send((seq, result));
            });
        } else {
            self.fetch_rx = None;
            self.loading = false;
            self.status.clear();
        }
        self.org_idx = 0;
        self.model_idx = 0;
        self.org_scroll = 0;
        self.model_scroll = 0;
    }

    fn poll_fetch_result(&mut self) {
        let Some(rx) = &self.fetch_rx else {
            return;
        };
        let Ok((seq, result)) = rx.try_recv() else {
            return;
        };
        if seq != self.fetch_seq {
            return;
        }
        self.loading = false;
        self.fetch_rx = None;
        match result {
            Ok(models) => {
                self.note(if is_zh() {
                    format!("已获取 {} 个模型", models.len())
                } else {
                    format!("Fetched {} models", models.len())
                });
                self.raw_models = models;
            }
            Err(err) => {
                let status = if is_zh() {
                    format!("获取模型失败: {err}")
                } else {
                    format!("Failed to fetch models: {err}")
                };
                self.warn(format_status_line(&status));
                self.raw_models.clear();
            }
        }
        self.rebuild_models();
    }

    /// 该供应商手填的模型名。
    fn custom_models(&self) -> Vec<String> {
        self.config
            .providers
            .get(self.provider_idx)
            .map(|provider| provider.custom_models.clone())
            .unwrap_or_default()
    }

    fn rebuild_models(&mut self) {
        let mut grouped = group_models(&self.custom_models(), &self.raw_models, &self.filter);
        self.orgs = grouped.keys().cloned().collect();
        if self.orgs.is_empty() {
            self.orgs.push("All".to_string());
        }
        self.org_idx = self.org_idx.min(self.orgs.len().saturating_sub(1));
        self.models = grouped.remove(&self.orgs[self.org_idx]).unwrap_or_default();
        self.model_idx = self.model_idx.min(self.models.len().saturating_sub(1));
        self.org_scroll = column_scroll(self.org_idx, self.org_scroll, column_visible_rows());
        self.model_scroll = column_scroll(self.model_idx, self.model_scroll, column_visible_rows());
    }

    fn add_provider(&mut self, ui: &mut Ui) -> Result<()> {
        if let Some(provider) = edit_provider_form(ui, ProviderConfig::new_custom())? {
            self.config.upsert_provider(provider);
            self.provider_idx = self.config.providers.len().saturating_sub(1);
            self.refresh_models();
        }
        Ok(())
    }

    /// 手填一个模型名。供应商的 `/models` 目录是它自己报的,内测模型不在
    /// 里面,只能这样进来。加完就激活——名字是用户特意打进来的,再让他按
    /// 一次 Tab 是白问一句;不想要了 Tab 取消,条目仍留在列表顶端。
    fn add_custom_model(&mut self, ui: &mut Ui) -> Result<()> {
        if self.config.providers.get(self.provider_idx).is_none() {
            return Ok(());
        }
        let mut fields = vec![Field::new(t("Model name", "模型名"), String::new())];
        if !run_form_editing(ui, t(" ADD CUSTOM MODEL ", " 添加自定义模型 "), &mut fields)? {
            return Ok(());
        }
        let name = fields[0].value.trim().to_string();
        if name.is_empty() {
            return Ok(());
        }
        // 先记快照再插:名字重不重复的判据只有 `insert_custom_model` 一份,
        // 没插成就把这一步快照丢掉,撤销栈里不留空步。
        self.undo.record(self.config);
        let added = insert_custom_model(self.config, self.provider_idx, &self.raw_models, &name);
        if added {
            if let Some(provider) = self.config.providers.get_mut(self.provider_idx) {
                miyu_base::provider_catalog::auto_configure_model_tags(self.paths, provider, &name);
            }
        } else {
            self.undo.undo(self.config);
        }
        self.note(if added {
            if is_zh() {
                format!("已添加并激活自定义模型: {name}")
            } else {
                format!("Added and activated custom model: {name}")
            }
        } else if is_zh() {
            format!("模型已在列表中: {name}")
        } else {
            format!("Model is already listed: {name}")
        });
        self.reveal_model(&name);
        Ok(())
    }

    /// 把光标移到这个模型上。过滤词或组织栏把它挡住了就先让开——刚加完
    /// 却看不见,用户没法判断到底加上没有。
    fn reveal_model(&mut self, full: &str) {
        if !self.filter.is_empty()
            && !full
                .to_ascii_lowercase()
                .contains(&self.filter.to_ascii_lowercase())
        {
            self.filter.clear();
        }
        self.rebuild_models();
        if self.models.iter().all(|model| model.full != full) {
            // 每个模型都会进 "All" 组,所以那一组一定找得到。
            if let Some(index) = self.orgs.iter().position(|org| org == "All") {
                self.org_idx = index;
                self.org_scroll =
                    column_scroll(self.org_idx, self.org_scroll, column_visible_rows());
                self.rebuild_models();
            }
        }
        if let Some(index) = self.models.iter().position(|model| model.full == full) {
            self.active_col = 2;
            self.model_idx = index;
            self.model_scroll =
                column_scroll(self.model_idx, self.model_scroll, column_visible_rows());
        }
    }

    /// 删掉光标所在的自定义模型:清掉手填清单、激活状态与各处池子引用。
    /// 拉取来的模型删不掉——它是供应商目录的内容,这里只是显示。
    fn delete_custom_model(&mut self) {
        let Some(model) = self
            .models
            .get(self.model_idx)
            .map(|entry| entry.full.clone())
        else {
            return;
        };
        self.undo.record(self.config);
        if !remove_custom_model(self.config, self.provider_idx, &model) {
            self.undo.undo(self.config);
            self.warn(
                t(
                    "Only manually added models can be deleted here; delete a provider from the provider column.",
                    "这里只能删手动添加的模型;删供应商请到供应商列。",
                )
                .to_string(),
            );
            return;
        }
        self.note(if is_zh() {
            format!("已删除自定义模型: {model}")
        } else {
            format!("Deleted custom model: {model}")
        });
        self.rebuild_models();
    }

    fn delete_provider(&mut self) {
        if self.config.providers.is_empty() {
            return;
        }
        if self
            .config
            .providers
            .get(self.provider_idx)
            .is_some_and(ProviderConfig::is_builtin_cli_provider)
        {
            // 内置供应商删了下次加载也会被重新注入,徒增困惑;要停用走编辑
            // 表单里的启用开关。
            self.warn(
                t(
                    "This built-in CLI provider cannot be deleted; disable it in its edit form instead.",
                    "内置 CLI 供应商不可删除;要停用请在编辑表单里关掉启用开关。",
                )
                .to_string(),
            );
            return;
        }
        self.undo.record(self.config);
        let removed = self.config.providers.remove(self.provider_idx);
        self.config.remove_provider_references(&removed.id);
        self.provider_idx = self
            .provider_idx
            .min(self.config.providers.len().saturating_sub(1));
        self.refresh_models();
    }

    /// 退回上一步。分步的:连按几次就退几步（上限见 `ConfigUndo`）。
    fn undo_delete(&mut self) {
        if !self.undo.undo(self.config) {
            return;
        }
        self.provider_idx = self
            .provider_idx
            .min(self.config.providers.len().saturating_sub(1));
        self.refresh_models();
    }

    fn select_or_edit(&mut self, ui: &mut Ui) -> Result<()> {
        match self.active_col {
            0 => {
                if let Some(provider) = self.config.providers.get(self.provider_idx).cloned() {
                    // 内置 Claude Code 走专用表单:没有 HTTP 概念,只有启用
                    // 总开关与 CLI 中转设置。
                    let edited = if provider.is_claude_code() {
                        edit_claude_code_provider_form(
                            ui,
                            provider,
                            &mut self.config.plugins.claude_code,
                        )?
                    } else if provider.is_antigravity() {
                        edit_antigravity_provider_form(
                            ui,
                            provider,
                            &mut self.config.plugins.antigravity,
                        )?
                    } else if provider.is_codex() {
                        edit_codex_provider_form(ui, provider, &mut self.config.plugins.codex)?
                    } else if provider.is_codebuddy() {
                        edit_codebuddy_provider_form(
                            ui,
                            provider,
                            &mut self.config.plugins.codebuddy,
                        )?
                    } else {
                        edit_provider_form(ui, provider)?
                    };
                    if let Some(provider) = edited {
                        let old_id = self.config.providers[self.provider_idx].id.clone();
                        self.config.providers[self.provider_idx] = provider.clone();
                        if self.config.active_provider == old_id {
                            self.config.active_provider = provider.id.clone();
                        }
                        if old_id != provider.id {
                            self.config
                                .rename_provider_references(&old_id, &provider.id);
                            self.thinking_variants
                                .rename_provider(&old_id, &provider.id);
                        }
                        self.refresh_models();
                    }
                }
            }
            2 => {
                let mut model_updated = false;
                if let Some(model) = self.models.get(self.model_idx).cloned() {
                    if let Some(provider) = self.config.providers.get_mut(self.provider_idx) {
                        miyu_base::provider_catalog::auto_configure_model_tags(
                            self.paths,
                            provider,
                            &model.full,
                        );
                    }
                    if let Some(provider) = self.config.providers.get_mut(self.provider_idx) {
                        if edit_model_form(
                            ui,
                            self.paths,
                            provider,
                            &model.full,
                            self.thinking_variants,
                        )? {
                            self.config.active_provider = provider.id.clone();
                            model_updated = true;
                            self.note(if is_zh() {
                                format!("已更新模型设置: {}", model.full)
                            } else {
                                format!("Updated model settings: {}", model.full)
                            });
                        }
                    }
                }
                if model_updated {
                    self.config.prune_model_references();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn toggle_model_activation(&mut self) {
        if self.active_col != 2 {
            return;
        }
        let mut removed = None;
        if let (Some(provider), Some(model)) = (
            self.config.providers.get_mut(self.provider_idx),
            self.models.get(self.model_idx),
        ) {
            if let Some(index) = provider.models.iter().position(|item| item == &model.full) {
                let provider_id = provider.id.clone();
                let model = model.full.clone();
                provider.models.remove(index);
                if provider.default_model == model {
                    provider.default_model = provider.models.first().cloned().unwrap_or_default();
                }
                self.note(if is_zh() {
                    format!("已取消激活模型: {model}")
                } else {
                    format!("Deactivated model: {model}")
                });
                removed = Some((provider_id, model));
            } else {
                provider.models.push(model.full.clone());
                miyu_base::provider_catalog::auto_configure_model_tags(
                    self.paths,
                    provider,
                    &model.full,
                );
                if provider.default_model.trim().is_empty() {
                    provider.default_model = model.full.clone();
                }
                self.note(if is_zh() {
                    format!("已激活模型: {}", model.full)
                } else {
                    format!("Activated model: {}", model.full)
                });
            }
        }
        if let Some((provider_id, model)) = removed {
            self.config
                .remove_active_model_references(&provider_id, &model);
        }
    }

    fn draw(&self, ui: &mut Ui) -> Result<()> {
        // 不再给 active_provider 打星号。这个菜单只回答「有哪些供应商、各自有
        // 哪些模型可用」;「现在用谁」由「配置文本模型」那个池子决定。星号标的
        // 是 `active_provider`——它现在只是 `provider(None)` 的兜底,在这里显示
        // 会让人以为在这一列按一下就能换模型。
        let providers = self
            .config
            .providers
            .iter()
            .map(|provider| {
                if provider.enabled {
                    provider.display_name.clone()
                } else {
                    // 目前只有内置 Claude Code 会处于未启用态,标出来免得
                    // 用户找不到"为什么模型列表里没有它"。
                    format!("{}{}", provider.display_name, t(" (disabled)", " (未启用)"))
                }
            })
            .collect::<Vec<_>>();
        let models = self
            .models
            .iter()
            .map(|model| {
                let provider = self.config.providers.get(self.provider_idx);
                let active = provider
                    .map(|provider| provider.models.iter().any(|item| item == &model.full))
                    .unwrap_or(false);
                // 标出手填的:只有它们能在这一列删掉,不标就看不出哪几行的 d
                // 是活的。
                let custom = provider
                    .map(|provider| {
                        provider
                            .custom_models
                            .iter()
                            .any(|item| item == &model.full)
                    })
                    .unwrap_or(false);
                format!(
                    "{} {}{}",
                    if active { "[*]" } else { "[ ]" },
                    model.name,
                    if custom {
                        t(" (custom)", "(自定义)")
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>();
        let orgs = self
            .orgs
            .iter()
            .map(|org| {
                if org == "All" {
                    t("All", "全部").to_string()
                } else {
                    org.clone()
                }
            })
            .collect::<Vec<_>>();

        let help = if self.filter_mode {
            if is_zh() {
                format!("搜索: {}_  [Enter]确认 [Esc]取消", self.filter)
            } else {
                format!("Search: {}_  [Enter]confirm [Esc]cancel", self.filter)
            }
        } else {
            // 按列列键位。一行列全部就得截断,而被截掉的总是排在末尾的
            // `[q]返回`——最该让人看见的那个。Enter / d 本来就按列改语义,
            // 分开写反而说得更准。
            let keys = if self.active_col == 2 {
                t(
                    "[h/l]column [j/k]move [Tab]activate [Enter]edit [n]add model [d]delete custom [/]search [r]refresh [q]back",
                    "[h/l]切栏 [j/k]移动 [Tab]激活 [Enter]模型设置 [n]添加模型 [d]删除自定义 [/]搜索 [r]刷新 [q]返回",
                )
            } else {
                t(
                    "[h/l]column [j/k]move [Enter]edit [n]add model [a]add provider [d]delete [/]search [r]refresh [q]back",
                    "[h/l]切栏 [j/k]移动 [Enter]编辑 [n]添加模型 [a]添加供应商 [d]删除供应商 [/]搜索 [r]刷新 [q]返回",
                )
            };
            format!("{keys}{}", self.undo.hint())
        };
        let models_title = if self.filter.is_empty() {
            t(" MODELS ", " 模型 ").to_string()
        } else if is_zh() {
            format!("{} /{}", t(" MODELS ", " 模型 ").trim(), self.filter)
        } else {
            format!("{} /{}", t(" MODELS ", " 模型 ").trim(), self.filter)
        };
        let columns = [
            Column {
                title: t(" PROVIDERS ", " 供应商 "),
                items: &providers,
                selected: self.provider_idx,
                scroll: self.provider_scroll,
                active: self.active_col == 0,
                weight: 28,
            },
            Column {
                title: t(" ORGANIZATION ", " 组织 "),
                items: &orgs,
                selected: self.org_idx,
                scroll: self.org_scroll,
                active: self.active_col == 1,
                weight: 22,
            },
            Column {
                title: &models_title,
                items: &models,
                selected: self.model_idx,
                scroll: self.model_scroll,
                active: self.active_col == 2,
                weight: 50,
            },
        ];
        draw_columns(
            ui,
            t(" PROVIDERS AND MODELS ", " 供应商和模型 "),
            &columns,
            &help,
            &self.status,
            self.status_error,
        )
    }
}

use miyu_base::config::EMBEDDING_MODALITY;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod dirty_tests {
    use super::*;

    /// 只动到旧位置的记忆配置,退出时也必须算脏——它虽然不进序列化,`save()`
    /// 却会把它折进 `plugins.memory` 写盘,不提示就等于静默丢改动。
    #[test]
    fn a_change_only_in_the_legacy_memory_slot_still_counts_as_dirty() {
        let mut config = AppConfig::default();
        let raw_before = serde_json::to_string(&config).unwrap();
        let snapshot_before = dirty_snapshot(&config).unwrap();

        config.memory.enabled = !config.memory.enabled;

        // 裸序列化确实看不见这一改(skip_serializing):这就是原来漏判的原因。
        assert_eq!(serde_json::to_string(&config).unwrap(), raw_before);
        // 按「会写成什么」来比就看得见了。
        assert_ne!(dirty_snapshot(&config).unwrap(), snapshot_before);
    }
}
