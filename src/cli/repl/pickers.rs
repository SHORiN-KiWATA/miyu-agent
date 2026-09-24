//! 全屏面板版的三个菜单：/effort（思考档位）、/persona（单选）、/models（多选勾选）。
//!
//! 行内版各在 `variant_menu` / `select` 里；这里只把同一份状态接到 `panel` 上，
//! 菜单长什么样、按键怎么算，两种形态一个来源。

use super::panel::{self, with_help_line, PanelFrame, PanelModel};
use crate::cli::*;

impl PanelModel for VariantMenu {
    type Output = Option<VariantSelections>;

    fn desired_rows(&self) -> u16 {
        self.height()
    }

    fn content(&mut self, frame: &PanelFrame) -> Vec<String> {
        self.lines(frame.width, frame.visible)
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Self::Output> {
        self.handle_key(code, modifiers)
    }
}

/// /effort 的面板。`None` = 取消。
pub(in crate::cli) fn pick_effort(
    live: &mut LiveReplTail,
    mut menu: VariantMenu,
) -> Result<Option<VariantSelections>> {
    panel::pick(live, &mut menu)
}

/// 模糊筛选的列表。
///
/// 单选：当前项打 `[*]`、光标一开始就落在它上，Enter 选高亮，Esc/q 取消。
/// 多选：Tab 勾选，Enter/q/Esc 完成（回车前没动过勾选、又搜过或移过光标，就当
/// 单选切到高亮那一项——和行内版 `inline_fuzzy_select` 一个规矩），Ctrl+C 取消。
struct FuzzyList<'a> {
    title: String,
    items: Vec<String>,
    active: Vec<bool>,
    initial: Vec<bool>,
    multi: bool,
    /// 多选时按 Tab 翻哪几格：给了就由它决定（`/models` 的「继承」连带，见
    /// `model_cmds::toggle_model_row`），没给就是翻高亮那一格。
    toggle: Option<&'a dyn Fn(&mut [bool], usize)>,
    matcher: SkimMatcherV2,
    query: String,
    selected: usize,
    scroll: usize,
    navigated: bool,
}

impl<'a> FuzzyList<'a> {
    fn new(title: &str, items: &[String], active: Vec<bool>, multi: bool, selected: usize) -> Self {
        Self {
            title: title.to_string(),
            items: items.to_vec(),
            initial: active.clone(),
            active,
            toggle: None,
            multi,
            matcher: SkimMatcherV2::default(),
            query: String::new(),
            selected: selected.min(items.len().saturating_sub(1)),
            scroll: 0,
            navigated: false,
        }
    }

    fn matches(&self) -> Vec<(i64, usize)> {
        fuzzy_matches(&self.matcher, &self.items, &self.query)
    }

    fn finished(&self) -> Option<Vec<bool>> {
        self.multi.then(|| self.active.clone())
    }
}

fn solo(len: usize, index: usize) -> Vec<bool> {
    let mut flags = vec![false; len];
    if let Some(slot) = flags.get_mut(index) {
        *slot = true;
    }
    flags
}

impl PanelModel for FuzzyList<'_> {
    type Output = Option<Vec<bool>>;

    fn desired_rows(&self) -> u16 {
        inline_fuzzy_lines(self.items.len())
    }

    fn content(&mut self, frame: &PanelFrame) -> Vec<String> {
        let matches = self.matches();
        self.selected = self.selected.min(matches.len().saturating_sub(1));
        let visible = matches.len().min(frame.visible);
        self.scroll = inline_fuzzy_scroll(self.selected, self.scroll, visible);
        let width = frame.width;
        let mut lines = vec![inline_single_header(&self.title, &self.query, width)];
        if matches.is_empty() {
            lines.push(format!("\x1b[2m{}\x1b[0m", t("no matches", "没有匹配项")));
        } else {
            lines.extend(
                matches
                    .iter()
                    .skip(self.scroll)
                    .take(visible)
                    .enumerate()
                    .map(|(row, (_, index))| {
                        inline_fuzzy_item_line(
                            &self.items[*index],
                            self.scroll + row == self.selected,
                            self.active.get(*index).copied().unwrap_or(false),
                            width,
                        )
                    }),
            );
        }
        let help = if self.multi {
            inline_fuzzy_help_line(width)
        } else {
            inline_single_help_line(width, false)
        };
        with_help_line(lines, frame.panel.rows, help)
    }

    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Self::Output> {
        let matches = self.matches();
        let control = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('c') if control => Some(None),
            KeyCode::Esc => Some(self.finished()),
            KeyCode::Char('q') if self.query.is_empty() => Some(self.finished()),
            KeyCode::Enter => {
                let highlighted = matches.get(self.selected).map(|(_, index)| *index);
                if !self.multi {
                    return Some(highlighted.map(|index| solo(self.items.len(), index)));
                }
                if self.active == self.initial && (self.navigated || !self.query.is_empty()) {
                    if let Some(index) = highlighted {
                        return Some(Some(solo(self.items.len(), index)));
                    }
                }
                Some(Some(self.active.clone()))
            }
            KeyCode::Tab if self.multi => {
                if let Some((_, index)) = matches.get(self.selected) {
                    match self.toggle {
                        Some(toggle) => toggle(&mut self.active, *index),
                        None => {
                            if let Some(slot) = self.active.get_mut(*index) {
                                *slot = !*slot;
                            }
                        }
                    }
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') if !control => {
                self.navigated = true;
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') if !control => {
                self.navigated = true;
                self.selected = (self.selected + 1).min(matches.len().saturating_sub(1));
                None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.scroll = 0;
                None
            }
            KeyCode::Char(ch) if !control => {
                self.query.push(ch);
                self.selected = 0;
                self.scroll = 0;
                None
            }
            _ => None,
        }
    }
}

