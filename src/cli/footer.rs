//! REPL 底部的状态：footer 那一行，全屏下一行放不下时再加上它底下的用量行。
//!
//! footer 显示当前模式、provider/模型、思考变体，回合跑着时跟着声波和这一轮的计时。
//! token 计量（输出速度、上下文占用与窗口、会话累计与缓存命中率）跟在 footer 右端；
//! 全屏下一行放不下时才挪到输入框下面那一行（用户 09-24：放得下一行，放不下才分两行）。
//! 大厅窄框与行内模式没有那一行，照旧跟在 footer 右端、按优先级丢弃——模型名比累计
//! 数字重要，模式标签又比模型名重要。

use crate::cli::*;

fn footer_display_width(text: &str) -> usize {
    UnicodeWidthStr::width(render::strip_ansi_text(text).as_str())
}

#[derive(Clone, Debug)]
pub(in crate::cli) struct ReplFooterStatus {
    pub(in crate::cli) provider: String,
    pub(in crate::cli) model: String,
    pub(in crate::cli) mixed_models: bool,
    pub(in crate::cli) thinking: Option<String>,
    pub(in crate::cli) token_usage: render::TokenMeter,
    /// 回合运行中的盲文转轮帧号;None=空闲不显示。随 spinner tick 推进,
    /// set_footer 的权威覆盖(from_config 构造)自然回落 None。
    pub(in crate::cli) running_spinner: Option<usize>,
    /// 会话上挂着的目标（`/goal`）。画在输入框第一行的右端，不占 footer。
    pub(in crate::cli) goal: Option<miyu_core::ipc::GoalHint>,
    /// 这一轮从什么时候开始算（09-24：声波右边那个计时，是这一轮对话的总用时，不是
    /// 会话的）。权威那份在 `LiveReplTail::turn_started`，这里是每次换 footer 时同步
    /// 过来的副本（`goal` 就是整份覆盖时漏过的）。
    pub(in crate::cli) turn_started: Option<std::time::Instant>,
}

/// 用量那串数字画在哪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum UsagePlacement {
    /// 跟在 footer 右端：大厅窄框、行内模式——它们底下没有留给用量的那一行。
    FooterRight,
    /// 全屏：活动区底下本来就空着一行。`below` 为假（一行放得下，见
    /// [`usage_fits_on_footer_line`]）时用量跟在 footer 右端、那一行画成空的——之前挪
    /// 下去时画的字得擦掉；为真时用量单独画在那一行（[`repl_usage_line`]）。
    Fullscreen { below: bool },
}

/// 量一行放不放得下时给声波和计时留的宽：三个空格 + 五柱声波 + 空格 + `59m 59s`。
const RUNNING_ALLOWANCE: usize = 3 + 5 + 1 + 7;
/// 同一行里左右两段之间至少空这么宽，挨得再近就像一整串了。
const USAGE_GAP: usize = 3;

/// 全屏下用量放不放得回 footer 那一行（用户 09-24：放得下一行，放不下才分两行）。
///
/// 按「跑着」的样子量：声波加一段分钟级的计时，空闲时也照这个量——回合一开始一结束
/// 就不会在一行和两行之间来回换。供应商名不算在内：同一行里它是头一个让位的。模式、
/// 模型名、思考档位要整个留得下，用量也要整串（带速度和累计）。
pub(in crate::cli) fn usage_fits_on_footer_line(
    mode: PersonaLane,
    readonly: bool,
    footer: &ReplFooterStatus,
    cols: usize,
) -> bool {
    let bar = footer_display_width(&input_prompt_bar(mode));
    let label = if readonly {
        t("read-only", "只读")
    } else {
        mode.label()
    };
    let essential = footer_display_width(&repl_footer_left_parts(
        label,
        &footer.model,
        None,
        footer.thinking.as_deref().unwrap_or_default(),
    ));
    let usage = footer_display_width(&usage_text_fitting(footer, usize::MAX, 0));
    bar + essential + RUNNING_ALLOWANCE + USAGE_GAP + usage <= cols
}

