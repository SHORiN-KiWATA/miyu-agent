//! 「启用的功能」：这个人格挂哪些子系统、插件、脚本、技能、MCP 服务器。
//!
//! 和引导里那一屏是**同一张表**（[`feature_catalog`]），只是作用域不同：引导
//! 只摆「不是底线」的那些，这里摆全（记忆、技能、常开插件、只有机器层的网络
//! 搜索与识图）。2026-09-20 补上——在这之前，引导走完这张表就再也没有入口了。
//!
//! 一行上两个动作，对应两层开关：
//! - **空格** = 这个人格用不用它（写 `persona.toml`，退出这屏即写盘）；
//! - **回车** = 这台机器上它怎么配（密钥、尺寸、账号，写 `config.jsonc`，跟着
//!   设置界面统一的「退出时保存」走）。
//!
//! 勾上一行会**顺手把机器层的开关也打开**（用户 2026-09-20 拍板）：两层都真才
//! 挂得上，不然勾了不生效，人要在两个菜单之间来回找。取消勾选不关机器层——
//! 那儿存着密钥和尺寸，关掉再勾回来就得重填。

use crate::config_tui::*;
use miyu_base::config::feature_catalog::{self, CatalogScope, FeatureItem, FeatureKind};
use miyu_base::terminal::chrome::{clip, nil, View};
use ratatui::text::Line;

/// 正文里的一行是什么。分节线不可选中，光标要跳过它。
enum Row {
    Divider(&'static str),
    Item(usize),
}

fn section_of(kind: FeatureKind) -> &'static str {
    match kind {
        FeatureKind::Machine => t("Machine-wide", "机器能力"),
        FeatureKind::Subsystem => t("Subsystems", "子系统"),
        FeatureKind::Plugin => t("Built-in plugins", "内置插件"),
        FeatureKind::Script => t("Scripts", "脚本"),
        FeatureKind::Skill => t("Skills", "技能"),
        FeatureKind::Mcp => t("MCP servers", "MCP 服务器"),
    }
}

/// 这一行右边挂什么标记：能进设置的摆齿轮，勾着却因为机器层关着而不生效的
/// 点出来。
fn marks(ui: &Ui, item: &FeatureItem) -> String {
    let mut marks = String::new();
    if item.on && item.machine_on == Some(false) && item.kind != FeatureKind::Machine {
        marks.push_str(t(" (off on this machine)", " (本机未开)"));
    }
    if item.settings {
        marks.push_str(if ui.theme().ascii { "  [*]" } else { "  ⚙" });
    }
    marks
}

