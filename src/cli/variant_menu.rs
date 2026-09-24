//! 思考档位菜单（/effort、`miyu effort`）：状态、每帧的行、按键。
//!
//! 画法与形态无关：行内（终端光标处）与全屏面板（`repl::pickers`）拿的是同一份
//! `lines()`，只是落笔的位置不同。09-17 从 `model_cmds` 搬出（那边过千行），顺手
//! 把行内「单模型 / 多模型」两个循环收成一个。

use crate::cli::repl::width::*;
use crate::cli::*;

/// 每个模型选了哪一档：(供应商, 模型, 档位；None = 模型默认档)。
pub(in crate::cli) type VariantSelections = Vec<(String, String, Option<String>)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) struct VariantMenuItem {
    pub(in crate::cli) provider_id: String,
    pub(in crate::cli) model: String,
    pub(in crate::cli) options: Vec<VariantMenuOption>,
    pub(in crate::cli) selected: usize,
    pub(in crate::cli) cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli) struct VariantMenuOption {
    pub(in crate::cli) label: String,
    pub(in crate::cli) value: Option<String>,
}

impl VariantMenuItem {
    pub(in crate::cli) fn from_options(options: &ThinkingVariantOptions) -> Self {
        let mut variants = vec![VariantMenuOption {
            label: "default".to_string(),
            value: None,
        }];
        variants.extend(options.variants.iter().map(|variant| VariantMenuOption {
            label: if variant == "default" {
                "default (variant)".to_string()
            } else {
                variant.clone()
            },
            value: Some(variant.clone()),
        }));
        let selected = options
            .selected
            .as_ref()
            .and_then(|selected| {
                variants
                    .iter()
                    .position(|variant| variant.value.as_ref() == Some(selected))
            })
            .unwrap_or(0);
        Self {
            provider_id: options.provider_id.clone(),
            model: options.model.clone(),
            options: variants,
            selected,
            cursor: selected,
        }
    }

    /// 会话档（09-24）：第一项「跟随全局（全局那一档）」，值为空 = 拔掉钉子；第二项是
    /// 模型默认档，钉的是 `MODEL_DEFAULT_PIN`（选默认是模型默认，不是回到跟随全局）；
    /// 后面是各档位。`pinned` 是这个会话钉住的那一项，没钉光标落在第一项。
    pub(in crate::cli) fn for_session(
        options: &ThinkingVariantOptions,
        pinned: Option<&str>,
    ) -> Self {
        let global = options.selected.as_deref().unwrap_or("default");
        let follow = if is_zh() {
            format!("跟随全局（{global}）")
        } else {
            format!("follow global ({global})")
        };
        let mut entries = vec![
            VariantMenuOption {
                label: follow,
                value: None,
            },
            VariantMenuOption {
                label: "default".to_string(),
                value: Some(miyu_core::llm::MODEL_DEFAULT_PIN.to_string()),
            },
        ];
        entries.extend(Self::from_options(options).options.into_iter().skip(1));
        let selected = pinned
            .and_then(|pinned| {
                entries
                    .iter()
                    .position(|entry| entry.value.as_deref() == Some(pinned))
            })
            .unwrap_or(0);
        Self {
            provider_id: options.provider_id.clone(),
            model: options.model.clone(),
            options: entries,
            selected,
            cursor: selected,
        }
    }

    pub(in crate::cli) fn selection(&self) -> (String, String, Option<String>) {
        (
            self.provider_id.clone(),
            self.model.clone(),
            self.options[self.selected].value.clone(),
        )
    }

    pub(in crate::cli) fn check_cursor(&mut self) {
        self.selected = self.cursor;
    }
}

/// 菜单状态。一个模型：一栏档位；多个模型：左栏模型、右栏档位，h/l 切栏。
#[derive(Debug, Clone)]
pub(in crate::cli) struct VariantMenu {
    items: Vec<VariantMenuItem>,
    active_column: usize,
    model_index: usize,
    model_scroll: usize,
    variant_scroll: usize,
}

impl VariantMenu {
    pub(in crate::cli) fn new(options: &[ThinkingVariantOptions]) -> Option<Self> {
        Self::from_items(options.iter().map(VariantMenuItem::from_options).collect())
    }

    pub(in crate::cli) fn from_items(items: Vec<VariantMenuItem>) -> Option<Self> {
        (!items.is_empty()).then_some(Self {
            items,
            active_column: 0,
            model_index: 0,
            model_scroll: 0,
            variant_scroll: 0,
        })
    }

    pub(in crate::cli) fn is_single(&self) -> bool {
        self.items.len() == 1
    }

    /// 想要的高度：抬头 + 条目 + 帮助行。
    pub(in crate::cli) fn height(&self) -> u16 {
        if self.is_single() {
            return inline_fuzzy_lines(self.items[0].options.len());
        }
        let max_options = self
            .items
            .iter()
            .map(|item| item.options.len())
            .max()
            .unwrap_or(1);
        inline_fuzzy_lines(self.items.len().max(max_options))
    }