/// 这一轮跑了多久：只在跑着（声波在动）时给，跑完就不显示（用户 09-24）。
/// `12s` / `1m 05s` / `1h 02m 05s`，和 `/goal` 提示、收段行同一个写法。
pub(in crate::cli) fn turn_clock_label(footer: &ReplFooterStatus) -> Option<String> {
    footer.running_spinner?;
    let started = footer.turn_started?;
    Some(miyu_base::durations::format_hms(started.elapsed()))
}

/// 这条车道的高亮色：输入框左侧那根粗线、footer 左下角的模式标签、输入框右上
/// 角那行 `/goal …`，三处是同一个颜色。
///
/// 收成一个来源是因为它们本来就必须一致——改主题时漏掉一处，屏幕四个角就对不
/// 上了。普通 = primary 蓝，开发 = tertiary 酒红（与 render/webui 的 tertiary
/// 同源）。
pub(in crate::cli) fn lane_accent_style(mode: PersonaLane) -> &'static str {
    match mode {
        PersonaLane::Active => "\x1b[1m\x1b[34m",
        PersonaLane::Dev => "\x1b[1m\x1b[35m",
    }
}

/// 输入框右上角那行 `/goal …` 的配色。
///
/// 不跟「已绑定沙盒」那些系统回执一样走暗灰：那是说过就算的一次性告知，而这行
/// 是常驻的状态灯，暗着就沉进星空里看不见了（用户 09-19：「这个是值得高亮的内
/// 容」）。用的是当前模式的高亮色——和左侧粗线、左下角模式标签同一个颜色，屏幕
/// 上这几处连成一气（用户 09-19 指定）。
pub(in crate::cli) fn goal_hint_style(mode: PersonaLane) -> &'static str {
    lane_accent_style(mode)
}

/// 输入框右上角那行 `/goal …`。没目标、或目标已完成就返回空串。
///
/// 长任务跑起来之后屏幕上一直只有正文，看不出「它还在自己往前跑吗、跑到第几轮
/// 了」（用户 09-19）。这行常驻提示就管这一件事，所以只说三个词：什么状态、第几
/// 轮、这个状态持续了多久。颜色另走 [`goal_hint_style`]——这里只出字，好让走查
/// 和单测比得了原文。
pub(in crate::cli) fn goal_hint_text(goal: Option<&miyu_core::ipc::GoalHint>) -> String {
    let Some(goal) = goal else {
        return String::new();
    };
    let running = goal.running();
    let state = match goal.phase.as_str() {
        "blocked" => t("blocked", "blocked"),
        _ if running => t("running", "running"),
        _ => t("paused", "paused"),
    };
    let mut text = format!("/goal {state}");
    // 轮数的说法跟 `/goal` 自己那份对齐（「进行中 · 第 3 轮」），两处说的是
    // 同一个数，长得也该一样。
    if goal.rounds > 0 {
        let rounds = goal.rounds;
        text.push_str(&if is_zh() {
            format!(" · 第 {rounds} 轮")
        } else {
            format!(" · round {rounds}")
        });
    }
    // 秒数只在真的往前跑时给：停着的时候那个数字每帧都一样，只会让人以为卡了。
    if running {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or_default();
        let elapsed = now.saturating_sub(goal.since_unix);
        if (0..86_400).contains(&elapsed) {
            // 时分秒(用户 09-23:原来写成 `1407s`,跑久了读不出是多久)。
            let elapsed = std::time::Duration::from_secs(elapsed as u64);
            text.push_str(&format!(" · {}", miyu_base::durations::format_hms(elapsed)));
        }
    }
    text
}

