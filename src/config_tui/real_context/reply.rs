//! 「什么时候回、回给谁」这一组设置。
//!
//! 主动回复、判定进阶、话题延续、回复目标、内容审核——它们共同决定插话行为，
//! 但彼此没有依赖，所以是一组独立的小编辑器而不是一个巨型表单。

use crate::config_tui::*;

pub(in crate::config_tui) fn edit_real_context_active_reply(
    ui: &mut Ui,
    state: &StateStore,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let skip_list_summary = active_judgement_skip_ids(state)
            .map(|ids| ids.len().to_string())
            .unwrap_or_else(|_| t("unavailable", "不可用").to_string());
        let options = vec![
            format!(
                "{}: {}",
                t("Scoring and restraint", "评分与克制"),
                boolean_label(settings.active_reply_enable)
            ),
            format!(
                "{}: {}",
                t("Inherit persona during judgement", "判断时继承人格"),
                boolean_label(settings.judge_include_persona)
            ),
            format!(
                "{}: {}",
                t("Custom prompt", "自定义提示词"),
                if settings.judge_persona_prompt.trim().is_empty() {
                    t("none", "未设置")
                } else {
                    t("set", "已设置")
                }
            ),
            format!(
                "{}: {}",
                t("Random judgement probability", "随机进入判断的概率"),
                settings.active_judge_probability
            ),
            format!(
                "{}: {}",
                t("Reply threshold", "回复阈值"),
                settings.reply_threshold
            ),
            format!(
                "{}: {}",
                t("Skip image-only messages", "跳过纯图片消息"),
                boolean_label(settings.skip_pure_image_active_judge)
            ),
            format!(
                "{}: {}",
                t("QQ ids that skip active judgement", "跳过主动判断的 QQ 号"),
                skip_list_summary
            ),
            format!(
                "{}: {}",
                t(
                    "New message supersedes pending judgement",
                    "新消息覆盖待判断消息",
                ),
                boolean_label(settings.active_reply_supersede_enable)
            ),
            format!(
                "{}: {}",
                t("Supersede window (seconds)", "覆盖窗口（秒）"),
                settings.active_reply_supersede_window_seconds
            ),
            format!(
                "{}: {}",
                t("Reply restraint", "回复克制"),
                boolean_label(settings.reply_restraint_enable)
            ),
            format!(
                "{}: {}",
                t(
                    "Post-reply observation window (seconds)",
                    "群聊发完消息后观察窗口（秒）"
                ),
                settings.after_speaking_window_seconds
            ),
            format!(
                "{}: {}",
                t("Post-reply score bonus", "群聊发完消息后观察窗口加分"),
                settings.after_speaking_score_boost
            ),
            t("Continuation window", "续聊窗口").to_string(),
            t("Trigger methods", "触发方式").to_string(),
            t("Concurrency and weights", "并发与权重").to_string(),
            format!(
                "{}: {}",
                t("Judge context window", "判断上下文消息数"),
                settings.judge_context_window
            ),
        ];
        draw_menu(
            ui,
            t(" ACTIVE REPLY JUDGEMENT ", " 主动回复判断 "),
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
                    settings.active_reply_enable = select_bool(
                        ui,
                        t("Scoring and restraint", "评分与克制"),
                        settings.active_reply_enable,
                    )?
                }
                1 => {
                    settings.judge_include_persona = select_bool(
                        ui,
                        t("Inherit persona during judgement", "判断时继承人格"),
                        settings.judge_include_persona,
                    )?
                }
                2 => edit_textarea(ui, &mut settings.judge_persona_prompt)?,
                3 => edit_real_context_number(
                    ui,
                    t("Random judgement probability", "随机进入判断的概率"),
                    settings.active_judge_probability,
                    settings,
                    |candidate, value| candidate.active_judge_probability = value,
                )?,
                4 => edit_real_context_number(
                    ui,
                    t("Reply threshold", "回复阈值"),
                    settings.reply_threshold,
                    settings,
                    |candidate, value| candidate.reply_threshold = value,
                )?,
                5 => {
                    settings.skip_pure_image_active_judge = select_bool(
                        ui,
                        t("Skip image-only messages", "跳过纯图片消息"),
                        settings.skip_pure_image_active_judge,
                    )?
                }
                6 => {
                    edit_active_judgement_skip_ids(ui, state)?;
                }
                7 => {
                    settings.active_reply_supersede_enable = select_bool(
                        ui,
                        t(
                            "New message supersedes pending judgement",
                            "新消息覆盖待判断消息",
                        ),
                        settings.active_reply_supersede_enable,
                    )?
                }
                8 => edit_real_context_number(
                    ui,
                    t("Supersede window (seconds)", "覆盖窗口（秒）"),
                    settings.active_reply_supersede_window_seconds,
                    settings,
                    |candidate, value| candidate.active_reply_supersede_window_seconds = value,
                )?,
                9 => {
                    settings.reply_restraint_enable = select_bool(
                        ui,
                        t("Reply restraint", "回复克制"),
                        settings.reply_restraint_enable,
                    )?
                }
                10 => edit_real_context_number(
                    ui,
                    t(
                        "Post-reply observation window (seconds)",
                        "群聊发完消息后观察窗口（秒）",
                    ),
                    settings.after_speaking_window_seconds,
                    settings,
                    |candidate, value| candidate.after_speaking_window_seconds = value,
                )?,
                11 => edit_real_context_number(
                    ui,
                    t("Post-reply score bonus", "群聊发完消息后观察窗口加分"),
                    settings.after_speaking_score_boost,
                    settings,
                    |candidate, value| candidate.after_speaking_score_boost = value,
                )?,
                12 => edit_real_context_continuation(ui, settings)?,
                13 => edit_real_context_triggers(ui, settings)?,
                14 => edit_real_context_judge_advanced(ui, settings)?,
                15 => edit_real_context_number(
                    ui,
                    t("Judge context window", "判断上下文消息数"),
                    settings.judge_context_window,
                    settings,
                    |candidate, value| candidate.judge_context_window = value,
                )?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn edit_active_judgement_skip_ids(
    ui: &mut Ui,
    state: &StateStore,
) -> Result<()> {
    let original = match active_judgement_skip_ids(state) {
        Ok(ids) => ids,
        Err(error) => {
            message(
                ui,
                &format!(
                    "{}: {error}",
                    t(
                        "Unable to read the active judgement skip list",
                        "无法读取主动判断跳过名单"
                    )
                ),
            )?;
            return Ok(());
        }
    };
    let mut edited = original.clone();
    edit_qq_id_list(
        ui,
        t(" ACTIVE JUDGEMENT SKIP QQ IDS ", " 跳过主动判断的 QQ 号 "),
        t("QQ id", "QQ 号"),
        &mut edited,
    )?;
    if let Err(error) = apply_active_judgement_skip_editor_changes(state, &original, &edited) {
        message(
            ui,
            &format!(
                "{}: {error}",
                t(
                    "Unable to update the active judgement skip list",
                    "无法更新主动判断跳过名单"
                )
            ),
        )?;
    }
    Ok(())
}

pub(in crate::config_tui) fn edit_real_context_judge_advanced(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::new(
                t("Timeout (seconds)", "判断超时（秒）"),
                settings.judge_timeout_seconds.to_string(),
            ),
            Field::new(
                t("Endpoint timeout (seconds)", "单模型超时（秒）"),
                settings.judge_endpoint_timeout_seconds.to_string(),
            ),
            Field::new(
                t(
                    "Global concurrency wait timeout (seconds)",
                    "全局判断并发等待超时（秒）",
                ),
                settings.judge_queue_wait_timeout_seconds.to_string(),
            ),
            Field::new(
                t("Maximum concurrency", "最大并发数"),
                settings.judge_max_concurrency.to_string(),
            ),
            Field::new(
                t("Maximum retries", "最大重试次数"),
                settings.judge_max_retries.to_string(),
            ),
            Field::new(
                t("Relevance weight", "相关性权重"),
                settings.judge_relevance_weight.to_string(),
            ),
            Field::new(
                t("Willingness weight", "意愿权重"),
                settings.judge_willingness_weight.to_string(),
            ),
            Field::new(
                t("Social weight", "社交适合度权重"),
                settings.judge_social_weight.to_string(),
            ),
            Field::new(
                t("Timing weight", "时机权重"),
                settings.judge_timing_weight.to_string(),
            ),
            Field::new(
                t("Continuity weight", "连续性权重"),
                settings.judge_continuity_weight.to_string(),
            ),
            Field::boolean(
                t("Use judgement recommendation", "采用判断建议加减分"),
                settings.judge_should_reply_adjust_enable,
            ),
            Field::new(
                t("Recommended-reply boost", "建议回复加分"),
                settings.judge_should_reply_boost_score.to_string(),
            ),
            Field::new(
                t("Recommended-silence penalty", "建议不回复减分"),
                settings.judge_should_reply_penalty_score.to_string(),
            ),
        ];
        if !run_edit_form(
            ui,
            t(" JUDGEMENT ADVANCED ", " 主动判断高级设置 "),
            &mut fields,
        )? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.judge_timeout_seconds = real_context_value(&fields, 0)?;
            candidate.judge_endpoint_timeout_seconds = real_context_value(&fields, 1)?;
            candidate.judge_queue_wait_timeout_seconds = real_context_value(&fields, 2)?;
            candidate.judge_max_concurrency = real_context_value(&fields, 3)?;
            candidate.judge_max_retries = real_context_value(&fields, 4)?;
            candidate.judge_relevance_weight = real_context_value(&fields, 5)?;
            candidate.judge_willingness_weight = real_context_value(&fields, 6)?;
            candidate.judge_social_weight = real_context_value(&fields, 7)?;
            candidate.judge_timing_weight = real_context_value(&fields, 8)?;
            candidate.judge_continuity_weight = real_context_value(&fields, 9)?;
            candidate.judge_should_reply_adjust_enable = real_context_bool(&fields, 10)?;
            candidate.judge_should_reply_boost_score = real_context_value(&fields, 11)?;
            candidate.judge_should_reply_penalty_score = real_context_value(&fields, 12)?;
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

pub(in crate::config_tui) fn edit_real_context_triggers(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::boolean(
                t("Take over direct triggers", "接管直接触发"),
                settings.takeover_direct_trigger_enable,
            ),
            Field::new(
                t("Direct-trigger boost", "直接触发加分"),
                settings.takeover_direct_trigger_boost_score.to_string(),
            ),
            Field::boolean(
                t(
                    "Privileged users skip group active judgement",
                    "管理员和私聊白名单跳过群聊主动回复判断",
                ),
                settings.privileged_direct_trigger_skip_active_judgement,
            ),
        ];
        if !run_edit_form(ui, t(" TRIGGER METHODS ", " 触发方式 "), &mut fields)? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.takeover_direct_trigger_enable = real_context_bool(&fields, 0)?;
            candidate.takeover_direct_trigger_boost_score = real_context_value(&fields, 1)?;
            candidate.privileged_direct_trigger_skip_active_judgement =
                real_context_bool(&fields, 2)?;
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

pub(in crate::config_tui) fn edit_real_context_continuation(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    loop {
        let mut fields = vec![
            Field::boolean(
                t("Natural continuation", "自然续聊"),
                settings.continuation_enable,
            ),
            Field::new(
                t("Continuation window (seconds)", "续聊窗口（秒）"),
                settings.continuation_window_seconds.to_string(),
            ),
            Field::new(
                t("Continuation boost", "续聊加分"),
                settings.continuation_boost_score.to_string(),
            ),
        ];
        if !run_edit_form(ui, t(" CONTINUATION WINDOW ", " 续聊窗口 "), &mut fields)? {
            return Ok(());
        }
        let mut candidate = settings.clone();
        let parsed = (|| -> std::result::Result<(), String> {
            candidate.continuation_enable = real_context_bool(&fields, 0)?;
            candidate.continuation_window_seconds = real_context_value(&fields, 1)?;
            candidate.continuation_boost_score = real_context_value(&fields, 2)?;
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

pub(in crate::config_tui) fn edit_real_context_reply_target(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = vec![
            format!(
                "{}: {}",
                t("Target the replied-to user", "定向回复对象"),
                boolean_label(settings.reply_target_enable)
            ),
            format!(
                "{}: {}",
                t("Quote target message", "引用目标消息"),
                boolean_label(settings.reply_target_quote_enable)
            ),
            format!(
                "{}: {}",
                t(
                    "Quote after intervening messages from others",
                    "和原消息间隔几条消息则引用"
                ),
                settings.reply_target_quote_after_other_messages
            ),
            format!(
                "{}: {}",
                t("Mention target user", "艾特目标用户"),
                boolean_label(settings.reply_target_mention_enable)
            ),
            format!(
                "{}: {}",
                t("Mention after elapsed seconds", "回复时间超过多少秒则艾特"),
                settings.reply_target_mention_after_seconds
            ),
            format!(
                "{}: {}",
                t(
                    "React after an active reply is accepted",
                    "确认主动回复后贴表情"
                ),
                boolean_label(settings.active_reply_reaction_enable)
            ),
            format!(
                "{}: {}",
                t("Active-reply reaction id", "主动回复贴的表情ID"),
                settings
                    .active_reply_reaction_emoji_ids
                    .first()
                    .copied()
                    .unwrap_or_default()
            ),
            format!(
                "{}: {}",
                t("Reaction cleanup timeout (seconds)", "表情清理超时（秒）"),
                settings.active_reply_reaction_timeout_seconds
            ),
        ];
        draw_menu(
            ui,
            t(" QUOTE, MENTION, AND REACTIONS ", " 引用艾特和贴表情 "),
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
                    settings.reply_target_enable = select_bool(
                        ui,
                        t("Target the replied-to user", "定向回复对象"),
                        settings.reply_target_enable,
                    )?
                }
                1 => {
                    settings.reply_target_quote_enable = select_bool(
                        ui,
                        t("Quote target message", "引用目标消息"),
                        settings.reply_target_quote_enable,
                    )?
                }
                2 => edit_real_context_number(
                    ui,
                    t(
                        "Quote after intervening messages from others",
                        "和原消息间隔几条消息则引用",
                    ),
                    settings.reply_target_quote_after_other_messages,
                    settings,
                    |candidate, value| candidate.reply_target_quote_after_other_messages = value,
                )?,
                3 => {
                    settings.reply_target_mention_enable = select_bool(
                        ui,
                        t("Mention target user", "艾特目标用户"),
                        settings.reply_target_mention_enable,
                    )?
                }
                4 => edit_real_context_number(
                    ui,
                    t("Mention after elapsed seconds", "回复时间超过多少秒则艾特"),
                    settings.reply_target_mention_after_seconds,
                    settings,
                    |candidate, value| candidate.reply_target_mention_after_seconds = value,
                )?,
                5 => {
                    settings.active_reply_reaction_enable = select_bool(
                        ui,
                        t(
                            "React after an active reply is accepted",
                            "确认主动回复后贴表情",
                        ),
                        settings.active_reply_reaction_enable,
                    )?
                }
                6 => {
                    let current = settings
                        .active_reply_reaction_emoji_ids
                        .first()
                        .copied()
                        .unwrap_or_default();
                    edit_real_context_number(
                        ui,
                        t("Active-reply reaction id", "主动回复贴的表情ID"),
                        current,
                        settings,
                        |candidate, value| candidate.active_reply_reaction_emoji_ids = vec![value],
                    )?;
                }
                7 => edit_real_context_number(
                    ui,
                    t("Reaction cleanup timeout (seconds)", "表情清理超时（秒）"),
                    settings.active_reply_reaction_timeout_seconds,
                    settings,
                    |candidate, value| candidate.active_reply_reaction_timeout_seconds = value,
                )?,
                _ => {}
            },
            _ => {}
        }
    }
}

pub(in crate::config_tui) fn edit_real_context_moderation(
    ui: &mut Ui,
    settings: &mut RealContextPluginSettings,
) -> Result<()> {
    let mut selected = 0usize;
    loop {
        let options = vec![
            format!(
                "{}: {}",
                t("Moderation", "违规判断"),
                boolean_label(settings.moderation_enable)
            ),
            format!(
                "{}: {}",
                t("Keyword precheck", "关键词触发初判"),
                boolean_label(settings.moderation_keyword_trigger_enable)
            ),
            format!(
                "{}: {}",
                t("Moderation keywords", "违规初判关键词"),
                settings.moderation_keywords.len()
            ),
            format!(
                "{}: {}",
                t("Moderation rules prompt", "违规规则提示词"),
                if settings.moderation_custom_rules.is_empty() {
                    t("none", "未设置")
                } else {
                    t("set", "已设置")
                }
            ),
            format!(
                "{}: {}",
                t("Minimum severity", "判断违规的阈值"),
                settings.moderation_min_severity
            ),
            format!(
                "{}: {}",
                t("Moderation timeout (seconds)", "违规判断超时"),
                settings.moderation_timeout_seconds
            ),
            format!(
                "{}: {}",
                t("Decode Base64 text", "Base64 违规初判"),
                boolean_label(settings.base64_moderation_enable)
            ),
            format!(
                "{}: {}",
                t("Minimum Base64 length", "Base64 最短长度"),
                settings.base64_moderation_min_chars
            ),
            format!(
                "{}: {}",
                t("Maximum decoded characters", "Base64 最大解码字符数"),
                settings.base64_moderation_max_decoded_chars
            ),
            format!(
                "{}: {}",
                t("Minimum printable ratio", "Base64 最低可打印比例"),
                settings.base64_moderation_min_printable_ratio
            ),
        ];
        draw_menu(
            ui,
            t(" SAFETY CHECKS ", " 违规判断 "),
            &options,
            selected,
            "",
        )?;
        match read_key(ui)? {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(options.len() - 1),
            KeyCode::Enter if selected == 0 => {
                settings.moderation_enable =
                    select_bool(ui, t("Moderation", "违规判断"), settings.moderation_enable)?
            }
            KeyCode::Enter if selected == 1 => {
                settings.moderation_keyword_trigger_enable = select_bool(
                    ui,
                    t("Keyword precheck", "关键词触发初判"),
                    settings.moderation_keyword_trigger_enable,
                )?
            }
            KeyCode::Enter if selected == 2 => edit_real_context_string_lines(
                ui,
                t(" MODERATION KEYWORDS ", " 违规初判关键词 "),
                &mut settings.moderation_keywords,
                256,
            )?,
            KeyCode::Enter if selected == 3 => {
                edit_textarea(ui, &mut settings.moderation_custom_rules)?
            }
            KeyCode::Enter if selected == 4 => edit_real_context_number(
                ui,
                t("Minimum severity", "判断违规的阈值"),
                settings.moderation_min_severity,
                settings,
                |candidate, value| candidate.moderation_min_severity = value,
            )?,
            KeyCode::Enter if selected == 5 => edit_real_context_number(
                ui,
                t("Moderation timeout (seconds)", "违规判断超时"),
                settings.moderation_timeout_seconds,
                settings,
                |candidate, value| candidate.moderation_timeout_seconds = value,
            )?,
            KeyCode::Enter if selected == 6 => {
                settings.base64_moderation_enable = select_bool(
                    ui,
                    t("Decode Base64 text", "Base64 违规初判"),
                    settings.base64_moderation_enable,
                )?
            }
            KeyCode::Enter if selected == 7 => edit_real_context_number(
                ui,
                t("Minimum Base64 length", "Base64 最短长度"),
                settings.base64_moderation_min_chars,
                settings,
                |candidate, value| candidate.base64_moderation_min_chars = value,
            )?,
            KeyCode::Enter if selected == 8 => edit_real_context_number(
                ui,
                t("Maximum decoded characters", "Base64 最大解码字符数"),
                settings.base64_moderation_max_decoded_chars,
                settings,
                |candidate, value| candidate.base64_moderation_max_decoded_chars = value,
            )?,
            KeyCode::Enter if selected == 9 => edit_real_context_number(
                ui,
                t("Minimum printable ratio", "Base64 最低可打印比例"),
                settings.base64_moderation_min_printable_ratio,
                settings,
                |candidate, value| candidate.base64_moderation_min_printable_ratio = value,
            )?,
            _ => {}
        }
    }
}