    /// 这一帧的行（不含左侧竖条）：抬头、`visible` 行条目、帮助行。`width` 是内容宽。
    pub(in crate::cli) fn lines(&mut self, width: usize, visible: usize) -> Vec<String> {
        if self.is_single() {
            self.single_lines(width, visible)
        } else {
            self.column_lines(width, visible)
        }
    }

    fn single_lines(&mut self, available: usize, visible: usize) -> Vec<String> {
        let item = &self.items[0];
        let width = single_variant_content_width(item).min(available).max(1);
        self.variant_scroll = inline_fuzzy_scroll(
            item.cursor,
            self.variant_scroll,
            visible.min(item.options.len()),
        );
        let mut lines = vec![variant_menu_header(
            t("Thinking variant", "思考档位"),
            true,
            width,
        )];
        for row in 0..visible {
            let index = self.variant_scroll + row;
            lines.push(item.options.get(index).map_or_else(
                || " ".repeat(width),
                |variant| {
                    variant_menu_cell(
                        &variant.label,
                        index == item.cursor,
                        index == item.cursor,
                        Some(index == item.selected),
                        width,
                    )
                },
            ));
        }
        lines.push(format!(
            "\x1b[2m{}\x1b[0m",
            truncate_visible_width(
                t(
                    "j/k move · Tab select · Enter confirm · Esc/q cancel",
                    "j/k 移动 · Tab 勾选 · Enter 确认 · Esc/q 取消"
                ),
                available,
            )
        ));
        lines
    }

    fn column_lines(&mut self, width: usize, visible: usize) -> Vec<String> {
        let separator = if width >= 3 { " │ " } else { "" };
        let available = width.saturating_sub(visible_width(separator));
        let (left_width, right_width) = variant_menu_column_widths(&self.items, available);
        self.model_scroll = inline_fuzzy_scroll(
            self.model_index,
            self.model_scroll,
            visible.min(self.items.len()),
        );
        let variants = &self.items[self.model_index];
        self.variant_scroll = inline_fuzzy_scroll(
            variants.cursor,
            self.variant_scroll,
            visible.min(variants.options.len()),
        );
        let separator = format!("\x1b[2m{separator}\x1b[0m");
        let mut lines = vec![format!(
            "{}{separator}{}",
            variant_menu_header(
                t("Provider / Model", "Provider / 模型"),
                self.active_column == 0,
                left_width,
            ),
            variant_menu_header(
                t("Thinking variant", "思考档位"),
                self.active_column == 1,
                right_width,
            )
        )];
        for row in 0..visible {
            let left_index = self.model_scroll + row;
            let right_index = self.variant_scroll + row;
            let left = self.items.get(left_index).map_or_else(
                || " ".repeat(left_width),
                |item| {
                    variant_menu_cell(
                        &format!("{} / {}", item.provider_id, item.model),
                        self.active_column == 0 && left_index == self.model_index,
                        left_index == self.model_index,
                        None,
                        left_width,
                    )
                },
            );
            let right = variants.options.get(right_index).map_or_else(
                || " ".repeat(right_width),
                |variant| {
                    variant_menu_cell(
                        &variant.label,
                        self.active_column == 1 && right_index == variants.cursor,
                        right_index == variants.cursor,
                        Some(right_index == variants.selected),
                        right_width,
                    )
                },
            );
            lines.push(format!("{left}{separator}{right}"));
        }
        lines.push(format!(
            "\x1b[2m{}\x1b[0m",
            truncate_visible_width(
                t(
                    "h/l switch · j/k move · Tab select · Enter confirm · Esc/q cancel",
                    "h/l 切栏 · j/k 移动 · Tab 勾选 · Enter 确认 · Esc/q 取消"
                ),
                width,
            )
        ));
        lines
    }

    /// 一次按键。`Some(None)` 取消，`Some(Some(..))` 确认，`None` 继续。
    pub(in crate::cli) fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<Option<VariantSelections>> {
        let single = self.is_single();
        let index = self.model_index;
        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Some(None),
            KeyCode::Esc | KeyCode::Char('q') => Some(None),
            KeyCode::Enter => Some(Some(
                self.items.iter().map(VariantMenuItem::selection).collect(),
            )),
            KeyCode::Left | KeyCode::Char('h') if !single => {
                self.active_column = 0;
                None
            }
            KeyCode::Right | KeyCode::Char('l') if !single => {
                self.active_column = 1;
                None
            }
            KeyCode::Up | KeyCode::Char('k') if !single && self.active_column == 0 => {
                self.model_index = self.model_index.saturating_sub(1);
                self.variant_scroll = 0;
                None
            }
            KeyCode::Down | KeyCode::Char('j') if !single && self.active_column == 0 => {
                self.model_index = (self.model_index + 1).min(self.items.len() - 1);
                self.variant_scroll = 0;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let item = &mut self.items[index];
                item.cursor = item.cursor.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let item = &mut self.items[index];
                let last = item.options.len() - 1;
                item.cursor = (item.cursor + 1).min(last);
                None
            }
            KeyCode::Tab if single || self.active_column == 1 => {
                self.items[index].check_cursor();
                None
            }
            _ => None,
        }
    }
}