/// Σ is hidden entirely when nothing has been spent yet, so an empty session
/// does not carry a "Σ0" that means nothing.
pub(in crate::cli) fn meter_cumulative(cumulative: TurnTokens) -> render::TokenMeter {
    render::TokenMeter {
        cumulative_tokens: (cumulative.total > 0).then_some(cumulative.total),
        cumulative_prompt_tokens: cumulative.prompt,
        cumulative_cached_tokens: cumulative.cache_read,
        ..Default::default()
    }
}

impl ReplFooterStatus {
    pub(in crate::cli) fn from_config(
        config: &AppConfig,
        session_tokens: u64,
        cumulative: TurnTokens,
    ) -> Self {
        let active = config.active_provider_model_choices();
        let mixed_models = active.len() > 1;
        let (provider_id, model) = match active.as_slice() {
            [] => ("-".to_string(), t("None", "无").to_string()),
            [choice] => (
                choice.provider_id.clone(),
                short_model_name(&choice.model, &choice.provider_id),
            ),
            _ => ("mixed".to_string(), t("Mixed", "混合").to_string()),
        };

        let window = config.active_context_window_with_source().ok().flatten();
        Self {
            model,
            provider: provider_id,
            mixed_models,
            thinking: None,
            running_spinner: None,
            // 目标状态由轮询线程一秒一拍地喂（`LiveReplTail::tick_goal_hint`），
            // 配置构造这一路不知道。
            goal: None,
            turn_started: None,
            token_usage: render::TokenMeter {
                session_tokens,
                context_window: window.map(|(value, _)| value),
                context_window_assumed: matches!(
                    window,
                    Some((_, miyu_base::config::ContextWindowSource::Assumed))
                ),
                ..meter_cumulative(cumulative)
            },
        }
    }

    pub(in crate::cli) fn update_token_usage(
        &mut self,
        result: &miyu_core::llm::ChatResult,
        session_tokens: u64,
        context_window: Option<usize>,
        cumulative: TurnTokens,
    ) {
        if result.usage.is_some() {
            let turn = TurnTokens::from_usage(result.usage.as_ref());
            self.set_token_usage_with_cache(
                turn,
                GenerationSpeed::from_usage(result.usage.as_ref()),
                session_tokens,
                context_window,
                cumulative,
            );
        }
    }

    pub(in crate::cli) fn set_token_usage_with_cache(
        &mut self,
        turn: TurnTokens,
        speed: GenerationSpeed,
        session_tokens: u64,
        context_window: Option<usize>,
        cumulative: TurnTokens,
    ) {
        let live_extra = self.token_usage.live_extra_tokens;
        self.token_usage = render::TokenMeter {
            live_extra_tokens: live_extra,
            turn_tokens: turn.total,
            turn_prompt_tokens: turn.prompt,
            turn_cached_tokens: turn.cache_read,
            session_tokens,
            context_window,
            ..meter_cumulative(cumulative)
        }
        .with_generation_speed(speed);
    }

    pub(in crate::cli) fn update_session_tokens(&mut self, session_tokens: u64) {
        self.token_usage.session_tokens = session_tokens;
        self.token_usage.session_tokens_unknown = false;
    }

    /// 上下文读数；没数过（显示「—」）时是 `None`。
    pub(in crate::cli) fn session_tokens(&self) -> Option<u64> {
        (!self.token_usage.session_tokens_unknown).then_some(self.token_usage.session_tokens)
    }

    /// 这条车道的上下文还没数过（大厅里按 Tab 换过去、会话还没开）：显示成「—」。
    pub(in crate::cli) fn mark_session_tokens_unknown(&mut self) {
        self.token_usage.session_tokens = 0;
        self.token_usage.session_tokens_unknown = true;
    }

