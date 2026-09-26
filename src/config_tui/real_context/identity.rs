//! 身份映射的编辑。
//!
//! 把平台 ID 对应到「这是谁」。按行解析（`parse_real_context_identity_lines`），
//! 因为用户是整段粘进来的，不是一条条填的。

use crate::config_tui::*;

pub(in crate::config_tui) fn edit_real_context_identities(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let mut options = vec![
            t("+ Add one", "+ 新增一项").to_string(),
            t("+ Add multiple", "+ 批量新增").to_string(),
        ];
        options.extend(
            settings
                .identity_mappings
                .iter()
                .map(|mapping| format!("{} -> {}", mapping.nickname, mapping.user_id)),
        );
        selected = selected.min(options.len() - 1);
        draw_menu(
            ui,
            t(" IDENTITY MAPPINGS ", " 识人映射 "),
            &options,
            selected,
            t(
                "[Enter]configure [Delete]remove [j/k]move [q]back",
                "[Enter]配置 [Delete]删除 [j/k]移动 [q]返回",
            ),
        )?;
        match read_key(ui)? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => {
                if let Some(mapping) = prompt_real_context_identity(ui, None)? {
                    upsert_real_context_identity(&mut settings.identity_mappings, mapping);
                }
            }
            KeyCode::Enter if selected == 1 => {
                let mut raw = format!(
                    "# {}",
                    t(
                        "one per line: nickname<Tab>QQ-id",
                        "每行一项：昵称<Tab>QQ号"
                    )
                );
                edit_textarea(ui, &mut raw)?;
                match parse_real_context_identity_lines(&raw) {
                    Ok(mappings) => {
                        for mapping in mappings {
                            upsert_real_context_identity(&mut settings.identity_mappings, mapping);
                        }
                    }
                    Err(error) => message(ui, &error)?,
                }
            }
            KeyCode::Enter => {
                let index = selected - 2;
                if let Some(mapping) = prompt_real_context_identity(
                    ui,
                    settings.identity_mappings.get(index).cloned(),
                )? {
                    if settings
                        .identity_mappings
                        .iter()
                        .enumerate()
                        .any(|(other, item)| other != index && item.nickname == mapping.nickname)
                    {
                        message(ui, t("That nickname already exists.", "该昵称已存在。"))?;
                    } else if let Some(item) = settings.identity_mappings.get_mut(index) {
                        *item = mapping;
                    }
                }
            }
            KeyCode::Delete | KeyCode::Backspace if selected >= 2 => {
                settings.identity_mappings.remove(selected - 2);
                selected = selected.min(settings.identity_mappings.len() + 1);
            }
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn prompt_real_context_identity(
    ui: &mut Ui,
    current: Option<RealContextIdentityMapping>,
) -> Result<Option<RealContextIdentityMapping>> {
    let existing = current.is_some();
    let mut fields = vec![
        Field::new(
            t("Protected nickname", "受保护昵称"),
            current
                .as_ref()
                .map(|mapping| mapping.nickname.clone())
                .unwrap_or_default(),
        ),
        Field::new(
            t("Expected QQ id", "对应 QQ 号"),
            current
                .as_ref()
                .map(|mapping| mapping.user_id.to_string())
                .unwrap_or_default(),
        ),
    ];
    if !run_item_form(
        ui,
        t(" IDENTITY MAPPING ", " 编辑识人映射 "),
        &mut fields,
        existing,
    )? {
        return Ok(None);
    }
    let nickname = fields[0].value.trim();
    if nickname.is_empty()
        || nickname.chars().count() > 128
        || nickname.chars().any(char::is_control)
    {
        message(
            ui,
            t(
                "Nickname must be 1-128 characters without control characters.",
                "昵称必须为 1 到 128 个字符，且不能包含控制字符。",
            ),
        )?;
        return Ok(None);
    }
    let user_id = match parse_positive_id(&fields[1].value) {
        Ok(user_id) => user_id,
        Err(error) => {
            message(ui, &error)?;
            return Ok(None);
        }
    };
    Ok(Some(RealContextIdentityMapping {
        nickname: nickname.to_string(),
        user_id,
    }))
}

pub(in crate::config_tui) fn upsert_real_context_identity(
    mappings: &mut Vec<RealContextIdentityMapping>,
    mapping: RealContextIdentityMapping,
) {
    if let Some(existing) = mappings
        .iter_mut()
        .find(|existing| existing.nickname == mapping.nickname)
    {
        *existing = mapping;
    } else {
        mappings.push(mapping);
    }
}

pub(in crate::config_tui) fn parse_real_context_identity_lines(
    raw: &str,
) -> std::result::Result<Vec<RealContextIdentityMapping>, String> {
    let mut mappings = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((nickname, user_id)) = line.rsplit_once('\t').or_else(|| line.rsplit_once('='))
        else {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("use nickname<Tab>QQ-id", "请使用 昵称<Tab>QQ号 格式")
            ));
        };
        let nickname = nickname.trim();
        if nickname.is_empty()
            || nickname.chars().count() > 128
            || nickname.chars().any(char::is_control)
        {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("invalid nickname", "昵称无效")
            ));
        }
        let user_id = parse_positive_id(user_id)?;
        if mappings
            .iter()
            .any(|mapping: &RealContextIdentityMapping| mapping.nickname == nickname)
        {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("duplicate nickname", "昵称重复")
            ));
        }
        mappings.push(RealContextIdentityMapping {
            nickname: nickname.to_string(),
            user_id,
        });
    }
    Ok(mappings)
}

pub(in crate::config_tui) fn edit_real_context_string_lines(
    ui: &mut Ui,
    _title: &'static str,
    values: &mut Vec<String>,
    maximum_chars: usize,
) -> Result<()> {
    let mut raw = values.join("\n");
    edit_textarea(ui, &mut raw)?;
    match parse_real_context_string_lines(&raw, maximum_chars) {
        Ok(parsed) => *values = parsed,
        Err(error) => message(ui, &error)?,
    }
    Ok(())
}

pub(in crate::config_tui) fn parse_real_context_string_lines(
    raw: &str,
    maximum_chars: usize,
) -> std::result::Result<Vec<String>, String> {
    let mut values = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let value = line.trim();
        if value.is_empty() {
            continue;
        }
        if value.chars().count() > maximum_chars || value.chars().any(char::is_control) {
            return Err(format!(
                "{} {}: {}",
                t("Line", "第"),
                index + 1,
                t("value is invalid or too long", "内容无效或过长")
            ));
        }
        if !values.iter().any(|existing| existing == value) {
            values.push(value.to_string());
        }
    }
    Ok(values)
}
