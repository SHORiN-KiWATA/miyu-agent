//! 人格与身份的增删改。
//!
//! 改人格名字要连带搬目录、迁数据库归属（`move_persona_scope`），任何一步失败
//! 都要能退回去——这也是为什么这里的操作不是简单的写配置。
//!
//! `ensure_persona_name_available` 在动手之前先查重：人格名同时是目录名和数据
//! 库里的 scope，撞了就会把两个人格的记忆搅在一起。

use crate::config_tui::*;

/// 「人格」菜单：这个人格是谁、挂哪些功能、提示词怎么写。
///
/// 2026-09-20 由「插件配置」+「自定义提示词」合并而来：功能本来就是跟着人格
/// 走的（`persona.toml`），而插件的「怎么配」是机器的事——后者现在挂在功能表
/// 每一行的回车上，不再单占一个菜单项。
pub(in crate::config_tui) fn edit_persona_menu(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
    pending: &mut PendingWrites,
) -> Result<()> {
    let mut selected = 0usize;
    // 数功能要扫脚本与技能目录，每帧算一遍太重：进来算一次，改过再算。
    let mut counts = feature_counts(config, paths, pending);
    loop {
        let persona = persona_display_name_for(config);
        let options = [
            format!("{} ({persona})", t("Current persona", "当前人格")),
            format!(
                "{} ({}/{})",
                t("Enabled features", "启用的功能"),
                counts.0,
                counts.1
            ),
            format!(
                "{} ({})",
                t("Prompt and preset dialogs", "提示词与预设对话"),
                if config.prompt.active_persona.trim().is_empty() {
                    t("built-in Miyu", "内置 Miyu")
                } else {
                    t("custom", "自定义")
                }
            ),
            format!(
                "{} ({})",
                t("Anti-amnesia reminder", "防失忆提醒"),
                if config.prompt.persona_reminder {
                    t("Enabled", "启用")
                } else {
                    t("Disabled", "禁用")
                }
            ),
            format!(
                "{} ({} {})",
                t("Reminder interval", "提醒间隔"),
                config.prompt.persona_reminder_interval.max(1),
                t("turns", "轮")
            ),
            String::new(),
            t("User identity", "用户身份").to_string(),
            t("Dev mode prompt", "开发模式提示词").to_string(),
        ];
        draw_menu(
            ui,
            t(" PERSONA & FEATURES ", " 人格和功能 "),
            &options,
            selected,
            t("[Enter]select/toggle [q]back", "[Enter]选择/切换 [q]返回"),
        )?;
        match read_key(ui)? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
                // 第 5 行是分隔用的空行，跳过去。
                if selected == 5 {
                    selected = 4;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                selected = (selected + 1).min(options.len() - 1);
                if selected == 5 {
                    selected = 6;
                }
            }
            KeyCode::Enter => match selected {
                0 => {
                    edit_personas(ui, paths, config)?;
                    counts = feature_counts(config, paths, pending);
                }
                1 => {
                    edit_features(ui, paths, config, pending)?;
                    counts = feature_counts(config, paths, pending);
                }
                2 => edit_active_persona_prompt(ui, paths, config)?,
                3 => config.prompt.persona_reminder = !config.prompt.persona_reminder,
                4 => {
                    if let Some(value) = edit_inline_value(
                        ui,
                        t("Reminder interval", "提醒间隔"),
                        &config.prompt.persona_reminder_interval.to_string(),
                        false,
                    )? {
                        if let Ok(interval) = value.trim().parse::<u32>() {
                            config.prompt.persona_reminder_interval = interval.max(1);
                        }
                    }
                }
                6 => edit_identities(ui, paths, config)?,
                7 => edit_dev_prompt(ui, paths, pending)?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn active_persona_label(config: &AppConfig) -> String {
    persona_display_name_for(config)
}

fn persona_display_name_for(config: &AppConfig) -> String {
    if config.prompt.active_persona.trim().is_empty() {
        "Miyu".to_string()
    } else {
        persona_display_name(&config.prompt.active_persona).to_string()
    }
}

/// 当前人格挂着几件、一共几件。
fn feature_counts(
    config: &AppConfig,
    paths: &MiyuPaths,
    pending: &PendingWrites,
) -> (usize, usize) {
    let scope = config.active_persona_scope();
    let manifest = pending.manifest(config, paths, &scope);
    let sources = crate::feature_sources::collect(config, paths);
    let items = miyu_base::config::feature_catalog::catalog(
        &manifest,
        &sources,
        miyu_core::skills::is_default_persona(config),
        miyu_base::config::feature_catalog::CatalogScope::Settings,
        Some(config),
    );
    (items.iter().filter(|item| item.on).count(), items.len())
}

/// 当前人格的提示词与预设对话。内置 Miyu 只有附加件可改（本体只读）。
fn edit_active_persona_prompt(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
) -> Result<()> {
    let name = config.prompt.active_persona.trim().to_string();
    if name.is_empty() {
        return edit_miyu_persona_extras(ui, paths, config);
    }
    if let Some(values) = edit_persona(ui, paths, config, &name)? {
        apply_persona_edit(paths, config, &name, &values.name, &values.content)?;
        write_persona_aux(
            paths,
            config,
            &miyu_base::config::persona_scope_name(&values.name),
            &values.hint,
            &values.dialogs,
        )?;
        if config.prompt.active_persona == name {
            config.prompt.active_persona = values.name;
        }
    }
    Ok(())
}

/// 开发模式的「AI 提示词」:编辑 config/dev-prompt.md 一个文件。09-24 起默认为空
/// (没有内置角色句),清空保存=删文件;记忆按保留人格 "dev" 落库,与这份提示词的
/// 内容完全解耦——怎么改都不会切库。
pub(in crate::config_tui) fn edit_dev_prompt(
    ui: &mut Ui,
    paths: &MiyuPaths,
    pending: &mut PendingWrites,
) -> Result<()> {
    // 老版本自动写进去的那行默认,运行时当没写,这里也显示成空(同一个口径)。
    let current = pending.dev_prompt(paths);
    let current = match current.trim() {
        miyu_base::config::LEGACY_DEV_SYSTEM_PROMPT => "",
        other => other,
    }
    .to_string();
    let mut fields = vec![Field::textarea(
        t("AI prompt (empty = none)", "AI 提示词(留空=不加)"),
        current.clone(),
    )];
    // 不摆「保存 / 返回」两个按钮：这一屏只有一个字段，编辑完退出就写
    // （用户 2026-09-20）。它是独立文件（`dev-prompt.md`），跟不了设置界面
    // 「退出时一起保存」那条路——那条只管 config.jsonc。
    run_form_without_buttons(ui, t(" DEV MODE ", " 开发模式 "), &mut fields)?;
    // 写在内存里，跟配置一起走「保存并退出」。没改就不记，免得一进一出就凭空
    // 动一次文件。
    let value = fields[0].value.trim();
    if value != current {
        pending.set_dev_prompt(value.to_string());
    }
    Ok(())
}

pub(in crate::config_tui) fn edit_personas(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
) -> Result<()> {
    manage_personas(ui, paths, config, PersonaMenuTarget::Global)?;
    Ok(())
}

pub(in crate::config_tui) enum PersonaMenuTarget {
    Global,
    Platform(PlatformPersonaOverride),
}

impl PersonaMenuTarget {
    pub(in crate::config_tui) fn custom_offset(&self) -> usize {
        match self {
            Self::Global => 1,
            Self::Platform(_) => 2,
        }
    }

    pub(in crate::config_tui) fn is_miyu(&self, config: &AppConfig) -> bool {
        match self {
            Self::Global => config.prompt.active_persona.trim().is_empty(),
            Self::Platform(persona) => matches!(persona, PlatformPersonaOverride::Miyu),
        }
    }

    pub(in crate::config_tui) fn custom_name<'a>(
        &'a self,
        config: &'a AppConfig,
    ) -> Option<&'a str> {
        match self {
            Self::Global => (!config.prompt.active_persona.trim().is_empty())
                .then_some(config.prompt.active_persona.as_str()),
            Self::Platform(persona) => persona.custom_name(),
        }
    }

    pub(in crate::config_tui) fn activate_inherit(&mut self) {
        if let Self::Platform(persona) = self {
            *persona = PlatformPersonaOverride::Inherit;
        }
    }

    pub(in crate::config_tui) fn activate_miyu(&mut self, config: &mut AppConfig) {
        match self {
            Self::Global => config.prompt.active_persona.clear(),
            Self::Platform(persona) => *persona = PlatformPersonaOverride::Miyu,
        }
    }

    pub(in crate::config_tui) fn activate_custom(&mut self, config: &mut AppConfig, name: String) {
        match self {
            Self::Global => config.prompt.active_persona = name,
            Self::Platform(persona) => *persona = PlatformPersonaOverride::Custom { name },
        }
    }

    pub(in crate::config_tui) fn rename_custom(&mut self, old_name: &str, new_name: &str) {
        if let Self::Platform(persona) = self {
            if persona.custom_name() == Some(old_name) {
                *persona = PlatformPersonaOverride::Custom {
                    name: new_name.to_string(),
                };
            }
        }
    }

    pub(in crate::config_tui) fn pending_reference_count(&self, name: &str) -> usize {
        usize::from(matches!(
            self,
            Self::Platform(PlatformPersonaOverride::Custom { name: current }) if current == name
        ))
    }

    pub(in crate::config_tui) fn into_platform(self) -> Option<PlatformPersonaOverride> {
        match self {
            Self::Global => None,
            Self::Platform(persona) => Some(persona),
        }
    }
}

pub(in crate::config_tui) fn manage_personas(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
    mut target: PersonaMenuTarget,
) -> Result<Option<PlatformPersonaOverride>> {
    std::fs::create_dir_all(config.prompts_dir_path(paths))?;
    let mut selected = 0usize;
    loop {
        let personas = list_personas(paths, config)?;
        let custom_offset = target.custom_offset();
        let mut options = Vec::with_capacity(personas.len() + custom_offset);
        if let PersonaMenuTarget::Platform(persona) = &target {
            options.push(format!(
                "{}{}",
                if persona.is_inherit() { "* " } else { "  " },
                t("Inherit current persona", "继承当前人格")
            ));
        }
        options.push(format!(
            "{}Miyu",
            if target.is_miyu(config) { "* " } else { "  " }
        ));
        options.extend(personas.iter().map(|name| {
            let display = persona_display_name(name);
            if target.custom_name(config) == Some(name.as_str()) {
                format!("* {display}")
            } else {
                format!("  {display}")
            }
        }));
        selected = selected.min(options.len().saturating_sub(1));
        draw_menu(
            ui,
            match &target {
                PersonaMenuTarget::Global => t(" AI PERSONA ", " AI 人格 "),
                PersonaMenuTarget::Platform(_) => {
                    t(" QQ CONVERSATION PERSONA ", " QQ 会话 AI 人格 ")
                }
            },
            &options,
            selected,
            t(
                "[Tab]activate [Enter]edit [a]add [d]delete [j/k]move [q]back",
                "[Tab]激活 [Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key(ui)? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(target.into_platform()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Tab => {
                if matches!(&target, PersonaMenuTarget::Platform(_)) && selected == 0 {
                    target.activate_inherit();
                } else if selected + 1 == custom_offset {
                    target.activate_miyu(config);
                } else if let Some(name) = personas.get(selected.saturating_sub(custom_offset)) {
                    target.activate_custom(config, name.clone());
                }
            }
            KeyCode::Char('a') => {
                if let Some(name) = new_persona(ui, paths, config)? {
                    target.activate_custom(config, name);
                }
            }
            KeyCode::Enter if selected >= custom_offset => {
                if let Some(name) = personas.get(selected - custom_offset) {
                    if let Some(values) = edit_persona(ui, paths, config, name)? {
                        apply_persona_edit(paths, config, name, &values.name, &values.content)?;
                        write_persona_aux(
                            paths,
                            config,
                            &miyu_base::config::persona_scope_name(&values.name),
                            &values.hint,
                            &values.dialogs,
                        )?;
                        target.rename_custom(name, &values.name);
                    }
                }
            }
            // 默认 Miyu 人格本体只读,但防失忆提示与预设对话是独立文件
            // (hints/default.md、dialogs/default.md),回车打开精简表单。
            KeyCode::Enter if selected + 1 == custom_offset => {
                edit_miyu_persona_extras(ui, paths, config)?;
            }
            KeyCode::Char('d') if selected >= custom_offset => {
                if let Some(name) = personas.get(selected - custom_offset) {
                    let persisted = AppConfig::load_or_default(paths)?;
                    let references = config
                        .platforms
                        .persona_reference_count(name)
                        .max(persisted.platforms.persona_reference_count(name))
                        .max(target.pending_reference_count(name));
                    if references > 0 {
                        message(
                            ui,
                            &if is_zh() {
                                format!(
                                    "该人格仍被 {references} 个 QQ 会话配置引用，请先解除引用。"
                                )
                            } else {
                                format!(
                                    "This persona is still used by {references} QQ conversation configuration(s). Remove those references first."
                                )
                            },
                        )?;
                        continue;
                    }
                    apply_persona_delete(paths, config, persisted, name)?;
                    selected = selected.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn apply_persona_edit(
    paths: &MiyuPaths,
    config: &mut AppConfig,
    old_name: &str,
    new_name: &str,
    content: &str,
) -> Result<()> {
    ensure_persona_name_available(paths, config, new_name, Some(old_name))?;
    if old_name == new_name {
        return write_persona(paths, config, new_name, content);
    }

    let old_path = config.persona_path(paths, old_name);
    let new_path = config.persona_path(paths, new_name);
    let old_content = std::fs::read(&old_path)?;
    let mut persisted = AppConfig::load_or_default(paths)?;
    let state = miyu_core::state::StateStore::new(paths)?;
    write_persona(paths, config, new_name, content)?;
    if let Err(error) = move_persona_scope(paths, config, old_name, new_name) {
        let _ = std::fs::remove_file(&new_path);
        return Err(error);
    }

    let old_scope = miyu_base::config::persona_scope_name(old_name);
    let new_scope = miyu_base::config::persona_scope_name(new_name);
    if let Err(error) = state.rename_persona_scope(&old_scope, &new_scope) {
        let _ = move_persona_scope(paths, config, new_name, old_name);
        let _ = std::fs::remove_file(&new_path);
        return Err(error);
    }
    if let Err(error) = std::fs::remove_file(&old_path) {
        let _ = state.rename_persona_scope(&new_scope, &old_scope);
        let _ = move_persona_scope(paths, config, new_name, old_name);
        let _ = std::fs::remove_file(&new_path);
        return Err(error.into());
    }

    persisted
        .platforms
        .rename_persona_references(old_name, new_name);
    if persisted.prompt.active_persona == old_name {
        persisted.prompt.active_persona = new_name.to_string();
    }
    if let Err(error) = persisted.save(paths) {
        let _ = std::fs::write(&old_path, old_content);
        let _ = std::fs::remove_file(&new_path);
        let _ = state.rename_persona_scope(&new_scope, &old_scope);
        let _ = move_persona_scope(paths, config, new_name, old_name);
        return Err(error);
    }

    config
        .platforms
        .rename_persona_references(old_name, new_name);
    if config.prompt.active_persona == old_name {
        config.prompt.active_persona = new_name.to_string();
    }
    Ok(())
}

pub(in crate::config_tui) fn apply_persona_delete(
    paths: &MiyuPaths,
    config: &mut AppConfig,
    mut persisted: AppConfig,
    name: &str,
) -> Result<()> {
    if persisted.prompt.active_persona == name {
        persisted.prompt.active_persona.clear();
        persisted.save(paths)?;
    }
    let scope = miyu_base::config::persona_scope_name(name);
    miyu_core::state::StateStore::new(paths)?.delete_persona_scope(&scope)?;
    let path = config.persona_path(paths, name);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    remove_persona_scope(paths, config, name)?;
    if config.prompt.active_persona == name {
        config.prompt.active_persona.clear();
    }
    Ok(())
}

pub(in crate::config_tui) struct PersonaFormValues {
    pub(in crate::config_tui) name: String,
    pub(in crate::config_tui) content: String,
    pub(in crate::config_tui) hint: String,
    pub(in crate::config_tui) dialogs: String,
}

/// 人格附属文件现值:防失忆提示(hints/<scope>.md)与预设对话
/// (dialogs/<scope>.md)。
pub(in crate::config_tui) fn persona_aux_values(
    paths: &MiyuPaths,
    config: &AppConfig,
    scope: &str,
) -> (String, String) {
    let hint = std::fs::read_to_string(miyu_core::persona_hint::manual_hint_path(
        config, paths, scope,
    ))
    .map(|text| text.trim().to_string())
    .unwrap_or_default();
    let dialogs = miyu_core::persona_hint::dialogs_raw(config, paths, scope);
    (hint, dialogs)
}

/// 附属文件落盘:非空写入,空则删除(清空提示=回到自动蒸馏,清空
/// 对话=不注入)。
pub(in crate::config_tui) fn write_persona_aux(
    paths: &MiyuPaths,
    config: &AppConfig,
    scope: &str,
    hint: &str,
    dialogs: &str,
) -> Result<()> {
    let targets = [
        (
            miyu_core::persona_hint::manual_hint_path(config, paths, scope),
            hint,
        ),
        (
            miyu_core::persona_hint::dialogs_path(config, paths, scope),
            dialogs,
        ),
    ];
    for (path, value) in targets {
        let value = value.trim();
        if value.is_empty() {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, format!("{value}\n"))?;
        }
    }
    Ok(())
}

pub(in crate::config_tui) fn persona_aux_fields(
    hint: String,
    dialogs: String,
    miyu: bool,
) -> Vec<Field> {
    let (hint_label, dialogs_label) = if miyu {
        (
            t(
                "Anti-amnesia reminder (empty = built-in default)",
                "防失忆提示(清空=恢复内置默认)",
            ),
            t(
                "Preset dialogs (Enter = list editor; empty = built-in default)",
                "预设对话(回车进列表编辑,清空=恢复内置默认)",
            ),
        )
    } else {
        (
            t(
                "Anti-amnesia reminder (empty = auto distill)",
                "防失忆提示(留空=自动蒸馏)",
            ),
            t(
                "Preset dialogs (Enter = list editor)",
                "预设对话(回车进列表编辑)",
            ),
        )
    };
    vec![
        Field::textarea(hint_label, hint),
        Field::dialog_list(dialogs_label, dialogs),
    ]
}

pub(in crate::config_tui) fn new_persona(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &AppConfig,
) -> Result<Option<String>> {
    let mut fields = vec![
        Field::new(t("Name", "名称"), String::new()),
        Field::textarea(t("Content", "内容"), String::new()),
    ];
    fields.extend(persona_aux_fields(String::new(), String::new(), false));
    if !run_form(ui, t(" NEW PERSONA ", " 新建人格 "), &mut fields)? {
        return Ok(None);
    }
    let name = sanitize_persona_name(&fields[0].value)?;
    ensure_persona_name_available(paths, config, &name, None)?;
    write_persona(paths, config, &name, &fields[1].value)?;
    write_persona_aux(
        paths,
        config,
        &miyu_base::config::persona_scope_name(&name),
        &fields[2].value,
        &fields[3].value,
    )?;
    Ok(Some(name))
}

pub(in crate::config_tui) fn edit_persona(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &AppConfig,
    current_name: &str,
) -> Result<Option<PersonaFormValues>> {
    let content = read_persona(paths, config, current_name)?;
    let (hint, dialogs) = persona_aux_values(
        paths,
        config,
        &miyu_base::config::persona_scope_name(current_name),
    );
    let mut fields = vec![
        Field::new(
            t("Name", "名称"),
            persona_display_name(current_name).to_string(),
        ),
        Field::textarea(t("Content", "内容"), content),
    ];
    fields.extend(persona_aux_fields(hint, dialogs, false));
    if !run_form(ui, t(" EDIT PERSONA ", " 编辑人格 "), &mut fields)? {
        return Ok(None);
    }
    let name = sanitize_persona_name(&fields[0].value)?;
    Ok(Some(PersonaFormValues {
        name,
        content: fields[1].value.clone(),
        hint: fields[2].value.clone(),
        dialogs: fields[3].value.clone(),
    }))
}

/// 默认 Miyu 人格:本体只读,回车只编辑附属的防失忆提示与预设对话
/// (scope 固定为 default)。
pub(in crate::config_tui) fn edit_miyu_persona_extras(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &AppConfig,
) -> Result<()> {
    let (hint, dialogs) = miyu_core::persona_hint::miyu_aux_prefill(config, paths);
    let mut fields = persona_aux_fields(hint, dialogs, true);
    if !run_form(ui, t(" MIYU EXTRAS ", " Miyu 人格附加 "), &mut fields)? {
        return Ok(());
    }
    write_persona_aux(paths, config, "default", &fields[0].value, &fields[1].value)
}

pub(in crate::config_tui) fn ensure_persona_name_available(
    paths: &MiyuPaths,
    config: &AppConfig,
    candidate: &str,
    current: Option<&str>,
) -> Result<()> {
    let candidate_scope = miyu_base::config::persona_scope_name(candidate);
    for existing in list_personas(paths, config)? {
        if current == Some(existing.as_str()) {
            continue;
        }
        if existing == candidate {
            bail!(
                "{}",
                t(
                    "A persona with this name already exists.",
                    "同名人格已存在。"
                )
            );
        }
        if miyu_base::config::persona_scope_name(&existing) == candidate_scope {
            bail!(
                "{}",
                t(
                    "This persona name conflicts with another persona's persistent scope.",
                    "该人格名称与另一个人格的持久化作用域冲突。",
                )
            );
        }
    }
    Ok(())
}

pub(in crate::config_tui) fn move_persona_scope(
    paths: &MiyuPaths,
    config: &AppConfig,
    old_name: &str,
    new_name: &str,
) -> Result<()> {
    if old_name == new_name
        || miyu_base::config::persona_scope_name(old_name)
            == miyu_base::config::persona_scope_name(new_name)
    {
        return Ok(());
    }
    let moves = [
        (
            config.persona_memory_data_dir(paths, old_name),
            config.persona_memory_data_dir(paths, new_name),
        ),
        (
            config.persona_memory_state_dir(paths, old_name),
            config.persona_memory_state_dir(paths, new_name),
        ),
        (
            config.persona_skills_dir(paths, old_name),
            config.persona_skills_dir(paths, new_name),
        ),
    ];
    if let Some((_, target)) = moves
        .iter()
        .find(|(source, target)| source.exists() && target.exists())
    {
        bail!(
            "persona scope destination already exists: {}",
            target.display()
        );
    }
    let mut completed = Vec::new();
    for (source, target) in moves {
        if let Err(error) = move_dir_if_exists(source.clone(), target.clone()) {
            for (from, to) in completed.into_iter().rev() {
                let _ = move_dir_if_exists(to, from);
            }
            return Err(error);
        }
        if target.exists() {
            completed.push((source, target));
        }
    }
    let old_scope = miyu_base::config::persona_scope_name(old_name);
    let new_scope = miyu_base::config::persona_scope_name(new_name);
    let file_moves = [
        (
            miyu_core::persona_hint::manual_hint_path(config, paths, &old_scope),
            miyu_core::persona_hint::manual_hint_path(config, paths, &new_scope),
        ),
        (
            miyu_core::persona_hint::dialogs_path(config, paths, &old_scope),
            miyu_core::persona_hint::dialogs_path(config, paths, &new_scope),
        ),
    ];
    for (source, target) in file_moves {
        if source.exists() && !target.exists() {
            std::fs::rename(&source, &target)?;
        }
    }
    Ok(())
}

pub(in crate::config_tui) fn remove_persona_scope(
    paths: &MiyuPaths,
    config: &AppConfig,
    name: &str,
) -> Result<()> {
    remove_dir_if_exists(config.persona_memory_data_dir(paths, name))?;
    remove_dir_if_exists(config.persona_memory_state_dir(paths, name))?;
    remove_dir_if_exists(config.persona_skills_dir(paths, name))?;
    let scope = miyu_base::config::persona_scope_name(name);
    for path in [
        miyu_core::persona_hint::manual_hint_path(config, paths, &scope),
        miyu_core::persona_hint::dialogs_path(config, paths, &scope),
    ] {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub(in crate::config_tui) fn move_dir_if_exists(from: PathBuf, to: PathBuf) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)?;
    Ok(())
}

pub(in crate::config_tui) fn remove_dir_if_exists(path: PathBuf) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

pub(in crate::config_tui) fn edit_identities(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
) -> Result<()> {
    std::fs::create_dir_all(config.identities_dir_path(paths))?;
    let mut selected = 0usize;
    loop {
        let identities = list_identities(paths, config)?;
        let mut options = Vec::with_capacity(identities.len() + 1);
        let default_marker = if config.prompt.active_identity.trim().is_empty() {
            "* "
        } else {
            "  "
        };
        options.push(format!(
            "{default_marker}{}",
            t("Do not use a user identity", "不使用用户身份")
        ));
        options.extend(identities.iter().map(|name| {
            let display = persona_display_name(name);
            if *name == config.prompt.active_identity {
                format!("* {display}")
            } else {
                format!("  {display}")
            }
        }));
        selected = selected.min(options.len().saturating_sub(1));
        draw_menu(
            ui,
            t(" USER IDENTITY ", " 用户身份 "),
            &options,
            selected,
            t(
                "[Tab]activate [Enter]edit [a]add [d]delete [j/k]move [q]back",
                "[Tab]激活 [Enter]编辑 [a]新增 [d]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key(ui)? {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Tab => {
                config.prompt.active_identity = if selected == 0 {
                    String::new()
                } else {
                    identities.get(selected - 1).cloned().unwrap_or_default()
                };
            }
            KeyCode::Char('a') => {
                if let Some(name) = new_identity(ui, paths, config)? {
                    config.prompt.active_identity = name;
                }
            }
            KeyCode::Enter if selected > 0 => {
                if let Some(name) = identities.get(selected - 1) {
                    if let Some(new_name) = edit_identity(ui, paths, config, name)? {
                        if config.prompt.active_identity == *name {
                            config.prompt.active_identity = new_name;
                        }
                    }
                }
            }
            KeyCode::Char('d') if selected > 0 => {
                if let Some(name) = identities.get(selected - 1) {
                    let path = config.identity_path(paths, name);
                    if path.exists() {
                        std::fs::remove_file(path)?;
                    }
                    if config.prompt.active_identity == *name {
                        config.prompt.active_identity.clear();
                    }
                    selected = selected.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn new_identity(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &AppConfig,
) -> Result<Option<String>> {
    edit_prompt_file_form(
        ui,
        t(" NEW IDENTITY ", " 新建用户身份 "),
        None,
        String::new(),
        |name, content| write_identity(paths, config, name, content),
    )
}

pub(in crate::config_tui) fn edit_identity(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &AppConfig,
    current_name: &str,
) -> Result<Option<String>> {
    let content = read_identity(paths, config, current_name)?;
    edit_prompt_file_form(
        ui,
        t(" EDIT IDENTITY ", " 编辑用户身份 "),
        Some(current_name),
        content,
        |name, content| {
            if name != current_name {
                let old_path = config.identity_path(paths, current_name);
                if old_path.exists() {
                    std::fs::remove_file(old_path)?;
                }
            }
            write_identity(paths, config, name, content)
        },
    )
}

pub(in crate::config_tui) fn list_identities(
    paths: &MiyuPaths,
    config: &AppConfig,
) -> Result<Vec<String>> {
    list_markdown_files(&config.identities_dir_path(paths))
}

pub(in crate::config_tui) fn read_identity(
    paths: &MiyuPaths,
    config: &AppConfig,
    name: &str,
) -> Result<String> {
    let path = config.identity_path(paths, name);
    if path.exists() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

pub(in crate::config_tui) fn write_identity(
    paths: &MiyuPaths,
    config: &AppConfig,
    name: &str,
    content: &str,
) -> Result<()> {
    let path = config.identity_path(paths, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format_text_file(content))?;
    Ok(())
}

pub(in crate::config_tui) fn edit_prompt_file_form<F>(
    ui: &mut Ui,
    title: &str,
    current_name: Option<&str>,
    content: String,
    write: F,
) -> Result<Option<String>>
where
    F: FnOnce(&str, &str) -> Result<()>,
{
    let Some((name, content)) = edit_prompt_file_values(ui, title, current_name, content)? else {
        return Ok(None);
    };
    write(&name, &content)?;
    Ok(Some(name))
}

pub(in crate::config_tui) fn edit_prompt_file_values(
    ui: &mut Ui,
    title: &str,
    current_name: Option<&str>,
    content: String,
) -> Result<Option<(String, String)>> {
    let mut fields = vec![
        Field::new(
            t("Name", "名称"),
            current_name
                .map(persona_display_name)
                .unwrap_or("")
                .to_string(),
        ),
        Field::textarea(t("Content", "内容"), content),
    ];
    if !run_form(ui, title, &mut fields)? {
        return Ok(None);
    }
    let name = sanitize_persona_name(&fields[0].value)?;
    Ok(Some((name, fields[1].value.clone())))
}

pub(in crate::config_tui) fn list_personas(
    paths: &MiyuPaths,
    config: &AppConfig,
) -> Result<Vec<String>> {
    let mut names = list_markdown_files(&config.prompts_dir_path(paths))?;
    names.retain(|name| !name.eq_ignore_ascii_case("system-prompt.md"));
    Ok(names)
}

pub(in crate::config_tui) fn list_markdown_files(dir: &std::path::Path) -> Result<Vec<String>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".md") {
                names.push(name);
            }
        }
    }
    names.sort();
    Ok(names)
}

pub(in crate::config_tui) fn read_persona(
    paths: &MiyuPaths,
    config: &AppConfig,
    name: &str,
) -> Result<String> {
    let path = config.persona_path(paths, name);
    if path.exists() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

pub(in crate::config_tui) fn write_persona(
    paths: &MiyuPaths,
    config: &AppConfig,
    name: &str,
    content: &str,
) -> Result<()> {
    let path = config.persona_path(paths, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format_text_file(content))?;
    Ok(())
}

pub(in crate::config_tui) fn sanitize_persona_name(value: &str) -> Result<String> {
    let mut name = value
        .trim()
        .trim_end_matches(".md")
        .replace(['/', '\\'], "-");
    if name.is_empty() {
        bail!("{}", t("Persona name cannot be empty", "人格名称不能为空"));
    }
    name.push_str(".md");
    if name.eq_ignore_ascii_case("system-prompt.md") {
        bail!(
            "{}",
            t(
                "system-prompt.md is reserved",
                "system-prompt.md 是保留文件名"
            )
        );
    }
    // "dev" 是开发模式的保留人格(记忆/技能命名空间挂其名下);同名
    // 用户人格会与 dev 模式共享记忆库,必须挡在创建入口。
    if persona_display_name(&name).eq_ignore_ascii_case(miyu_core::state::DEV_PERSONA) {
        bail!(
            "{}",
            t(
                "\"dev\" is reserved for dev mode",
                "\"dev\" 是开发模式的保留名"
            )
        );
    }
    Ok(name)
}

pub(in crate::config_tui) fn persona_display_name(name: &str) -> &str {
    name.strip_suffix(".md").unwrap_or(name)
}