    /// 把界面上那份的 Σ 收回来，返回收回的数。
    ///
    /// 空闲时轮询只改界面上的 footer（见 `JobsFeed::cumulative`），主循环手里这份
    /// 是上次显式刷新时的。主循环每一圈都拿手里这份整份覆盖界面上的，不先收回来
    /// 就把较新的 Σ 盖回旧值，下一次轮询才又改回来（用户 09-23）。
    pub(in crate::cli) fn adopt_cumulative(&mut self, shown: &ReplFooterStatus) -> TurnTokens {
        self.token_usage.cumulative_tokens = shown.token_usage.cumulative_tokens;
        self.token_usage.cumulative_prompt_tokens = shown.token_usage.cumulative_prompt_tokens;
        self.token_usage.cumulative_cached_tokens = shown.token_usage.cumulative_cached_tokens;
        TurnTokens {
            total: self.token_usage.cumulative_tokens.unwrap_or(0),
            prompt: self.token_usage.cumulative_prompt_tokens,
            cache_read: self.token_usage.cumulative_cached_tokens,
        }
    }

    /// Σ 上那份「还没落进库里」的加数：正在跑的子代理。返回是否真的变了，
    /// 调用方据此决定要不要重画——并行几个子代理时它一秒能变好几次，
    /// 不看这个就会一直重画整条 footer。
    pub(in crate::cli) fn update_live_extra_tokens(&mut self, extra: u64) -> bool {
        let changed = self.token_usage.live_extra_tokens != extra;
        self.token_usage.live_extra_tokens = extra;
        changed
    }

    /// 回合中途的逐请求刷新:在(回合前的)基线上叠加回合累计。必须作用
    /// 在基线快照的克隆上,同一回合内可重复调用而不重复相加。
    pub(in crate::cli) fn apply_round_usage(
        &mut self,
        context_tokens: u64,
        turn: TurnTokens,
        speed: GenerationSpeed,
    ) {
        let meter = &mut self.token_usage;
        meter.turn_tokens = turn.total;
        meter.turn_prompt_tokens = turn.prompt;
        meter.turn_cached_tokens = turn.cache_read;
        meter.generation_tokens = speed.tokens;
        meter.generation_ms = speed.millis;
        if context_tokens > 0 {
            meter.session_tokens = context_tokens;
            meter.session_tokens_unknown = false;
        }
        let cumulative = meter.cumulative_tokens.unwrap_or(0) + turn.total;
        meter.cumulative_tokens = (cumulative > 0).then_some(cumulative);
        meter.cumulative_prompt_tokens += turn.prompt;
        meter.cumulative_cached_tokens += turn.cache_read;
    }

    /// `assumed` 必须跟着窗口值一起传：只更新数字、不更新出处，footer 就会拿
    /// 上一次的出处去解释这一次的数——切个会话或换个模型就错了。
    pub(in crate::cli) fn update_context_window(
        &mut self,
        context_window: Option<usize>,
        assumed: bool,
    ) {
        self.token_usage.context_window = context_window;
        self.token_usage.context_window_assumed = assumed;
    }

    /// Returns whether anything actually moved, so an idle tick only forces a
    /// redraw when the numbers changed.
    pub(in crate::cli) fn update_cumulative_tokens(&mut self, cumulative: TurnTokens) -> bool {
        let meter = meter_cumulative(cumulative);
        let changed = self.token_usage.cumulative_tokens != meter.cumulative_tokens
            || self.token_usage.cumulative_prompt_tokens != meter.cumulative_prompt_tokens
            || self.token_usage.cumulative_cached_tokens != meter.cumulative_cached_tokens;
        self.token_usage.cumulative_tokens = meter.cumulative_tokens;
        self.token_usage.cumulative_prompt_tokens = meter.cumulative_prompt_tokens;
        self.token_usage.cumulative_cached_tokens = meter.cumulative_cached_tokens;
        changed
    }

    pub(in crate::cli) fn reset_token_usage(
        &mut self,
        session_tokens: u64,
        context_window: Option<usize>,
    ) {
        self.token_usage = render::TokenMeter {
            session_tokens,
            context_window,
            ..Default::default()
        };
    }

    pub(in crate::cli) fn update_thinking_variant(&mut self, variant: Option<&str>) {
        self.thinking = if self.mixed_models {
            None
        } else {
            variant.map(str::to_string)
        };
    }
}

