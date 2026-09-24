//! 子代理面板怎么画：标题栏、各项排成行、一个子代理的内层时间线排成面板。
//!
//! 从 `subagent.rs` 搬来（09-24，那份超过了 800 行的目标线）。只搬不改。

use super::*;

/// 面板标题栏：名字 + 跑了多久（还没动静时说一声，免得看着像死的）。
pub(super) fn subagent_title(log: &SubagentLog, display: &str) -> String {
    let elapsed = log.started.map(|at| at.elapsed()).unwrap_or_default();
    let mut title = format!("{display} · {}", format_seconds(elapsed));
    // 烧了多少、跑了几个工具：一个子代理可能跑几分钟，标题上没有量就只剩
    // 「running」，看不出它是在干活还是卡住了。
    if let Some(stats) = log.stats.as_deref().filter(|text| !text.trim().is_empty()) {
        title.push_str(" · ");
        title.push_str(stats.trim());
    }
    if log.steps.is_empty() && log.reasoning.trim().is_empty() {
        title.push_str(&format!(" · {}", t("starting…", "启动中…")));
    }
    title
}

/// 子代理面板的内容：串起来的时间线，每一步各自包成块（面板里也能点开）。
///
/// 块 id **按位置复用**。这条路每收到一小段思考就要走一遍（一秒好几次），每次
/// 都新登记一批的话，登记处几秒就被刷爆——而淘汰是按 id 从小到大来的，最先被
/// 端掉的正是这个子代理自己那块覆盖层（它登记得最早）。表现出来就是"面板里不
/// 是流式刷新的"和"工具行点不开了"（用户实测）。后台任务面板那边早就是这么做
/// 的，这里漏了。
/// 面板里的一项：一步（要连线、可点开）、一段它说的话（整段照排），或者最前面
/// 那行「提示词」抬头（可点开，但不在时间线上：和第一步之间不连线、空一行）。
pub enum PanelEntry {
    Step(String),
    Text(Vec<String>),
    Header(String),
    /// 露在上一步抬头底下的那几行（详细档的思考全文、工具详情）。
    Tail(Vec<String>),
}

/// 把面板里的各项排成行：步与步之间连线，正文段上下各空一行、不连线。
///
/// **两块子代理面板共用它**：前台从事件攒步、后台从日志行攒步，取数的地方不同，
/// 「步与步之间怎么空行」这套规矩不该有两份（报告 §2.2 项 D——它原来确实是两份
/// 状态机，后台那份叫 `Previous`，四个状态；这份三个）。
pub fn thread_panel(entries: Vec<PanelEntry>) -> Vec<String> {
    let indent = indent();
    let mut lines = Vec::new();
    let mut previous_was_step = false;
    let mut previous_was_text = false;
    for entry in entries {
        match entry {
            PanelEntry::Header(line) => {
                lines.push(line);
                // 当作"前面是一段正文"：下一步之前空一行、不连线。
                previous_was_step = false;
                previous_was_text = true;
            }
            PanelEntry::Step(line) => {
                if previous_was_step {
                    lines.push(rail());
                } else if previous_was_text {
                    lines.push(String::new());
                }
                lines.push(line);
                previous_was_step = true;
                previous_was_text = false;
            }
            PanelEntry::Text(body) => {
                // 开头那一项不用先空一行：面板顶上凭空一行空白，看着像内容掉了
                //（后台那份状态机一直是这么做的，合并时取它）。
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.extend(body.into_iter().map(|line| format!("{indent}{line}")));
                previous_was_step = false;
                previous_was_text = true;
            }
            // 不点开也露在上一步抬头底下的那几行，连线从中间穿过去——和主线
            // `step_rows` 一个样子。它跟着上一步走，所以前面不另起连线。
            PanelEntry::Tail(body) => {
                let prefix = rail_prefix();
                lines.extend(body.into_iter().map(|line| format!("{prefix}{line}")));
            }
        }
    }
    lines
}

/// 面板跟着主线走的两样设置，由 `publish_subagent` 交进来（`subagent_lines` 手上
/// 没有渲染器）。
#[derive(Clone, Copy)]
pub(super) struct PanelLook {
    /// 「思考中」底下那扇窗露几行（`display.thinking_scroll_lines`，0 = 不开窗）。
    pub(super) thought_lines: usize,
    /// 「展开思考内容」开着：正在想的那一块本来就是展开的，不另开窗。
    pub(super) expand_thought: bool,
}

