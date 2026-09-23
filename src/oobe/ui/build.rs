//! 每一屏的内容：把 [`App`] 的状态变成一串行。不管布局，不管星空。

use super::widgets::{body_w, clip, ln, nil, pad, Cx, View, NAME_COL};
use super::{pfocus, App, Prov, Screen, HINT_AT};
use crate::oobe::providers::PROTOCOLS;
use miyu_base::config::feature_catalog::FeatureKind;
use miyu_base::terminal::palette::{BLUE, DIM, FAINT, GOLD, GREEN, INK};
use miyu_base::terminal::starfield::fade;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

pub(super) fn build(app: &App, cx: &Cx) -> View {
    let theme = app.theme;
    let mut sticky: Vec<Line> = Vec::new();
    let mut body: Vec<Line> = Vec::new();
    let mut cursor_row = 0usize;
    let mut caret: Option<(usize, usize)> = None;
    let mut footer: Option<Line> = None;
    let mut footer_caret: Option<usize> = None;
    let mut counter: Option<String> = None;
    let mut keys: Vec<(&str, &str)> = Vec::new();

    match app.screen {
        // ── 00 欢迎 ────────────────────────────────────────────
        // 自检照跑（后面几屏要用探到的东西），但**不显示**——开场只留
        // 一片星空、一个 MIYU、一句话。`MIYU_OOBE_VERBOSE=1` 能调出来看。
        Screen::Welcome => {
            if std::env::var_os("MIYU_OOBE_VERBOSE").is_some() {
                for row in &app.loader.rows {
                    let elapsed = if row.micros >= 1000 {
                        format!("{:.1} ms", row.micros as f64 / 1000.0)
                    } else {
                        format!("{} µs", row.micros)
                    };
                    body.push(ln(vec![
                        Span::styled(
                            format!("{} ", if row.ok { theme.check() } else { "·" }),
                            if row.ok {
                                theme.fg(GREEN)
                            } else {
                                theme.fg(FAINT)
                            },
                        ),
                        Span::styled(pad(row.label, 10), theme.dim(DIM)),
                        Span::styled(clip(&row.value, 42), theme.fg(FAINT)),
                        Span::raw("  "),
                        Span::styled(elapsed, theme.fg(FAINT)),
                    ]));
                }
                if !app.loader.rows.is_empty() {
                    body.push(nil());
                }
            }
            if app.intro >= HINT_AT {
                // 淡入 + 呼吸：先用 24 帧从无到有，之后缓慢起落。
                let age = (app.intro - HINT_AT) as f32;
                let fade_t = (age / 24.0).min(1.0);
                let breath = ((app.tick as f32 / 13.0).sin() * 0.5 + 0.5) * 0.35 + 0.65;
                body.push(cx.txt(
                    "回车进入设置引导",
                    fade(theme, GOLD, fade_t * breath).add_modifier(Modifier::BOLD),
                ));
            }
        }

        // ── 01 人格 ───────────────────────────────────────────
        Screen::Persona => {
            sticky.push(cx.bold("人格"));
            sticky.push(nil());
            body.push(cx.radio(
                app.focus == pfocus::BUILTIN,
                !app.persona_custom,
                "用内置的 Miyu",
                "开箱即用",
                30,
            ));
            body.push(cx.radio(
                app.focus == pfocus::CUSTOM,
                app.persona_custom,
                "自己捏一个",
                "",
                30,
            ));
            cursor_row = 2 + app.focus.min(1);
            if app.persona_custom {
                for (slot, label, placeholder) in [
                    (pfocus::NAME, "名字", "给它起个名字"),
                    (
                        pfocus::SETTING,
                        "人格提示词  可留空",
                        "比如：你是一个乐于助人的软件工程师。",
                    ),
                ] {
                    body.push(nil());
                    body.push(cx.txt(label, theme.dim(DIM)));
                    let base = body.len();
                    let (lines, caret_row, caret_col) = cx.field(
                        if slot == pfocus::NAME {
                            &app.name
                        } else {
                            &app.setting
                        },
                        placeholder,
                        app.focus == slot,
                        app.editing && app.focus == slot,
                        false,
                    );
                    body.extend(lines);
                    if app.focus == slot {
                        cursor_row = body.len() - 1;
                        if app.editing {
                            caret = Some((base + caret_row, caret_col));
                        }
                    }
                }
                body.push(nil());
                body.push(cx.action(app.focus == pfocus::GO, "继续"));
                if app.focus == pfocus::GO {
                    cursor_row = body.len() - 1;
                }
            } else {
                body.push(nil());
                body.push(cx.txt("预置人格，不建文件。", theme.fg(FAINT)));
            }
            if app.editing {
                keys.push(("⏎", "结束编辑"));
                keys.push(("Alt+⏎", "换行"));
            } else {
                keys.push(("↑↓ jk", "移动"));
                keys.push(("⏎", "编辑 / 继续"));
                keys.push(("Esc", "上一步"));
                keys.push(("Ctrl+S", "跳过引导"));
            }
        }

        // ── 02 功能 ───────────────────────────────────────────
        Screen::Features => {
            let current_id = app
                .feats
                .get(app.feat_cur)
                .map(|item| item.id.clone())
                .unwrap_or_default();
            sticky.push(cx.two(
                vec![
                    Span::styled("自选功能", Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw("   "),
                    Span::styled("Tab 或空格开关 · Ctrl+A 全开/全关", theme.fg(GOLD)),
                ],
                body_w().saturating_sub(26),
                vec![
                    // 双列容不下 id 那一列，改成只报**光标底下**那一个。
                    Span::styled(pad(&clip(&current_id, 22), 22), theme.dim(DIM)),
                    Span::styled(
                        format!("{} / {}", app.on_count(), app.feats.len()),
                        theme.fg(FAINT),
                    ),
                ],
            ));
            if app.feats.is_empty() {
                body.push(cx.txt("这个人格下没有可选的功能，直接下一步。", theme.fg(FAINT)));
            }
            // 双列。行优先编号：第 i 项在第 i/2 行、第 i%2 列，所以 ↑↓ 是 ±2、←→ 是 ±1。
            let mut last_kind: Option<FeatureKind> = None;
            let mut pending: Vec<usize> = Vec::new();
            let flush = |body: &mut Vec<Line>, pend: &mut Vec<usize>, cur_row: &mut usize| {
                for pair in pend.chunks(2) {
                    let mut spans: Vec<Span> = Vec::new();
                    for (slot, index) in pair.iter().enumerate() {
                        let item = &app.feats[*index];
                        if slot == 1 {
                            spans.push(Span::raw("  "));
                        }
                        let cur = *index == app.feat_cur;
                        if cur {
                            *cur_row = body.len();
                        }
                        spans.push(Span::styled(
                            if cur {
                                theme.cursor().to_string()
                            } else {
                                "  ".into()
                            },
                            theme.fg(if cur { BLUE } else { FAINT }),
                        ));
                        spans.push(Span::styled(
                            if item.on { "[*]" } else { "[ ]" },
                            theme.fg(if item.on { BLUE } else { FAINT }),
                        ));
                        spans.push(Span::raw(" "));
                        // 没勾的不压暗:暗色是「不可用」的语气,这里只是还没选。
                        let mut style = if cur { theme.fg(BLUE) } else { Style::new() };
                        if cur {
                            style = theme.select(style);
                        }
                        spans.push(Span::styled(
                            pad(&clip(&item.name, NAME_COL - 1), NAME_COL),
                            style,
                        ));
                    }
                    body.push(Line::from(spans));
                }
                pend.clear();
            };
            for (index, item) in app.feats.iter().enumerate() {
                let section = App::section_of(item.kind);
                if last_kind.map(App::section_of) != Some(section) {
                    flush(&mut body, &mut pending, &mut cursor_row);
                    body.push(cx.divider(section));
                    last_kind = Some(item.kind);
                }
                pending.push(index);
            }
            flush(&mut body, &mut pending, &mut cursor_row);
            // 备注不占列：光标底下那项的说明单独一行钉在列表下方。
            if let Some(item) = app.feats.get(app.feat_cur) {
                if !item.hint.trim().is_empty() {
                    body.push(nil());
                    body.push(cx.txt(clip(&item.hint, body_w()), theme.fg(FAINT)));
                }
            }
            if !app.feats.is_empty() {
                counter = Some(format!("{}/{}", app.feat_cur + 1, app.feats.len()));
            }
            keys.push(("↑↓←→ jk", "移动"));
            keys.push(("Tab / 空格", "开关"));
            keys.push(("Ctrl+A", "全开/全关"));
            keys.push(("⏎", "下一步"));
            keys.push(("Esc", "上一步"));
        }

        // ── 03 认识你 ─────────────────────────────────────────
        Screen::Identity => {
            sticky.push(cx.bold(format!("{}怎么认识你", app.ai_name())));
            sticky.push(nil());
            let base = body.len();
            let (lines, caret_row, caret_col) = cx.field(
                &app.identity,
                "比如：我叫 shorin，是你的 Master。可留空。",
                app.focus == 0,
                app.editing,
                false,
            );
            body.extend(lines);
            cursor_row = body.len() - 1;
            if app.editing {
                caret = Some((base + caret_row, caret_col));
            }
            body.push(nil());
            body.push(cx.action(!app.editing && app.focus == 1, "继续"));
            if app.focus == 1 {
                cursor_row = body.len() - 1;
            }
            if app.editing {
                keys.push(("⏎", "结束编辑"));
                keys.push(("Alt+⏎", "换行"));
            } else {
                keys.push(("↑↓ jk", "移动"));
                keys.push(("⏎", "编辑 / 继续"));
                keys.push(("Esc", "上一步"));
            }
        }

        // ── 04 终端集成 ───────────────────────────────────────
        Screen::ShellHook => {
            sticky.push(cx.bold("终端集成"));
            sticky.push(nil());
            body.push(cx.txt(
                "装上之后在终端直接打字就能问，不用敲 miyu。",
                theme.dim(DIM),
            ));
            body.push(nil());
            let base = body.len();
            for (index, (shell, installed)) in app.shells.iter().enumerate() {
                let note = if *installed {
                    "已集成"
                } else if *shell == app.facts.current_shell {
                    "当前 shell"
                } else if *shell == "fish" {
                    "支持多行"
                } else {
                    "只支持单行"
                };
                body.push(cx.radio(
                    index == app.shell_cur,
                    index == app.shell_cur,
                    shell,
                    note,
                    20,
                ));
            }
            let skip = app.shells.len();
            body.push(cx.radio(
                app.shell_cur == skip,
                app.shell_cur == skip,
                "不装",
                "跳过集成",
                20,
            ));
            cursor_row = base + app.shell_cur;
            keys.push(("↑↓ jk", "选"));
            keys.push(("⏎", "下一步"));
            keys.push(("Esc", "上一步"));
        }

        // ── 05 接模型 ─────────────────────────────────────────
        Screen::Provider => match app.prov {
            Prov::Pick => {
                sticky.push(cx.bold("接模型"));
                sticky.push(nil());
                let col = app
                    .options
                    .iter()
                    .map(|option| option.label.width())
                    .max()
                    .unwrap_or(0)
                    + 6;
                let mut section = "";
                for (index, option) in app.options.iter().enumerate() {
                    if option.section != section {
                        section = option.section;
                        body.push(cx.divider(section));
                    }
                    if index == app.prov_cur {
                        cursor_row = body.len();
                    }
                    body.push(cx.radio(
                        index == app.prov_cur,
                        index == app.prov_cur,
                        &option.label,
                        &option.note,
                        col,
                    ));
                }
                counter = Some(format!("{}/{}", app.prov_cur + 1, app.options.len().max(1)));
                keys.push(("↑↓ jk", "选"));
                keys.push(("⏎", "确认"));
                keys.push(("Esc", "上一步"));
            }
            Prov::Form => {
                let (name, url) = app
                    .preset
                    .as_ref()
                    .map(|provider| (provider.display_name.clone(), provider.base_url.clone()))
                    .unwrap_or_default();
                sticky.push(cx.bold(format!("填 API key  {name}")));
                if !url.trim().is_empty() {
                    sticky.push(cx.txt(clip(&url, body_w()), theme.fg(FAINT)));
                }
                sticky.push(nil());
                if app.form_has_public() {
                    // opencode Zen:公共密钥的免费额度 / 自己的 key,二选一。
                    let cur = app.form_focus == 0;
                    let line = ln(vec![
                        Span::styled(
                            if cur {
                                theme.cursor().to_string()
                            } else {
                                "  ".into()
                            },
                            theme.fg(if cur { BLUE } else { FAINT }),
                        ),
                        Span::styled(
                            if app.public_quota { "[*]" } else { "[ ]" },
                            theme.fg(if app.public_quota { BLUE } else { FAINT }),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            "使用公共密钥的免费额度",
                            if cur { theme.fg(BLUE) } else { Style::new() },
                        ),
                        Span::styled(
                            if cur {
                                "   Tab / 空格 开关 · Ctrl+A 全开/全关"
                            } else {
                                ""
                            },
                            theme.fg(GOLD),
                        ),
                    ]);
                    if cur {
                        cursor_row = body.len();
                    }
                    body.push(if cur { cx.select(line) } else { line });
                    body.push(nil());
                }
                let key_index = app.form_key_index();
                let key_optional = app.form_has_public() && app.public_quota;
                body.push(cx.txt(
                    if key_optional {
                        "API key  可留空"
                    } else {
                        "API key"
                    },
                    theme.dim(DIM),
                ));
                let key_base = body.len();
                let (lines, caret_row, caret_col) = cx.field(
                    &app.api_key,
                    if key_optional {
                        "用公共密钥就不用填"
                    } else {
                        "粘贴进来，不回显"
                    },
                    app.form_focus == key_index,
                    app.editing && app.form_focus == key_index,
                    true,
                );
                body.extend(lines);
                if app.form_focus == key_index {
                    cursor_row = body.len() - 1;
                    if app.editing {
                        caret = Some((key_base + caret_row, caret_col));
                    }
                }
                body.push(nil());
                let action_index = app.form_action_index();
                body.push(cx.action(
                    !app.editing && app.form_focus == action_index,
                    "获取模型列表",
                ));
                if app.form_focus == action_index {
                    cursor_row = body.len() - 1;
                }
                if app.editing {
                    keys.push(("⏎", "结束编辑"));
                } else {
                    keys.push(("↑↓ jk", "移动"));
                    keys.push(("⏎", "编辑 / 继续"));
                    keys.push(("Esc", "返回"));
                }
            }
            Prov::CustomEp => {
                sticky.push(cx.bold("自定义供应商"));
                sticky.push(nil());
                for (slot, label, placeholder, mask) in [
                    (0usize, "显示名称", "比如：我的中转", false),
                    (1, "配置 ID", "小写字母、数字、连字符", false),
                    (2, "接口地址", "https://…", false),
                    (3, "API key", "粘贴进来，不回显；可留空", true),
                ] {
                    let value = match slot {
                        0 => &app.ep_name,
                        1 => &app.ep_id,
                        2 => &app.ep_url,
                        _ => &app.api_key,
                    };
                    body.push(cx.txt(label, theme.dim(DIM)));
                    let base = body.len();
                    let (lines, caret_row, caret_col) = cx.field(
                        value,
                        placeholder,
                        app.focus == slot,
                        app.editing && app.focus == slot,
                        mask,
                    );
                    body.extend(lines);
                    if app.focus == slot {
                        cursor_row = body.len() - 1;
                        if app.editing {
                            caret = Some((base + caret_row, caret_col));
                        }
                    }
                }
                body.push(nil());
                // 协议是选的不是填的。
                let (protocol_id, protocol_name) = PROTOCOLS[app.ep_proto];
                body.push(cx.two(
                    vec![
                        Span::styled(
                            if app.focus == 4 {
                                theme.cursor().to_string()
                            } else {
                                "  ".into()
                            },
                            theme.fg(if app.focus == 4 { BLUE } else { FAINT }),
                        ),
                        Span::styled("协议", theme.dim(DIM)),
                        Span::raw("  "),
                        Span::styled(
                            protocol_id,
                            if app.focus == 4 {
                                theme.fg(BLUE)
                            } else {
                                Style::new()
                            },
                        ),
                    ],
                    24,
                    vec![
                        Span::styled(protocol_name, theme.fg(FAINT)),
                        Span::styled(
                            if app.focus == 4 {
                                "   ←→ 切换"
                            } else {
                                ""
                            },
                            theme.fg(GOLD),
                        ),
                    ],
                ));
                if app.focus == 4 {
                    cursor_row = body.len() - 1;
                }
                body.push(nil());
                body.push(cx.action(!app.editing && app.focus == 5, "获取模型列表"));
                if app.focus == 5 {
                    cursor_row = body.len() - 1;
                }
                if app.editing {
                    keys.push(("⏎", "结束编辑"));
                } else {
                    keys.push(("↑↓ jk", "移动"));
                    keys.push(("⏎", "编辑 / 继续"));
                    keys.push(("Esc", "返回"));
                }
            }
            Prov::Fetching => {
                sticky.push(cx.bold("接模型"));
                sticky.push(nil());
                let name = app
                    .pending
                    .as_ref()
                    .map(|provider| provider.display_name.clone())
                    .unwrap_or_default();
                body.push(ln(vec![
                    Span::styled(theme.spinner(app.tick / 3), theme.fg(BLUE)),
                    Span::raw("  "),
                    Span::styled(format!("正在向 {name} 拉模型列表…"), theme.dim(DIM)),
                ]));
                body.push(nil());
                body.push(cx.txt("本机 CLI 要联网取目录，最长可能等 20 秒。", theme.fg(FAINT)));
                keys.push(("Esc", "取消"));
            }
            Prov::PickModel => {
                let name = app
                    .pending
                    .as_ref()
                    .map(|provider| provider.display_name.clone())
                    .unwrap_or_default();
                let visible = app.visible_models();
                let filtered = !app.model_query.is_empty();
                sticky.push(cx.two(
                    vec![Span::styled(
                        format!("选个模型  {name}"),
                        Style::new().add_modifier(Modifier::BOLD),
                    )],
                    body_w().saturating_sub(10),
                    vec![Span::styled(
                        if filtered {
                            format!("匹配 {} / {}", visible.len(), app.models.len())
                        } else {
                            format!("共 {} 个", app.models.len())
                        },
                        theme.fg(FAINT),
                    )],
                ));
                // vim 式搜索行：`/` 打开，落在底部横线上方（vim 命令行的位置）。
                if app.model_search || filtered {
                    footer_caret = app.model_search.then_some(1 + app.model_query.width());
                    footer = Some(ln(vec![
                        Span::styled("/", theme.fg(GOLD)),
                        Span::raw(app.model_query.clone()),
                    ]));
                }
                sticky.push(nil());
                let base = body.len();
                for (row, &index) in visible.iter().enumerate() {
                    let model = &app.models[index];
                    let cur = row == app.model_cur;
                    let line = ln(vec![
                        Span::styled(
                            if cur {
                                theme.cursor().to_string()
                            } else {
                                "  ".into()
                            },
                            theme.fg(if cur { BLUE } else { FAINT }),
                        ),
                        Span::styled(
                            if cur {
                                theme.radio_on()
                            } else {
                                theme.radio_off()
                            },
                            theme.fg(if cur { BLUE } else { FAINT }),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            clip(model, body_w().saturating_sub(6)),
                            if cur { theme.fg(BLUE) } else { Style::new() },
                        ),
                    ]);
                    body.push(if cur { cx.select(line) } else { line });
                }
                if visible.is_empty() {
                    body.push(cx.txt("没有匹配的模型，Backspace 改改看。", theme.fg(FAINT)));
                }
                cursor_row = base + app.model_cur.min(visible.len().saturating_sub(1));
                counter = Some(format!(
                    "{}/{}",
                    (app.model_cur + 1).min(visible.len().max(1)),
                    visible.len().max(1)
                ));
                if app.model_search {
                    keys.push(("⏎", "收起搜索"));
                    keys.push(("Esc", "清除"));
                    keys.push(("↑↓", "选"));
                } else {
                    keys.push(("↑↓ jk", "选"));
                    keys.push(("/", "搜索"));
                    keys.push(("⏎", "确认并开始"));
                    keys.push((
                        "Esc",
                        if filtered {
                            "清除筛选"
                        } else {
                            "上一步"
                        },
                    ));
                }
            }
        },
    }

    if let Some(notice) = &app.notice {
        body.push(nil());
        body.push(ln(vec![
            Span::styled("! ", theme.fg(miyu_base::terminal::palette::CORAL)),
            Span::styled(
                clip(notice, body_w().saturating_sub(2)),
                theme.fg(miyu_base::terminal::palette::CORAL),
            ),
        ]));
    }
    let _ = INK;

    View {
        sticky,
        body,
        cursor_row,
        caret,
        footer: footer.into_iter().collect(),
        footer_caret,
        counter,
        keys: keys
            .into_iter()
            .map(|(key, label)| (key.to_string(), label.to_string()))
            .collect(),
    }
}