/// `readonly`:这个会话开着只读模式(09-23,Tab 切换),模式标签后面跟一段「只读」。
/// 它不放在 `ReplFooterStatus` 里:那份是被 `set_footer` 整份覆盖的,会话级的开关
/// 放进去每次覆盖都得记着带回来(`goal` 就是这么漏过的)。
pub(in crate::cli) fn repl_footer_line(
    mode: PersonaLane,
    readonly: bool,
    footer: &ReplFooterStatus,
    cols: usize,
    usage: UsagePlacement,
) -> String {
    let cols = cols.max(1);
    let bar = input_prompt_bar(mode);
    let bar_width = footer_display_width(&bar);
    let right_plain = match usage {
        UsagePlacement::FooterRight | UsagePlacement::Fullscreen { below: false } => {
            usage_text_fitting(footer, cols.saturating_sub(bar_width), 24)
        }
        UsagePlacement::Fullscreen { below: true } => String::new(),
    };
    let right = if right_plain.is_empty() {
        String::new()
    } else {
        format!("\x1b[2m{right_plain}\x1b[0m")
    };
    let right_width = footer_display_width(&right);
    let left_budget = cols.saturating_sub(bar_width.saturating_add(right_width).saturating_add(1));
    let left = repl_footer_left(mode, readonly, footer, left_budget);
    let gap = cols
        .saturating_sub(
            bar_width
                .saturating_add(footer_display_width(&left))
                .saturating_add(right_width),
        )
        .max(1);
    let line = format!("{bar}{left}{}{right}", " ".repeat(gap));
    // Even the fixed fields can exceed a tiny terminal. Keep the footer on
    // one row and pad it fully because spinner ticks overwrite without clearing.
    pad_row(&line, cols)
}

/// 全屏下一行放不下时 footer 底下那一行：用量右对齐，整行垫满（重画时不先擦）。
pub(in crate::cli) fn repl_usage_line(footer: &ReplFooterStatus, cols: usize) -> String {
    let cols = cols.max(1);
    let text = usage_text_fitting(footer, cols, 0);
    if text.is_empty() {
        return " ".repeat(cols);
    }
    let width = footer_display_width(&text);
    let line = format!(
        "{}\x1b[2m{text}\x1b[0m",
        " ".repeat(cols.saturating_sub(width))
    );
    pad_row(&line, cols)
}

fn pad_row(line: &str, cols: usize) -> String {
    let line = render::clip_to_display_width(line, cols);
    let padding = cols.saturating_sub(footer_display_width(&line));
    format!("{line}{}", " ".repeat(padding))
}

/// 用量那串字，按宽度降级：先丢输出速度，再丢累计，最后丢百分比，上下文表撑到最后。
/// `reserve` 是同一行上还要留给左边的列数（跟在 footer 右端时，模式和模型名至少要
/// 留出这么宽）。
fn usage_text_fitting(footer: &ReplFooterStatus, width: usize, reserve: usize) -> String {
    // The usage figures carry only the standing gauges — how much context is
    // left, and what the session has cost. The per-turn figure is transient and
    // already has its own home in the `Token:` line printed after each reply.
    let usage = render::TokenMeter {
        turn_tokens: 0,
        ..footer.token_usage
    };
    let mut text = String::new();
    for (with_speed, with_cumulative, with_percent) in [
        (true, true, true),
        (false, true, true),
        (false, false, true),
        (false, false, false),
    ] {
        let meter = render::TokenMeter {
            cumulative_tokens: usage.cumulative_tokens.filter(|_| with_cumulative),
            ..usage
        };
        text = render::format_token_usage_inline_opts(&meter, with_percent, with_speed);
        if footer_display_width(&text).saturating_add(reserve) <= width {
            break;
        }
    }
    text
}