pub(super) fn subagent_lines(log: &mut SubagentLog, look: PanelLook) -> Vec<String> {
    if log.step_blocks.len() > log.steps.len() {
        // 步被从前面裁过，位置对不上了，重来一轮。
        log.step_blocks.clear();
    }
    let mut entries = Vec::with_capacity(log.steps.len() + 2);
    for (index, step) in log.steps.iter().enumerate() {
        if step.kind == StepKind::Speech {
            entries.push(PanelEntry::Text(step.body.clone()));
            continue;
        }
        let id = if step.body.is_empty() {
            None
        } else {
            let detail = step_detail(step);
            match log.step_blocks.get(index).copied() {
                Some(id) => {
                    blocks::update(id, String::new(), detail);
                    Some(id)
                }
                None => {
                    let id = blocks::register(detail);
                    if let Some(id) = id {
                        // 位置要对齐：正文为空的那些步不登记，用 0 占位。
                        while log.step_blocks.len() < index {
                            log.step_blocks.push(0);
                        }
                        log.step_blocks.push(id);
                    }
                    id
                }
            }
        };
        // **走 `step_rows`**：抬头、底下露着的那几行（命令预览）、块的起止都在它
        // 里面，和主线、后台面板一份。尾巴另推一项的话它落在块外面：点开只换掉抬头，
        // 命令出现两遍——后台面板 09-17 就踩过（用户逐条报的 1/2/3）。
        let row = step_rows(step, id.filter(|id| *id != 0));
        // 「提示词」是抬头，不进时间线（用户：提示词 tag 行可以不参与 timeline）。
        if index == 0 && log.has_prompt {
            entries.push(PanelEntry::Header(row));
        } else {
            entries.push(PanelEntry::Step(row));
        }
    }
    // 正在跑的内层工具 / 正在流参数的那一个，各露一行——和主线的 live 区一个
    // 规矩。面板不归转轮管（它按块版本刷新），所以这两行是静态文字。
    if let Some((glyph, display, peek, since, tail)) = &log.running {
        let mut label = format!(
            "{display} · {} · {}",
            t("running", "运行中"),
            format_seconds(since.elapsed())
        );
        if let Some(peek) = peek {
            label.push_str(PEEK_SEP);
            label.push_str(peek);
        }
        entries.push(PanelEntry::Step(panel_live_step_line(glyph, &label)));
        // 跑着的命令底下也露命令，和主线跑着的那一步一个样子。
        if !tail.is_empty() {
            entries.push(PanelEntry::Tail(tail.clone()));
        }
    } else if let Some((phase, glyph, since)) = &log.preparing {
        entries.push(PanelEntry::Step(panel_live_step_line(
            glyph,
            &format!("{phase} · {}", format_seconds(since.elapsed())),
        )));
    }
    // 还在想的那一段也露一行，不然「正在思考」期间面板看着是死的。
    //
    // 这一行**也要能点开**：它常常是面板里最下面那一行，而正在想什么恰恰是
    // 此刻最值得看的（用户实测：浮层内最下面一行无法交互）。块 id 存在
    // `live_block` 里复用，每刷新一次只更新内容。
    if !log.reasoning.trim().is_empty() {
        // 「展开思考内容」关着、开了窗：抬头照主线写 `思考中 · 3.2s`，底下露最近几行
        //（主线那扇窗的规矩，用户 09-17 定的；面板里原来只有一截单行窥视，用户
        // 09-24：「思考的预览也没有」）。开着的话这一块本来就是展开的，不另开窗。
        let window = (!look.expand_thought && look.thought_lines > 0)
            .then(|| panel_thought_window(&log.reasoning, look.thought_lines))
            .filter(|rows| !rows.is_empty());
        let head = match &window {
            Some(_) => format!(
                "{} · {}",
                t("thinking", "思考中"),
                format_seconds(
                    log.reasoning_since
                        .map(|at| at.elapsed())
                        .unwrap_or_default()
                )
            ),
            None => format!(
                "{}{PEEK_SEP}{}",
                t("thinking", "思考中"),
                peek_tail(&log.reasoning, panel_step_width())
            ),
        };
        let line = panel_live_step_line(glyph_think(), &head);
        let detail = {
            let indent = indent();
            let mut lines = vec![line.clone(), String::new()];
            log.reasoning_rows.sync(&log.reasoning, detail_width());
            lines.extend(
                log.reasoning_rows
                    .range(0, usize::MAX)
                    .into_iter()
                    .map(|piece| {
                        format!("{THOUGHT_BODY_STYLE}{indent}{DETAIL_INDENT}{piece}\x1b[0m")
                    }),
            );
            lines.push(String::new());
            lines
        };
        let id = match log.live_block {
            Some(id) => {
                blocks::update(id, String::new(), detail);
                Some(id)
            }
            None => {
                let id = blocks::register(detail);
                log.live_block = id;
                id
            }
        };
        entries.push(PanelEntry::Step(match id {
            Some(id) => format!("{}{line}{}", blocks::begin_marker(id), blocks::END_MARKER),
            None => line,
        }));
        if let Some(rows) = window {
            entries.push(PanelEntry::Tail(rows));
        }
    }
    // 还在说的那段话排在最底下——它是此刻正在发生的事。说完的会被
    // `seal_subagent_speech` 封成一步，按时序留在该在的位置上。
    if !log.speech.trim().is_empty() {
        entries.push(PanelEntry::Text(render_speech_lines(
            log.speech.trim(),
            detail_width(),
        )));
    }
    thread_panel(entries)
}
