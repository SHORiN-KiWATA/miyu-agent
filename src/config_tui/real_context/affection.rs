//! 好感度相关设置。
//!
//! 分值与提示词分开编辑：调分值是常事，改提示词是罕事，混在一个表单里会让常用
//! 项被埋掉。

use crate::config_tui::*;

pub(in crate::config_tui) fn edit_real_context_affection(
    ui: &mut Ui,
    _config: &AppConfig,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = [
            format!(
                "{}: {}",
                t("Affection system", "好感度系统"),
                boolean_label(settings.affection_enable)
            ),
            format!(
                "{}: {}",
                t(
                    "Judge affection changes after replies",
                    "回复后判断好感度变化",
                ),
                boolean_label(settings.affection_update_enable)
            ),
            t("Score and limits", "分值与限制").to_string(),
            t("Relationship prompts", "关系提示词").to_string(),
            format!(
                "{}: {}",
                t("Top-tier QQ IDs", "允许到达最高挡位的 QQ 号"),
                settings.affection_unlimited_user_ids.len()
            ),
        ];
        draw_menu(
            ui,
            t(" AFFECTION AND RELATIONSHIP ", " 好感度与关系 "),
            &options,
            selected,
            "",
        )?;
        match read_key(ui)? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => match selected {
                0 => {
                    settings.affection_enable = select_bool(
                        ui,
                        t("Affection system", "好感度系统"),
                        settings.affection_enable,
                    )?;
                }
                1 => {
                    settings.affection_update_enable = select_bool(
                        ui,
                        t(
                            "Judge affection changes after replies",
                            "回复后判断好感度变化",
                        ),
                        settings.affection_update_enable,
                    )?;
                }
                2 => edit_real_context_affection_values(ui, settings)?,
                3 => edit_real_context_affection_prompts(ui, settings)?,
                4 => {
                    let mut raw = settings
                        .affection_unlimited_user_ids
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join("\n");
                    edit_textarea(ui, &mut raw)?;
                    match parse_id_list(&raw) {
                        Ok(ids) => settings.affection_unlimited_user_ids = ids,
                        Err(error) => message(ui, &error.to_string())?,
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn edit_real_context_affection_values(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::new(
                t("Initial score", "首次互动默认好感度"),
                settings.affection_initial_score.to_string(),
            ),
            Field::new(
                t("Minimum score", "好感度下限"),
                settings.affection_min_score.to_string(),
            ),
            Field::new(
                t("Global maximum score", "全局最高好感度"),
                settings.affection_max_score.to_string(),
            ),
            Field::new(
                t("Regular-user maximum", "普通用户最高好感度"),
                settings.affection_regular_max_score.to_string(),
            ),
            Field::new(
                t("Reply bias minimum", "主动回复最低加值"),
                settings.affection_bias_min.to_string(),
            ),
            Field::new(
                t("Reply bias maximum", "主动回复最高加值"),
                settings.affection_bias_max.to_string(),
            ),
            Field::new(
                t("Gain pivot", "好感增益拐点"),
                settings.affection_gain_pivot.to_string(),
            ),
            Field::new(
                t("Delta scale", "好感变化倍率"),
                settings.affection_delta_scale.to_string(),
            ),
            Field::new(
                t("Single-change minimum", "单次变化下限"),
                settings.affection_delta_min.to_string(),
            ),
            Field::new(
                t("Single-change maximum", "单次变化上限"),
                settings.affection_delta_max.to_string(),
            ),
            Field::new(
                t("Confidence threshold", "变化置信度阈值"),
                settings.affection_update_confidence_threshold.to_string(),
            ),
            Field::new(
                t(
                    "Daily gain limit (0 = unlimited)",
                    "单日正向上限（0 = 不限）",
                ),
                settings.affection_daily_gain_limit.to_string(),
            ),
            Field::new(
                t(
                    "Daily loss limit (0 = unlimited)",
                    "单日负向上限（0 = 不限）",
                ),
                settings.affection_daily_loss_limit.to_string(),
            ),
            Field::boolean(
                t("Automatic tags", "自动标签"),
                settings.affection_auto_tag_enable,
            ),
            Field::new(
                t("Maximum tags (0 = unlimited)", "标签上限（0 = 不限）"),
                settings.affection_max_tags.to_string(),
            ),
            Field::new(
                t("Recent events in prompt", "注入提示词的近期变化条数"),
                settings.affection_recent_events_for_prompt.to_string(),
            ),
            Field::new(
                t(
                    "Update timeout (seconds; 0 = unlimited)",
                    "更新超时（秒；0 = 不限）",
                ),
                settings.affection_update_timeout_seconds.to_string(),
            ),
        ];
        if !run_edit_form(
            ui,
            t(" AFFECTION SCORE AND LIMITS ", " 好感度分值与限制 "),
            &mut fields,
        )? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.affection_initial_score = real_context_value(&fields, 0)?;
            candidate.affection_min_score = real_context_value(&fields, 1)?;
            candidate.affection_max_score = real_context_value(&fields, 2)?;
            candidate.affection_regular_max_score = real_context_value(&fields, 3)?;
            candidate.affection_bias_min = real_context_value(&fields, 4)?;
            candidate.affection_bias_max = real_context_value(&fields, 5)?;
            candidate.affection_gain_pivot = real_context_value(&fields, 6)?;
            candidate.affection_delta_scale = real_context_value(&fields, 7)?;
            candidate.affection_delta_min = real_context_value(&fields, 8)?;
            candidate.affection_delta_max = real_context_value(&fields, 9)?;
            candidate.affection_update_confidence_threshold = real_context_value(&fields, 10)?;
            candidate.affection_daily_gain_limit = real_context_value(&fields, 11)?;
            candidate.affection_daily_loss_limit = real_context_value(&fields, 12)?;
            candidate.affection_auto_tag_enable = real_context_bool(&fields, 13)?;
            candidate.affection_max_tags = real_context_value(&fields, 14)?;
            candidate.affection_recent_events_for_prompt = real_context_value(&fields, 15)?;
            candidate.affection_update_timeout_seconds = real_context_value(&fields, 16)?;
            candidate.validate().map_err(|error| error.to_string())
        })();
        match parsed {
            Ok(()) => {
                *settings = candidate;
                return Ok(());
            }
            Err(error) => message(ui, &error)?,
        }
    }
}

pub(in crate::config_tui) fn edit_real_context_affection_prompts(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let prompts = [
        (
            t("Estranged", "刻意疏远"),
            &mut settings.affection_prompt_estranged,
        ),
        (t("Cold", "冷漠"), &mut settings.affection_prompt_cold),
        (t("Neutral", "中立"), &mut settings.affection_prompt_neutral),
        (t("Known", "认识"), &mut settings.affection_prompt_known),
        (t("Friend", "好友"), &mut settings.affection_prompt_friend),
        (t("Trusted", "信任"), &mut settings.affection_prompt_trusted),
        (t("Close", "亲近"), &mut settings.affection_prompt_close),
    ];
    let mut selected = 0usize;
    loop {
        let options = prompts
            .iter()
            .map(|(label, value)| {
                format!(
                    "{label}: {}",
                    if value.is_empty() {
                        t("unset", "未设置")
                    } else {
                        t("set", "已设置")
                    }
                )
            })
            .collect::<Vec<_>>();
        draw_menu(
            ui,
            t(" AFFECTION RELATIONSHIP PROMPTS ", " 好感度关系提示词 "),
            &options,
            selected,
            "",
        )?;
        match read_key(ui)? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter => edit_textarea(ui, prompts[selected].1)?,
            _ => {}
        }
    }
}