pub(in crate::cli) fn repl_footer_left(
    mode: PersonaLane,
    readonly: bool,
    footer: &ReplFooterStatus,
    width: usize,
) -> String {
    let thinking = footer.thinking.as_deref().unwrap_or_default();
    let colored_thinking = (!thinking.is_empty()).then(|| primary_footer_text(thinking));
    let colored_thinking = colored_thinking.as_deref().unwrap_or_default();
    // 回合运行中,模型信息右侧是 Miyu 的声波律动(用户 08-20 选定):五柱
    // 波浪的高度与亮度随帧流动,颜色跟随模式主色(普通蓝/dev 酒红)。与
    // 模型信息之间隔三个空格,不进 " · " 序列(用户点名)。波浪右边紧跟这一轮
    // 的计时(用户 09-24),两样同进同退:跑完一起消失。
    let wave = footer.running_spinner.map(|frame| {
        let wave = sound_wave_frame(frame, mode == PersonaLane::Dev);
        match turn_clock_label(footer) {
            Some(clock) => format!("{wave} \x1b[2m{clock}\x1b[0m"),
            None => wave,
        }
    });
    let with_wave = |text: String| match wave.as_deref() {
        Some(wave) => format!("{text}   {wave}"),
        None => text,
    };
    let provider = format!("\x1b[2m{}\x1b[0m", footer.provider);
    // 只读开着时直接顶替模式那几个字(用户 09-23:「普通 · 只读」太长,车道看
    // 输入框竖条的颜色就知道)。窄屏时模式标签最后才裁,它同样一直看得见。
    let mode = if readonly {
        format!(
            "{}{}\x1b[0m",
            readonly_label_style(),
            t("read-only", "只读")
        )
    } else {
        colored_footer_mode_label(mode)
    };
    let full = with_wave(repl_footer_left_parts(
        &mode,
        &footer.model,
        Some(&provider),
        colored_thinking,
    ));
    if footer_display_width(&full) <= width {
        return full;
    }

    let compact = with_wave(repl_footer_left_parts(
        &mode,
        &footer.model,
        None,
        colored_thinking,
    ));
    if footer_display_width(&compact) <= width {
        return compact;
    }

    let wave_width = wave
        .as_deref()
        .map_or(0, |wave| 3 + footer_display_width(wave));
    let fixed_width = footer_display_width(&mode)
        .saturating_add(3)
        .saturating_add(wave_width)
        .saturating_add(if thinking.is_empty() {
            0
        } else {
            3 + footer_display_width(colored_thinking)
        });
    let model_budget = width.saturating_sub(fixed_width);
    let model = render::clip_to_display_width(&footer.model, model_budget);
    let left = with_wave(repl_footer_left_parts(
        &mode,
        &model,
        None,
        colored_thinking,
    ));
    render::clip_to_display_width(&left, width)
}

pub(in crate::cli) fn repl_footer_left_parts(
    mode: &str,
    model: &str,
    provider: Option<&str>,
    thinking: &str,
) -> String {
    let mut endpoint = model.to_string();
    if let Some(provider) = provider.filter(|provider| !provider.is_empty()) {
        if !endpoint.is_empty() {
            endpoint.push(' ');
        }
        endpoint.push_str(provider);
    }
    let mut parts = vec![mode.to_string(), endpoint];
    if !thinking.is_empty() {
        parts.push(thinking.to_string());
    }
    parts.join(" · ")
}

