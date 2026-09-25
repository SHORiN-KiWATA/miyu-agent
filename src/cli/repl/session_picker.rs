//! Full-screen session selection on the shared picker panel (`panel`): the
//! lobby keeps its input and captions; conversations share the question
//! panel's scrollable body viewport. Search, Ctrl+D delete, Enter switch.
//!
//! Ctrl+D **当场删**，没有 y/N（用户 09-20 拍板：「应该改成 ctrl+d 直接删除」）。
//! 原来是按一下弹一行「删除「xx」？y/N」再等一个键。删掉的那一行从列表里消失
//! 就是回执，光标停在原位（下面的行顶上来），可以连着删。
//!
//! 例外是**正在跑**的会话（它自己或它的子代理在跑，09-25）：删它会停掉这一轮、拆掉整棵
//! 子代理树。第一下只把那一行换成提醒，再按一下才删，按别的键就算了。连着删的时候光标
//! 会顶到它身上——用户就是这么一下把正跑着调研的主会话删掉的。

use super::panel::{self, with_help_line, PanelFrame, PanelModel};
use crate::cli::*;

pub(super) fn pick(
    live: &mut LiveReplTail,
    entries: &[SessionListEntry],
    active: &str,
    cursor: Option<usize>,
    notice: Option<String>,
) -> Result<SessionPick> {
    let mut picker = SessionPicker::new(entries.to_vec(), active, cursor).with_notice(notice);
    panel::pick(live, &mut picker)
}

/// 会话选择面板。自己带着那份列表：回合里开的 `/session` 面板要活过这一次调用（B4）。
pub(in crate::cli) struct SessionPicker {
    entries: Vec<SessionListEntry>,
    lines: Vec<String>,
    search: Vec<String>,
    matcher: SkimMatcherV2,
    query: String,
    selected: usize,
    scroll: usize,
    /// 按过一下 Ctrl+D、等着第二下的那一行（`entries` 下标）。
    armed: Option<usize>,
    /// 上一次删除没成的原因，换掉帮助行，按任意键消失。
    notice: Option<String>,
}

impl SessionPicker {
    pub(in crate::cli) fn new(
        entries: Vec<SessionListEntry>,
        active: &str,
        cursor: Option<usize>,
    ) -> Self {
        Self {
            lines: entries
                .iter()
                .map(|entry| session_select_line(entry, Some(active)))
                .collect(),
            search: entries.iter().map(session_select_search).collect(),
            matcher: SkimMatcherV2::default(),
            query: String::new(),
            selected: cursor.unwrap_or_else(|| session_initial_selection(&entries, Some(active))),
            scroll: 0,
            armed: None,
            notice: None,
            entries,
        }
    }

    pub(in crate::cli) fn with_notice(mut self, notice: Option<String>) -> Self {
        self.notice = notice;
        self
    }

    /// 那一行此刻显示的字：等第二下 Ctrl+D 的换成提醒（标题留着，看得出是哪条）。
    fn row_text(&self, index: usize) -> String {
        if self.armed != Some(index) {
            return self.lines[index].clone();
        }
        format!(
            "{} · {}",
            t(
                "running · Ctrl+D again stops it and its subagents and deletes it",
                "正在运行 · 再按 Ctrl+D 连同这一轮和子代理一起删",
            ),
            display_session_name(&self.entries[index].name),
        )
    }

    fn help_line(&self, width: usize) -> String {
        if self.armed.is_some() {
            let hint = t("any other key keeps it", "按别的键就不删");
            return format!("\x1b[33m{}\x1b[0m", truncate_visible_width(hint, width));
        }
        match &self.notice {
            Some(notice) => format!("\x1b[31m{}\x1b[0m", truncate_visible_width(notice, width)),
            None => inline_single_help_line(width, true),
        }
    }

    fn matches(&self) -> Vec<(i64, usize)> {
        fuzzy_matches(&self.matcher, &self.search, &self.query)
    }
}

impl PanelModel for SessionPicker {
    type Output = SessionPick;

    fn desired_rows(&self) -> u16 {
        inline_fuzzy_lines(self.matches().len())
    }

    fn content(&mut self, frame: &PanelFrame) -> Vec<String> {
        let matches = self.matches();
        self.selected = self.selected.min(matches.len().saturating_sub(1));
        let visible = matches.len().min(frame.visible);
        self.scroll = inline_fuzzy_scroll(self.selected, self.scroll, visible);
        let width = frame.width;
        let mut content = vec![inline_single_header(
            t("Select session", "选择会话"),
            &self.query,
            width,
        )];
        if matches.is_empty() {
            content.push(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")));
        } else {
            content.extend(
                matches
                    .iter()
                    .skip(self.scroll)
                    .take(visible)
                    .enumerate()
                    .map(|(row, (_, index))| {
                        inline_single_item_line(
                            &self.row_text(*index),
                            self.scroll + row == self.selected,
                            width,
                        )
                    }),
            );
        }
        with_help_line(content, frame.panel.rows, self.help_line(width))
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<SessionPick> {
        let matches = self.matches();
        let key = inline_select_key(code, modifiers, true);
        // 提醒和上一次的报错都只活到下一个键。
        self.notice = None;
        let armed = self.armed.take();
        match key {
            InlineSelectKey::Cancel => Some(SessionPick::Cancelled),
            InlineSelectKey::Accept => Some(matches.get(self.selected).map_or(
                SessionPick::Cancelled,
                |(_, index)| {
                    SessionPick::Switch(miyu_core::ipc::SessionRef::Id {
                        id: self.entries[*index].id.clone(),
                    })
                },
            )),
            // 当场删，不问 y/N（用户 09-20）。调用方删完会带着同一个
            // `index` 重开列表，光标停在原位。正在跑的要按第二下（09-25）。
            InlineSelectKey::DeleteRequest => {
                let &(_, index) = matches.get(self.selected)?;
                if self.entries[index].running && armed != Some(index) {
                    self.armed = Some(index);
                    return None;
                }
                Some(SessionPick::Delete {
                    session_id: self.entries[index].id.clone(),
                    index,
                })
            }
            InlineSelectKey::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            InlineSelectKey::Down => {
                self.selected = (self.selected + 1).min(matches.len().saturating_sub(1));
                None
            }
            InlineSelectKey::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.scroll = 0;
                None
            }
            InlineSelectKey::Char(ch) => {
                self.query.push(ch);
                self.selected = 0;
                self.scroll = 0;
                None
            }
            InlineSelectKey::Ignore => None,
        }
    }
}
