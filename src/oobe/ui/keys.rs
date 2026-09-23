//! 按键。`Esc` 是「上一步」而不是「跳过」：取消子菜单之后它空出来了，而且这是
//! 最好找的一个键。跳过退到 `Ctrl+S`。

use super::{pfocus, App, Origin, Outcome, Prov, Screen};
use crate::oobe::providers::{self, Choice, PROTOCOLS};
use crossterm::event::{KeyCode, KeyModifiers};

/// 导航态下 `hjkl` 等价方向键；编辑态下它们只能是字母。
fn vimify(app: &App, code: KeyCode) -> KeyCode {
    if app.editing {
        return code;
    }
    match code {
        KeyCode::Char('j') => KeyCode::Down,
        KeyCode::Char('k') => KeyCode::Up,
        KeyCode::Char('h') => KeyCode::Left,
        KeyCode::Char('l') => KeyCode::Right,
        _ => code,
    }
}

/// 返回 `true` 表示这一下往终端打过字（装 shell hook 那种），画面要整屏重来。
pub(in crate::oobe) fn on_key(app: &mut App, raw: KeyCode, mods: KeyModifiers) -> bool {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);

    if ctrl && matches!(raw, KeyCode::Char('c')) {
        app.outcome = Some(Outcome::Aborted);
        return false;
    }

    // 开场动画任意键跳过。第一次看着好玩，第十次就是挡路的。
    if app.screen == Screen::Welcome && !app.loader.done() {
        app.skip_intro();
        if raw == KeyCode::Enter {
            return false;
        }
    }

    if app.editing {
        match raw {
            KeyCode::Esc => app.leave_edit(),
            KeyCode::Enter if alt => {
                if let Some(buffer) = app.buf() {
                    buffer.push('\n');
                }
            }
            KeyCode::Enter => {
                app.leave_edit();
                match app.screen {
                    Screen::Persona => app.focus = (app.focus + 1).min(pfocus::GO),
                    Screen::Identity => app.focus = 1,
                    Screen::Provider if app.prov == Prov::Form => {
                        app.form_focus = app.form_action_index()
                    }
                    Screen::Provider if app.prov == Prov::CustomEp => {
                        app.focus = (app.focus + 1).min(5)
                    }
                    _ => {}
                }
            }
            KeyCode::Backspace => {
                if let Some(buffer) = app.buf() {
                    buffer.pop();
                }
            }
            KeyCode::Char(ch) if !ctrl => {
                if let Some(buffer) = app.buf() {
                    buffer.push(ch);
                }
            }
            _ => {}
        }
        return false;
    }

    // 模型列表的搜索输入：字符进查询串（jk 在这儿也是字母），Enter 收起输入框、
    // 筛选留着，Esc 清掉筛选。上下键照常选。
    if app.screen == Screen::Provider && app.prov == Prov::PickModel && app.model_search {
        match raw {
            KeyCode::Esc => {
                app.model_search = false;
                app.model_query.clear();
                app.model_cur = 0;
            }
            KeyCode::Enter => app.model_search = false,
            KeyCode::Backspace => {
                app.model_query.pop();
                app.clamp_model_cursor();
            }
            KeyCode::Up => app.model_cur = app.model_cur.saturating_sub(1),
            KeyCode::Down => {
                let last = app.visible_models().len().saturating_sub(1);
                app.model_cur = (app.model_cur + 1).min(last);
            }
            KeyCode::Char(ch) if !ctrl => {
                app.model_query.push(ch);
                app.model_cur = 0;
            }
            _ => {}
        }
        return false;
    }

    if ctrl && matches!(raw, KeyCode::Char('s')) {
        app.finish(Outcome::Skipped);
        return false;
    }
    if raw == KeyCode::Esc {
        match app.screen {
            Screen::Welcome => app.outcome = Some(Outcome::Aborted),
            // 筛着的模型列表：Esc 先清筛选，再按一次才退回上一屏。
            Screen::Provider if app.prov == Prov::PickModel && !app.model_query.is_empty() => {
                app.model_query.clear();
                app.model_cur = 0;
            }
            Screen::Provider if app.prov != Prov::Pick => {
                app.prov = match app.prov {
                    Prov::PickModel | Prov::Fetching => {
                        app.fetch = None;
                        app.pending = None;
                        match app.origin {
                            Origin::Form => Prov::Form,
                            Origin::Custom => Prov::CustomEp,
                            Origin::Pick => Prov::Pick,
                        }
                    }
                    _ => Prov::Pick,
                };
                app.notice = None;
                app.fade = 0;
            }
            screen => {
                let previous = screen.prev();
                app.goto(previous);
            }
        }
        return false;
    }
    if app.screen == Screen::Features {
        if raw == KeyCode::Char('g') {
            app.feat_cur = 0;
            return false;
        }
        if raw == KeyCode::Char('G') {
            app.feat_cur = app.feats.len().saturating_sub(1);
            return false;
        }
    }

    let code = vimify(app, raw);
    let tab_fwd = raw == KeyCode::Tab && !shift;
    let tab_back = raw == KeyCode::BackTab || (raw == KeyCode::Tab && shift);

    match app.screen {
        Screen::Welcome => {
            if code == KeyCode::Enter {
                app.goto(Screen::Persona);
            }
        }

        Screen::Persona => {
            let max = if app.focus >= pfocus::CUSTOM {
                pfocus::GO
            } else {
                pfocus::CUSTOM
            };
            match code {
                KeyCode::Enter if app.field_here() => app.enter_edit(),
                KeyCode::Enter => {
                    if app.commit_persona() {
                        app.goto(Screen::Features);
                    }
                }
                KeyCode::Up => {
                    app.focus = app.focus.saturating_sub(1);
                    app.persona_custom = app.focus >= pfocus::CUSTOM;
                }
                KeyCode::Down => {
                    app.focus = (app.focus + 1).min(max);
                    app.persona_custom = app.focus >= pfocus::CUSTOM;
                }
                _ if tab_fwd => {
                    app.focus = (app.focus + 1).min(max);
                    app.persona_custom = app.focus >= pfocus::CUSTOM;
                }
                _ if tab_back => {
                    app.focus = app.focus.saturating_sub(1);
                    app.persona_custom = app.focus >= pfocus::CUSTOM;
                }
                _ => {}
            }
        }

        Screen::Features => {
            let last = app.feats.len().saturating_sub(1);
            let toggle = |app: &mut App| {
                if let Some(item) = app.feats.get_mut(app.feat_cur) {
                    item.on = !item.on;
                }
            };
            match code {
                // 双列：上下跨行是 ±2，左右换列是 ±1。
                KeyCode::Up => app.feat_cur = app.feat_cur.saturating_sub(2),
                KeyCode::Down => app.feat_cur = (app.feat_cur + 2).min(last),
                KeyCode::Left => app.feat_cur = app.feat_cur.saturating_sub(1),
                KeyCode::Right => app.feat_cur = (app.feat_cur + 1).min(last),
                KeyCode::PageUp => app.feat_cur = app.feat_cur.saturating_sub(12),
                KeyCode::PageDown => app.feat_cur = (app.feat_cur + 12).min(last),
                KeyCode::Home => app.feat_cur = 0,
                KeyCode::End => app.feat_cur = last,
                KeyCode::Char(' ') => toggle(app),
                // Ctrl+A:全开 / 全关来回切。有没开的就全开上,已经全开了才是
                // 全关——半开状态下按一次的意图是「都要」(用户 09-14)。
                KeyCode::Char('a' | 'A') if ctrl => {
                    let target = app.feats.iter().any(|item| !item.on);
                    for item in app.feats.iter_mut() {
                        item.on = target;
                    }
                }
                KeyCode::Enter => {
                    if app.commit_features() {
                        app.goto(Screen::Identity);
                    }
                }
                _ if tab_fwd || tab_back => toggle(app),
                _ => {}
            }
        }

        Screen::Identity => match code {
            KeyCode::Enter if app.field_here() => app.enter_edit(),
            KeyCode::Enter => {
                if app.commit_identity() {
                    app.goto(Screen::ShellHook);
                }
            }
            KeyCode::Up => app.focus = 0,
            KeyCode::Down => app.focus = 1,
            _ if tab_fwd || tab_back => app.focus = 1 - app.focus,
            _ => {}
        },

        Screen::ShellHook => {
            let last = app.shells.len();
            match code {
                KeyCode::Up => app.shell_cur = app.shell_cur.saturating_sub(1),
                KeyCode::Down => app.shell_cur = (app.shell_cur + 1).min(last),
                KeyCode::Enter => match app.commit_shell() {
                    Ok(printed) => {
                        app.goto(Screen::Sandbox);
                        return printed;
                    }
                    Err(error) => app.notice = Some(error),
                },
                _ if tab_fwd => {
                    app.shell_cur = if app.shell_cur == last {
                        0
                    } else {
                        app.shell_cur + 1
                    }
                }
                _ if tab_back => {
                    app.shell_cur = if app.shell_cur == 0 {
                        last
                    } else {
                        app.shell_cur - 1
                    }
                }
                _ => {}
            }
        }

        Screen::Sandbox => match code {
            KeyCode::Up => app.sandbox_cur = 0,
            KeyCode::Down => app.sandbox_cur = 1,
            KeyCode::Enter => {
                if app.commit_sandbox() {
                    app.goto(Screen::Provider);
                }
            }
            _ if tab_fwd || tab_back => app.sandbox_cur = 1 - app.sandbox_cur,
            _ => {}
        },

        Screen::Provider => match app.prov {
            Prov::Pick => {
                let last = app.options.len().saturating_sub(1);
                match code {
                    KeyCode::Up => app.prov_cur = app.prov_cur.saturating_sub(1),
                    KeyCode::Down => app.prov_cur = (app.prov_cur + 1).min(last),
                    KeyCode::PageUp => app.prov_cur = app.prov_cur.saturating_sub(8),
                    KeyCode::PageDown => app.prov_cur = (app.prov_cur + 8).min(last),
                    KeyCode::Home => app.prov_cur = 0,
                    KeyCode::End => app.prov_cur = last,
                    KeyCode::Enter => {
                        let Some(option) = app.options.get(app.prov_cur).cloned() else {
                            return false;
                        };
                        app.notice = None;
                        match option.choice {
                            Choice::Keep => app.finish(Outcome::Completed),
                            Choice::Cli(provider) => app.start_fetch(provider, Origin::Pick),
                            Choice::Preset {
                                provider,
                                needs_key: false,
                            } => app.start_fetch(provider, Origin::Pick),
                            Choice::Preset { provider, .. } => {
                                app.public_quota = providers::public_quota_available(&provider);
                                app.preset = Some(provider);
                                app.api_key.clear();
                                app.prov = Prov::Form;
                                app.form_focus = 0;
                                app.fade = 0;
                            }
                            Choice::Custom => {
                                app.prov = Prov::CustomEp;
                                app.focus = 0;
                                app.fade = 0;
                            }
                            Choice::OpenSettings => app.finish(Outcome::OpenSettings),
                        }
                    }
                    _ if tab_fwd => {
                        app.prov_cur = if app.prov_cur == last {
                            0
                        } else {
                            app.prov_cur + 1
                        }
                    }
                    _ if tab_back => {
                        app.prov_cur = if app.prov_cur == 0 {
                            last
                        } else {
                            app.prov_cur - 1
                        }
                    }
                    _ => {}
                }
            }
            Prov::Form => {
                let action_index = app.form_action_index();
                let toggle_here = app.form_has_public() && app.form_focus == 0;
                match code {
                    KeyCode::Enter if app.field_here() => app.enter_edit(),
                    KeyCode::Enter | KeyCode::Char(' ') if toggle_here => {
                        app.public_quota = !app.public_quota;
                    }
                    KeyCode::Enter if app.form_focus == action_index => {
                        let use_public = app.form_has_public() && app.public_quota;
                        if !use_public && app.api_key.trim().is_empty() {
                            app.notice = Some("先填 API key".into());
                            return false;
                        }
                        let Some(preset) = app.preset.clone() else {
                            app.prov = Prov::Pick;
                            return false;
                        };
                        app.notice = None;
                        let key = if use_public { "" } else { app.api_key.as_str() };
                        let provider = providers::with_key(preset, key);
                        app.start_fetch(provider, Origin::Form);
                    }
                    KeyCode::Enter => app.form_focus = (app.form_focus + 1).min(action_index),
                    KeyCode::Up => app.form_focus = app.form_focus.saturating_sub(1),
                    KeyCode::Down => app.form_focus = (app.form_focus + 1).min(action_index),
                    _ if (tab_fwd || tab_back) && toggle_here => {
                        app.public_quota = !app.public_quota;
                    }
                    _ if tab_fwd => app.form_focus = (app.form_focus + 1) % (action_index + 1),
                    _ if tab_back => {
                        app.form_focus = (app.form_focus + action_index) % (action_index + 1)
                    }
                    _ => {}
                }
            }
            Prov::CustomEp => match code {
                KeyCode::Enter if app.field_here() => app.enter_edit(),
                KeyCode::Enter if app.focus == 5 => {
                    if let Some(error) = providers::validate_custom(&app.ep_id, &app.ep_url) {
                        app.notice = Some(error.into());
                        return false;
                    }
                    app.notice = None;
                    let provider = providers::custom_provider(
                        &app.ep_name,
                        &app.ep_id,
                        &app.ep_url,
                        PROTOCOLS[app.ep_proto].0,
                        &app.api_key,
                    );
                    app.start_fetch(provider, Origin::Custom);
                }
                KeyCode::Enter if app.focus == 4 => {
                    app.ep_proto = (app.ep_proto + 1) % PROTOCOLS.len()
                }
                KeyCode::Left if app.focus == 4 => {
                    app.ep_proto = (app.ep_proto + PROTOCOLS.len() - 1) % PROTOCOLS.len()
                }
                KeyCode::Right if app.focus == 4 => {
                    app.ep_proto = (app.ep_proto + 1) % PROTOCOLS.len()
                }
                KeyCode::Up => app.focus = app.focus.saturating_sub(1),
                KeyCode::Down => app.focus = (app.focus + 1).min(5),
                _ if tab_fwd => app.focus = (app.focus + 1) % 6,
                _ if tab_back => app.focus = (app.focus + 5) % 6,
                _ => {}
            },
            Prov::Fetching => {}
            Prov::PickModel => {
                let len = app.visible_models().len();
                let last = len.saturating_sub(1);
                match code {
                    KeyCode::Char('/') => app.model_search = true,
                    KeyCode::Up => app.model_cur = app.model_cur.saturating_sub(1),
                    KeyCode::Down => app.model_cur = (app.model_cur + 1).min(last),
                    KeyCode::PageUp => app.model_cur = app.model_cur.saturating_sub(8),
                    KeyCode::PageDown => app.model_cur = (app.model_cur + 8).min(last),
                    KeyCode::Enter => {
                        if let Some(model) = app.selected_model() {
                            app.commit_provider(&model);
                        }
                    }
                    _ if tab_fwd => app.model_cur = (app.model_cur + 1) % len.max(1),
                    _ if tab_back => {
                        let len = len.max(1);
                        app.model_cur = (app.model_cur + len - 1) % len
                    }
                    _ => {}
                }
            }
        },
    }
    false
}