/// 单选面板（/persona）。返回选中的下标；`None` = 取消。
pub(in crate::cli) fn pick_single(
    live: &mut LiveReplTail,
    title: &str,
    items: &[String],
    initial: usize,
) -> Result<Option<usize>> {
    let mut list = FuzzyList::new(title, items, solo(items.len(), initial), false, initial);
    Ok(panel::pick(live, &mut list)?.and_then(|flags| flags.iter().position(|on| *on)))
}

/// 多选面板，带一条 Tab 规矩（见 `FuzzyList::toggle`）。
pub(in crate::cli) fn pick_multi_with(
    live: &mut LiveReplTail,
    title: &str,
    items: &[String],
    active: Vec<bool>,
    toggle: Option<&dyn Fn(&mut [bool], usize)>,
) -> Result<Option<Vec<bool>>> {
    let mut list = FuzzyList::new(title, items, active, true, 0);
    list.toggle = toggle;
    panel::pick(live, &mut list)
}

/// 全屏下不带参数的 `/models`：面板多选 → 落成会话覆盖 → 一行回执。返回真的改了没。
///
/// 空闲时（`RemoteRepl::cmd_models`）和回合跑着时（`midturn_panel`，09-20）
/// 共用：只要路径 + 会话 id，不碰 `RemoteRepl` 的状态，所以回合循环里也调得了。
pub(in crate::cli) async fn pick_models_panel(
    paths: &MiyuPaths,
    live: &mut LiveReplTail,
    session_id: &str,
) -> Result<bool> {
    let config = AppConfig::load(paths)?;
    let choices = config.text_provider_model_choices();
    if choices.is_empty() {
        bail!(
            "{}",
            t(
                "no configured provider models; configure a model first",
                "没有已配置的 provider 模型；请先配置模型",
            )
        );
    }
    let menu = SessionModelMenu::new(&config, choices, paths, Some(session_id))?;
    let rule = menu.toggle_rule();
    let Some(active) = pick_multi_with(
        live,
        t("Select model", "选择模型"),
        &menu.labels,
        menu.initial.clone(),
        Some(&rule),
    )?
    else {
        return Ok(false);
    };
    let (changed, message) = menu.apply(paths, Some(session_id), active).await?;
    repl_note(live, &format!("\x1b[2m{message}\x1b[0m\n"))?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<String> {
        ["alpha", "beta", "gamma"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn frame(rows: u16, width: usize) -> PanelFrame {
        PanelFrame {
            panel: super::panel::Panel {
                left: 0,
                top: 0,
                width: width as u16,
                rows,
            },
            width,
            visible: usize::from(rows.saturating_sub(2)),
        }
    }

    #[test]
    fn single_list_starts_on_the_current_item_and_enter_picks_the_highlight() {
        let mut list = FuzzyList::new("选择人格", &items(), solo(3, 1), false, 1);
        let lines = list.content(&frame(5, 40));
        assert_eq!(lines.len(), 5);
        assert!(lines[0].contains("选择人格"));
        assert!(lines[2].contains("[*] beta"), "{lines:?}");
        assert!(lines[2].contains('›'), "{lines:?}");
        assert_eq!(list.on_key(KeyCode::Down, KeyModifiers::NONE), None);
        assert_eq!(
            list.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Some(solo(3, 2)))
        );
        let mut list = FuzzyList::new("选择人格", &items(), solo(3, 1), false, 1);
        assert_eq!(list.on_key(KeyCode::Esc, KeyModifiers::NONE), Some(None));
    }

    #[test]
    fn multi_list_keeps_the_inline_enter_rules() {
        // 没动勾选、也没搜索或移动：回车=按现状完成。
        let mut list = FuzzyList::new("选择模型", &items(), solo(3, 0), true, 0);
        assert_eq!(
            list.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Some(solo(3, 0)))
        );
        // 移过光标：回车=单选切到高亮项。
        let mut list = FuzzyList::new("选择模型", &items(), solo(3, 0), true, 0);
        list.on_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            list.on_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(Some(solo(3, 1)))
        );
        // Tab 勾过：回车=按勾选完成；Esc 同样是完成不是取消。
        let mut list = FuzzyList::new("选择模型", &items(), solo(3, 0), true, 0);
        list.on_key(KeyCode::Down, KeyModifiers::NONE);
        list.on_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            list.on_key(KeyCode::Esc, KeyModifiers::NONE),
            Some(Some(vec![true, true, false]))
        );
        let mut list = FuzzyList::new("选择模型", &items(), solo(3, 0), true, 0);
        assert_eq!(
            list.on_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(None)
        );
    }

    #[test]
    fn typing_filters_and_no_match_line_appears() {
        let mut list = FuzzyList::new("选择模型", &items(), solo(3, 0), true, 0);
        for ch in "zzz".chars() {
            list.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let lines = list.content(&frame(5, 40));
        assert!(
            lines[1].contains("没有匹配项") || lines[1].contains("no matches"),
            "{lines:?}"
        );
    }
}