/// 行内版：在光标处往下画几行，选完擦掉。全屏 TUI 走 `repl::pickers::pick_effort`。
pub(in crate::cli) fn inline_variant_select(
    mut menu: VariantMenu,
) -> Result<Option<VariantSelections>> {
    let menu_lines = menu.height();
    reserve_inline_fuzzy_space(menu_lines)?;
    let mut session = InlineRawMode::start()?;
    let (_, cursor_y) = cursor::position().unwrap_or((0, menu_lines.saturating_sub(1)));
    let anchor_y = cursor_y.saturating_sub(menu_lines.saturating_sub(1));
    loop {
        let (cols, _) = terminal::size().unwrap_or((80, 24));
        let bar = inline_fuzzy_bar();
        let width = usize::from(cols).saturating_sub(visible_width(&bar)).max(1);
        let lines = menu.lines(width, usize::from(menu_lines.saturating_sub(2)));
        queue!(session.stdout, Hide)?;
        for row in 0..menu_lines {
            queue!(
                session.stdout,
                MoveTo(0, anchor_y + row),
                Clear(ClearType::CurrentLine)
            )?;
        }
        for (row, line) in lines.iter().take(usize::from(menu_lines)).enumerate() {
            queue!(
                session.stdout,
                MoveTo(0, anchor_y + row as u16),
                Print(&bar),
                Print(line)
            )?;
        }
        session.stdout.flush()?;
        if let Event::Key(KeyEvent {
            code, modifiers, ..
        }) = event::read()?
        {
            if let Some(result) = menu.handle_key(code, modifiers) {
                clear_inline_fuzzy(&mut session.stdout, anchor_y, menu_lines)?;
                return Ok(result);
            }
        }
    }
}

pub(in crate::cli) fn single_variant_content_width(item: &VariantMenuItem) -> usize {
    item.options
        .iter()
        .map(|option| visible_width(&option.label).saturating_add(6))
        .chain(std::iter::once(visible_width(t(
            "Thinking variant",
            "思考档位",
        ))))
        .max()
        .unwrap_or(1)
}

pub(in crate::cli) fn variant_menu_column_widths(
    items: &[VariantMenuItem],
    available: usize,
) -> (usize, usize) {
    if available == 0 {
        return (0, 0);
    }
    if available == 1 {
        return (1, 0);
    }
    let left_needed = items
        .iter()
        .map(|item| {
            visible_width(&format!("{} / {}", item.provider_id, item.model)).saturating_add(2)
        })
        .chain(std::iter::once(visible_width(t(
            "Provider / Model",
            "Provider / 模型",
        ))))
        .max()
        .unwrap_or(1);
    let right_needed = items
        .iter()
        .flat_map(|item| item.options.iter())
        .map(|option| visible_width(&option.label).saturating_add(6))
        .chain(std::iter::once(visible_width(t(
            "Thinking variant",
            "思考档位",
        ))))
        .max()
        .unwrap_or(1);
    if left_needed.saturating_add(right_needed) <= available {
        return (left_needed, right_needed);
    }
    let total_needed = left_needed.saturating_add(right_needed).max(1);
    let left = available
        .saturating_mul(left_needed)
        .saturating_div(total_needed)
        .clamp(1, available - 1);
    (left, available - left)
}

pub(in crate::cli) fn variant_menu_header(label: &str, active: bool, width: usize) -> String {
    let label = pad_visible_width(&truncate_visible_width(label, width), width);
    if active {
        format!("\x1b[1m\x1b[35m{label}\x1b[0m")
    } else {
        format!("\x1b[1m{label}\x1b[0m")
    }
}

pub(in crate::cli) fn variant_menu_cell(
    label: &str,
    focused: bool,
    highlighted: bool,
    checked: Option<bool>,
    width: usize,
) -> String {
    let marker = if highlighted { "›" } else { " " };
    let check = match checked {
        Some(true) => "[*] ",
        Some(false) => "[ ] ",
        None => "",
    };
    let line = pad_visible_width(
        &truncate_visible_width(&format!("{marker} {check}{label}"), width),
        width,
    );
    if focused {
        format!("\x1b[1m\x1b[35m{line}\x1b[0m")
    } else if checked == Some(true) {
        format!("\x1b[1m\x1b[32m{line}\x1b[0m")
    } else if highlighted {
        format!("\x1b[1m{line}\x1b[0m")
    } else {
        format!("\x1b[2m{line}\x1b[0m")
    }
}
