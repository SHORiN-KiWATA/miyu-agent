//! 方向键：命令候选和任务条（会话项目第 3 段，用户口径第 8 条）。
//!
//! - 命令候选开着时，↑↓ 在候选里挑，回车 / Tab 用挑中的那一条；
//! - 否则光标在输入框最后一行、又没在翻上键历史时，↓ 进任务条。任务条上 ↑↓ 挪，露不下
//!   的跟着滚（最多露 5 条，底下「↓ 还有 x 个」）；在最上面一条再按 ↑ 回输入框；回车
//!   就是点它，光标留在任务条上（切了会话就停在切过去的那条）；Esc 回输入框；打字也回
//!   输入框，字照常进去。
//!
//! 编辑器不知道候选面板有没有被 Esc 收掉、底下有没有任务条，所以这一层在活动区上，
//! 排在编辑器前面。

use super::*;

/// 一次按键在这儿的着落。
pub(in crate::cli) enum Navigated {
    /// 不归这儿管，照旧往下交。挑中的命令已经填进输入框的，由编辑器接着处理这一下回车。
    Pass,
    /// 这儿处理完了，活动区要重画。
    Done,
}

impl LiveReplTail {
    pub(in crate::cli) fn navigate_key(&mut self, event: &Event) -> Result<Navigated> {
        let (code, modifiers) = match event {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind,
                ..
            }) if *kind != KeyEventKind::Release => (*code, *modifiers),
            Event::Paste(_) => {
                self.leave_strip();
                return Ok(Navigated::Pass);
            }
            _ => return Ok(Navigated::Pass),
        };
        // 面板开着时按键归面板。
        if self.overlay_open() {
            return Ok(Navigated::Pass);
        }
        if self.strip_focus.is_some() {
            return self.navigate_strip(code, modifiers);
        }
        let plain = modifiers.is_empty();
        if let Some(names) = self.command_candidates() {
            match code {
                KeyCode::Down | KeyCode::Up if plain => {
                    self.move_command_pick(names.len(), code == KeyCode::Down);
                    return Ok(Navigated::Done);
                }
                KeyCode::Tab | KeyCode::Enter if plain => {
                    if let Some(index) = self.command_pick() {
                        return Ok(self.take_command(names[index], code == KeyCode::Enter));
                    }
                }
                KeyCode::Esc if self.command_pick().is_some() => {
                    self.command_pick = None;
                    return Ok(Navigated::Done);
                }
                _ => {}
            }
            return Ok(Navigated::Pass);
        }
        if code == KeyCode::Down && plain && self.may_enter_strip() {
            // 露在最上面的那一条，滚动那一截停在原地（在子代理会话里是露着正在看的那条的地方）。
            // 主会话里顶上那行「● 主会话」就是这里、回车什么也不做：跳过它，↓ 回车照旧是进第一个
            // 子代理（09-26 加这一行之前就是这个手感）；想停到它上面再按 ↑。
            let view = self.strip_view();
            let rows = self.strip_rows();
            let visible = view.visible(rows.len());
            let focus = visible
                .iter()
                .copied()
                .find(|&index| {
                    !matches!(
                        rows[index],
                        crate::cli::repl::strip::StripItem::Root { current: true, .. }
                    )
                })
                .or_else(|| visible.first().copied());
            self.strip_scroll = view.scroll;
            self.strip_focus = focus;
            return Ok(Navigated::Done);
        }
        Ok(Navigated::Pass)
    }

    /// 命令候选里挑中的那一条。输入框一变就作废（候选跟着变了）。
    pub(in crate::cli) fn command_pick(&self) -> Option<usize> {
        self.command_pick
            .as_ref()
            .filter(|(_, typed)| *typed == self.editor.input)
            .map(|(index, _)| *index)
    }

    /// 候选开着、方向键能在里面挑的时候，是哪几条。在打命令名（还没空格）、不是翻上键
    /// 历史翻出来的、面板也没被 Esc 收掉。
    fn command_candidates(&self) -> Option<Vec<&'static str>> {
        let typed = self.editor.input.trim_start();
        if !typed.starts_with('/') || typed.contains(char::is_whitespace) {
            return None;
        }
        if crate::cli::repl_history_is_clean(
            &self.editor.input,
            &self.editor.history,
            self.editor.history_clean_index,
        ) {
            return None;
        }
        if self
            .screen
            .as_ref()
            .is_some_and(screen::Screen::command_hint_dismissed)
        {
            return None;
        }
        let names = miyu_core::slash_commands::repl_command_suggestions(typed);
        (!names.is_empty()).then_some(names)
    }

    /// ↓ 从没挑往第一条走、到底停住；↑ 从没挑跳到最后一条，从第一条再往上就是不挑了。
    fn move_command_pick(&mut self, len: usize, down: bool) {
        let next = match (self.command_pick(), down) {
            (None, true) => Some(0),
            (None, false) => len.checked_sub(1),
            (Some(index), true) => Some((index + 1).min(len.saturating_sub(1))),
            (Some(index), false) => index.checked_sub(1),
        };
        self.command_pick = next.map(|index| (index, self.editor.input.clone()));
    }

    /// 用挑中的那条命令。Tab 只补全；回车直接执行，要参数的（`<名字>`）补上空格等人接着打。
    fn take_command(&mut self, name: &'static str, enter: bool) -> Navigated {
        self.command_pick = None;
        let needs_args = enter
            && miyu_core::slash_commands::repl_command_spec_for_name(name)
                .is_some_and(|spec| spec.arg_hint.starts_with('<'));
        self.editor.input = if needs_args {
            format!("{name} ")
        } else {
            name.to_string()
        };
        self.editor.cursor = self.editor.input.chars().count();
        self.editor.history_clean_index = None;
        self.editor.raw_pasted_lines = 0;
        if enter && !needs_args {
            Navigated::Pass
        } else {
            Navigated::Done
        }
    }

    /// 光标在输入框最后一行、没在翻上键历史、底下有任务条。
    fn may_enter_strip(&self) -> bool {
        !self.strip_rows().is_empty()
            && !crate::cli::repl_history_is_clean(
                &self.editor.input,
                &self.editor.history,
                self.editor.history_clean_index,
            )
            && self.editor.cursor_on_last_row()
    }

    fn navigate_strip(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<Navigated> {
        let len = self.strip_rows().len();
        let Some(focus) = self.strip_focus.filter(|focus| *focus < len) else {
            self.leave_strip();
            return Ok(Navigated::Pass);
        };
        let plain = modifiers.is_empty();
        Ok(match code {
            KeyCode::Down if plain => {
                let next = (focus + 1).min(len - 1);
                self.strip_scroll = self.strip_view().scroll_to_show(next, len);
                self.strip_focus = Some(next);
                Navigated::Done
            }
            KeyCode::Up if plain => {
                match focus.checked_sub(1) {
                    Some(previous) => {
                        self.strip_scroll = self.strip_view().scroll_to_show(previous, len);
                        self.strip_focus = Some(previous);
                    }
                    None => self.leave_strip(),
                }
                Navigated::Done
            }
            // 回车是点它。光标留在任务条上（用户 09-26：原来每切一次会话就回到输入框）：切了会话
            // 的话，切完停在切过去的那一条，见 `apply_strip_refocus`。
            KeyCode::Enter if plain => {
                self.strip_refocus = self
                    .strip_rows()
                    .get(focus)
                    .and_then(crate::cli::repl::strip::StripItem::session_id)
                    .map(|target| (target.to_string(), self.current_strip_session()));
                self.activate_strip_row(focus)?;
                Navigated::Done
            }
            KeyCode::Esc => {
                self.leave_strip();
                Navigated::Done
            }
            // 别的键：回输入框，这一下照常交给编辑器（打字接着往里打）。
            _ => {
                self.leave_strip();
                Navigated::Pass
            }
        })
    }

    pub(in crate::cli::repl::tail) fn leave_strip(&mut self) {
        self.strip_focus = None;
        self.strip_scroll = 0;
    }
}