pub(in crate::config_tui) fn edit_features(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
    pending: &mut PendingWrites,
) -> Result<()> {
    let scope = config.active_persona_scope();
    let default_persona = miyu_core::skills::is_default_persona(config);
    // 这一轮改过就看改过的：退出去再进来要还是自己刚勾的样子。
    let mut manifest = pending.manifest(config, paths, &scope);
    let sources = crate::feature_sources::collect(config, paths);
    let mut items = feature_catalog::catalog(
        &manifest,
        &sources,
        default_persona,
        CatalogScope::Settings,
        Some(config),
    );
    if items.is_empty() {
        return Ok(());
    }
    // 进来时的勾选:保存时只给这一轮新勾上的开机器开关(见 `newly_ticked`)。
    let initial = items.clone();
    let mut selected = 0usize;
    let mut dirty = false;
    loop {
        let cx = ui.cx();
        let name_col = items
            .iter()
            .map(|item| display_width(&item.name) + 10)
            .max()
            .unwrap_or(NAME_COL_MIN)
            .clamp(NAME_COL_MIN, NAME_COL_MAX)
            .min(ui.body_width().saturating_sub(24).max(NAME_COL_MIN));

        // 按种类分节。同一种类的连在一起（catalog 就是按种类排的）。
        let mut rows: Vec<Row> = Vec::new();
        let mut last: Option<FeatureKind> = None;
        for (index, item) in items.iter().enumerate() {
            if last != Some(item.kind) {
                rows.push(Row::Divider(section_of(item.kind)));
                last = Some(item.kind);
            }
            rows.push(Row::Item(index));
        }
        let mut body: Vec<Line<'static>> = Vec::with_capacity(rows.len());
        let mut cursor_row = 0usize;
        for (row_index, row) in rows.iter().enumerate() {
            match row {
                Row::Divider(name) => body.push(cx.divider(name)),
                Row::Item(index) => {
                    let item = &items[*index];
                    if *index == selected {
                        cursor_row = row_index;
                    }
                    // 说明先按剩下的宽度截断再挂标记：脚本的描述是脚本作者
                    // 写的，长短不由我们定，不截会把齿轮顶出屏幕。
                    let tail = marks(ui, item);
                    let room = ui
                        .body_width()
                        .saturating_sub(name_col + display_width(&tail) + 2);
                    body.push(cx.check(
                        *index == selected,
                        item.on,
                        &item.name,
                        &format!("{}{tail}", clip(&item.hint, room)),
                        name_col,
                    ));
                }
            }
        }
        body.push(nil());

        let on = items.iter().filter(|item| item.on).count();
        // 回车做什么按当前这一行来:能配的进设置，其余的看完整说明。以前
        // 这行文案恒定写「设置」，而脚本/技能/MCP 按下去一点反应都没有。
        let (footer, keys) = key_bar(
            &cx,
            if items[selected].settings {
                t(
                    "[Space]toggle [Ctrl+A]all [⏎]settings [↑↓ jk]move [Esc]back",
                    "[空格]开关 [Ctrl+A]全开/全关 [⏎]设置 [↑↓ jk]移动 [Esc]返回",
                )
            } else {
                t(
                    "[Space]toggle [Ctrl+A]all [⏎]details [↑↓ jk]move [Esc]back",
                    "[空格]开关 [Ctrl+A]全开/全关 [⏎]详情 [↑↓ jk]移动 [Esc]返回",
                )
            },
        );
        ui.show(
            t(" FEATURES ", " 启用的功能 "),
            View {
                body,
                cursor_row,
                footer,
                counter: Some(format!("{on}/{}", items.len())),
                keys,
                ..View::default()
            },
        )?;

        match read_key_with_mods(ui)? {
            // Ctrl+A:全开 / 全关来回切。有没开的就全开上,已经全开了才是全关
            // ——半开状态下按一次的意图是「都要」(与 OOBE 同一语义,用户 09-23)。
            (KeyCode::Char('a' | 'A'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                let target = items.iter().any(|item| !item.on);
                for item in items.iter_mut() {
                    item.on = target;
                }
                dirty = true;
            }
            (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => break,
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => selected = selected.saturating_sub(1),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                selected = (selected + 1).min(items.len() - 1);
            }
            (KeyCode::Char(' '), _) | (KeyCode::Tab, _) => {
                items[selected].on = !items[selected].on;
                dirty = true;
            }
            (KeyCode::Enter, _) => {
                let item = &items[selected];
                if !item.settings {
                    show_details(ui, item)?;
                    continue;
                }
                let (id, name) = (item.id.clone(), item.name.clone());
                open_settings(ui, paths, config, &id, &name)?;
                // 设置页可能动了机器层的开关，回来要按新状态重摆一遍。
                let refreshed = crate::feature_sources::collect(config, paths);
                let mut fresh = feature_catalog::catalog(
                    &manifest,
                    &refreshed,
                    default_persona,
                    CatalogScope::Settings,
                    Some(config),
                );
                // 这一轮还没写盘的勾选要留住：按 id 把用户改过的贴回去。
                for item in &mut fresh {
                    if let Some(current) = items
                        .iter()
                        .find(|old| old.kind == item.kind && old.id == item.id)
                    {
                        item.on = current.on;
                    }
                }
                items = fresh;
                selected = selected.min(items.len().saturating_sub(1));
            }
            _ => {}
        }
    }

    if dirty {
        feature_catalog::apply_selection(&mut manifest, &items, &sources, default_persona);
        feature_catalog::apply_machine_switches(
            config,
            &feature_catalog::newly_ticked(&initial, &items),
        );
        // 不在这儿写盘：攒起来，跟配置一起走「保存并退出」。
        pending.set_manifest(&scope, manifest);
    }
    Ok(())
}

/// 一件功能的「怎么配」。子系统各有各的去处，其余走插件设置表单。
/// 一行的完整说明。表上那一列按宽度截断（说明长短不由我们定），截掉的部分
/// 得有地方看得到——脚本与技能没有设置页，回车就落在这儿。
fn show_details(ui: &mut Ui, item: &FeatureItem) -> Result<()> {
    let kind = section_of(item.kind);
    let origin = match item.kind {
        FeatureKind::Script | FeatureKind::Skill => Some(if item.builtin {
            t("bundled with Miyu", "Miyu 自带")
        } else {
            t("yours", "你自己加的")
        }),
        _ => None,
    };
    let mut text = format!("{}\n\n{}", item.name, item.hint.trim());
    let mut tail = vec![format!("{}: {kind}", t("Kind", "类别"))];
    if let Some(origin) = origin {
        tail.push(format!("{}: {origin}", t("Origin", "来源")));
    }
    if item.id != item.name {
        tail.push(format!("id: {}", item.id));
    }
    if item.on && item.machine_on == Some(false) {
        tail.push(
            t(
                "This persona has it on, but the machine-wide switch is off, so it is not live.",
                "这个人格勾着，但机器级开关关着，所以并没有生效。",
            )
            .to_string(),
        );
    }
    text.push_str("\n\n");
    text.push_str(&tail.join("\n"));
    message(ui, &text)
}

fn open_settings(
    ui: &mut Ui,
    paths: &MiyuPaths,
    config: &mut AppConfig,
    id: &str,
    display_name: &str,
) -> Result<()> {
    match id {
        // 语音有一整套自己的菜单（唤醒、听写、TTS、音色）。
        "voice" => edit_voice(ui, paths, config),
        _ => edit_plugin_detail(ui, config, id, display_name),
    }
}