/// 声波律动帧:五柱波浪,正弦驱动高度,三档颜色全部取自终端 16 色盘里由
/// matugen 绑定的语义色,不碰 bright 位——用户的 kitty 模板里 color12(94)
/// 是写死的 `#a39ec4`,不随壁纸换色(09-05 用户实录:波峰颜色对不上)。
/// 普通模式:峰=primary(34 加粗)、中=secondary(96)、谷=secondary_fixed_dim
/// (36 加 dim);dev 模式整条走 tertiary(35)的加粗/正常/dim 三档。
/// 每帧相位步进 0.24 rad,配合 80ms 的 footer tick 约每秒 3 rad,与演示稿
/// 的流速一致。
pub(in crate::cli) fn sound_wave_frame(frame: usize, dev: bool) -> String {
    const LEVELS: [char; 7] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇'];
    let (hi, mid, low) = if dev {
        ("\x1b[1m\x1b[35m", "\x1b[35m", "\x1b[2m\x1b[35m")
    } else {
        ("\x1b[1m\x1b[34m", "\x1b[96m", "\x1b[2m\x1b[36m")
    };
    let t = frame as f32 * 0.24;
    let mut out = String::new();
    for i in 0..5 {
        let height = ((t - i as f32 * 0.9).sin() + 1.0) / 2.0;
        let glyph = LEVELS[((height * (LEVELS.len() - 1) as f32) as usize).min(LEVELS.len() - 1)];
        out.push_str(if height > 0.72 {
            hi
        } else if height > 0.35 {
            mid
        } else {
            low
        });
        out.push(glyph);
        out.push_str("\x1b[0m");
    }
    out
}

/// 「只读」用金色(用户 09-23):跟空会话模式行上提示按键(`Tab`、`Shift+Tab`)的
/// 那个金同源(`palette::GOLD`),而且按同一个色深降级——256 色的终端(macOS 自带
/// 的 Terminal.app 就是)里写死真彩色,状态行和大厅会是两种金。
pub(in crate::cli) fn readonly_label_style() -> String {
    let theme = miyu_base::terminal::palette::Theme::detect();
    format!(
        "\x1b[1m{}",
        theme.fg_ansi(miyu_base::terminal::palette::GOLD)
    )
}

pub(in crate::cli) fn colored_footer_mode_label(mode: PersonaLane) -> String {
    format!("{}{}\x1b[0m", lane_accent_style(mode), mode.label())
}

pub(in crate::cli) fn primary_footer_text(text: &str) -> String {
    format!("\x1b[1m\x1b[34m{text}\x1b[0m")
}

pub(in crate::cli) fn turn_meter(
    turn: TurnTokens,
    speed: GenerationSpeed,
    session_tokens: u64,
    context_window: Option<usize>,
    cumulative: TurnTokens,
) -> render::TokenMeter {
    render::TokenMeter {
        turn_tokens: turn.total,
        turn_prompt_tokens: turn.prompt,
        turn_cached_tokens: turn.cache_read,
        session_tokens,
        context_window,
        ..meter_cumulative(cumulative)
    }
    .with_generation_speed(speed)
}

/// footer 上的思考档位：会话作用域的模型池，加上这个会话钉住的档位（09-24：effort
/// 做成会话级）。几处重算 footer 的地方都走这里，别再各自 `from_config` 了事。
pub(in crate::cli) fn footer_thinking_summary(
    paths: &MiyuPaths,
    session_config: &AppConfig,
    session_id: &str,
) -> Result<Option<String>> {
    let mut client = OpenAiCompatibleClient::from_config(session_config, paths)?;
    client.apply_session_thinking_variants(paths, session_id);
    Ok(client.thinking_variant_summary())
}

/// The footer/status display must reflect the session's pinned model pool,
/// not just the global config.
pub(in crate::cli) fn footer_config_for_session(
    paths: &MiyuPaths,
    config: &AppConfig,
    session_id: &str,
) -> AppConfig {
    let mut config = config.clone();
    let Ok(store) = StateStore::new(paths) else {
        return config;
    };
    if let Ok(Some(models)) = store.session_model_override(session_id) {
        // 与 `apply_session_model_override` 同一道守卫:远端 REPL 走的是这条路,
        // 覆盖指向已删除的模型时曾让 `miyu normal` 整个起不来(08-28)。
        match config.usable_model_override(models) {
            Some(usable) => config.active_provider_models = Some(usable),
            None => crate::cli::model_cmds::drop_stale_model_override(&store, session_id),
        }
    }
    config
}
