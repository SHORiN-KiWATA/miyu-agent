use super::clip_to_display_width;
use super::live_area::{LiveArea, Rewrite};
use anyhow::Result;
use std::io::{self, IsTerminal, Write};

const WIDTH: usize = 7;
const TRAIL_LEN: usize = 6;
const HOLD_END: usize = 9;
const HOLD_START: usize = 30;
pub(crate) use miyu_base::terminal::SPINNER_INTERVAL;
const MIN_FADE_ALPHA: f64 = 0.12;
const ACTIVE_DOTS: [&str; TRAIL_LEN] = ["▪", "▪", "▫", "▫", "·", "·"];
const INACTIVE_DOT: &str = "·";
const BRAILLE_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn braille_frame(frame: usize) -> &'static str {
    BRAILLE_FRAMES[frame % BRAILLE_FRAMES.len()]
}

/// Marks a sub-phase line as a live block header that carries its own
/// animated spinner glyph (parallel subagents: one spinner per block).
/// When any sub-phase line starts with this marker the spinner renders in
/// block mode: marker lines get the glyph, blank lines are preserved as
/// block separators, and the phase line is not rendered.
pub const BLOCK_MARKER: char = '\u{1}';

/// 同样是块模式的一行，但这一帧**不画转轮**：标记那一格连同它后面那一格留空，
/// 别的行的列位不变。思考的抬头滚出屏幕之后正文行上就不再挂转轮（用户 09-17：
/// 转轮跟着正文往下走反而碍眼）——可 live 区还得按块模式画，才不会冒出一行相位。
pub const BLOCK_MARKER_IDLE: char = '\u{2}';

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SpinnerStyle {
    Scanner,
    Braille,
}

#[derive(Clone, Copy)]
struct ScannerState {
    active_position: usize,
    is_holding: bool,
    hold_progress: usize,
    hold_total: usize,
    movement_progress: usize,
    movement_total: usize,
    is_moving_forward: bool,
}

pub struct WaitSpinner {
    phase: String,
    sub_phase: Option<String>,
    style: SpinnerStyle,
    frame: usize,
    /// 块模式上一帧每一行「原样 → 画好的样子」。见 [`WaitSpinner::block_rows`]。
    row_cache: Vec<CachedRow>,
    /// `row_cache` 是按多宽裁的。宽度一变整份作废。
    row_cache_width: usize,
    /// 画在哪、上一帧画了什么（`live_area`）。
    area: LiveArea,
}

/// 块模式的一行：进来时什么样、画出去什么样、占几列。
struct CachedRow {
    source: String,
    painted: String,
    width: usize,
}

impl WaitSpinner {
    pub(crate) fn supported() -> bool {
        // daemon 往别人的 tty 回写时 stdout 不是终端，但那条线程报了宽度——
        // 就是在往终端画，转轮照转。
        io::stdout().is_terminal() || crate::render::cols_override_active()
    }

    pub fn start(phase: String, style: SpinnerStyle) -> Self {
        Self {
            phase,
            sub_phase: None,
            style,
            frame: 0,
            row_cache: Vec::new(),
            row_cache_width: 0,
            area: LiveArea::default(),
        }
    }

    pub(crate) fn set_phase(&mut self, phase: String) {
        self.phase = phase;
    }

    pub fn set_sub_phase(&mut self, sub_phase: Option<String>) {
        self.sub_phase = sub_phase;
    }

    pub fn tick(&mut self, writer: &mut impl Write) -> Result<()> {
        self.tick_in(writer, true)
    }

    /// 把 live 区最上面的几行「就地」交给 scrollback：内容不动（哪一行和已画的不一样
    /// 才重写那一行——一般只有带转轮字形的第一行），只是从此不再归转轮管，下一帧从
    /// 它们底下起手。思考正文高过一屏、顶上的行往 scrollback 滚时用它：以前是收掉
    /// 转轮擦整片、写那一行、下一帧再整片重画，中间隔着空当，每滚一行闪一下
    /// （用户实测）。有软折行、或行数对不上时办不到，返回 false，调用方走擦了重画。
    pub fn commit_leading_rows(
        &mut self,
        writer: &mut impl Write,
        rows: &[String],
        terminal_width: usize,
    ) -> Result<bool> {
        self.area.commit_leading_rows(writer, rows, terminal_width)
    }

    /// 画一帧。`synchronized` = 自己裹一对同步输出标记；调用方已经在块里就传假。
    pub fn tick_in(&mut self, writer: &mut impl Write, synchronized: bool) -> Result<()> {
        let terminal_width = crate::render::terminal_cols(120);
        let (lines, widths) = self.frame_rows(terminal_width);
        if !lines.is_empty() {
            self.area.paint(
                writer,
                lines,
                widths,
                terminal_width,
                synchronized,
                Rewrite::FromFirstChange,
            )?;
        }
        let total = total_frames_for_style(self.style);
        self.frame = (self.frame + 1) % total.max(1);
        Ok(())
    }

    /// 这一帧的每一行（含转义）与各自的显示宽度。
    fn frame_rows(&mut self, terminal_width: usize) -> (Vec<String>, Vec<usize>) {
        let block = self
            .sub_phase
            .take()
            .filter(|sub| sub.contains(BLOCK_MARKER) || sub.contains(BLOCK_MARKER_IDLE));
        if let Some(sub) = block {
            let rows = self.block_rows(&sub, terminal_width);
            self.sub_phase = Some(sub);
            return rows;
        }
        let (output, _) = render_frame_at_width(self.frame, self, terminal_width);
        let lines = output.lines().map(str::to_string).collect::<Vec<_>>();
        let widths = lines
            .iter()
            .map(|line| super::command_ansi_width(line))
            .collect();
        (lines, widths)
    }

    /// 块模式（全屏时间线）的每一行。
    ///
    /// 全屏下这一段是整条时间线：一轮跑上百步就是几百行，每一拍（40ms）都要裁宽、
    /// 上色、量宽度——09-23 量尺 150 步时这一项一拍 3.3ms（debug），占了拼帧的八成。
    /// 可一拍里真变的只有挂转轮字形的那一行和正在长的那几行：其余行和上一帧同一
    /// 位置、同样宽度下一模一样，直接用上一帧画好的。
    fn block_rows(&mut self, sub: &str, terminal_width: usize) -> (Vec<String>, Vec<usize>) {
        let usable = terminal_width.saturating_sub(1).max(1);
        if self.row_cache_width != usable {
            self.row_cache.clear();
            self.row_cache_width = usable;
        }
        let glyph = paint_secondary(braille_frame(self.frame));
        let mut lines = Vec::new();
        let mut widths = Vec::new();
        for (index, source) in sub.lines().enumerate() {
            // 挂转轮字形的行每一帧都不一样，照算。
            let animated = source.contains(BLOCK_MARKER);
            let hit = self
                .row_cache
                .get(index)
                .filter(|cached| !animated && cached.source == source);
            if let Some(cached) = hit {
                lines.push(cached.painted.clone());
                widths.push(cached.width);
                continue;
            }
            let painted = block_row(source, usable, &glyph);
            let width = super::command_ansi_width(&painted);
            let row = CachedRow {
                source: source.to_string(),
                painted: painted.clone(),
                width,
            };
            match self.row_cache.get_mut(index) {
                Some(slot) => *slot = row,
                None => self.row_cache.push(row),
            }
            lines.push(painted);
            widths.push(width);
        }
        self.row_cache.truncate(lines.len());
        (lines, widths)
    }

    pub fn stop(&mut self, writer: &mut impl Write) -> Result<()> {
        self.stop_in(writer, true)
    }

    /// 收掉转轮那几行。`synchronized` = 自己裹一对同步输出标记；调用方已经在块里
    /// 就传假（2026 是布尔不是栈，里层的结束会把外层提前结掉）。
    pub fn stop_in(&mut self, writer: &mut impl Write, synchronized: bool) -> Result<()> {
        self.area.clear(writer, synchronized)
    }
}

fn render_frame_at_width(
    frame: usize,
    state: &WaitSpinner,
    terminal_width: usize,
) -> (String, u16) {
    if let Some(sub) = &state.sub_phase {
        if sub.contains(BLOCK_MARKER) || sub.contains(BLOCK_MARKER_IDLE) {
            return render_block_frame(frame, sub, terminal_width);
        }
    }
    let (spinner_prefix, spinner_width) = match state.style {
        SpinnerStyle::Scanner => {
            let scanner = scanner_state(frame % total_frames_scanner());
            (
                (0..WIDTH)
                    .map(|char_index| render_cell(char_index, scanner))
                    .collect::<String>(),
                WIDTH,
            )
        }
        SpinnerStyle::Braille => (paint_secondary(braille_frame(frame)), 1),
    };
    let usable = terminal_width.saturating_sub(1).max(1);
    // 转轮落在第 0 列、文字从第 2 列起——和时间线里跑着的那一行（转轮在左边距、
    // logo 在第 2 列）同一列。原来点阵档退两格，一进时间线转轮就往左跳两格
    //（用户实测：shellhook 最开始的转轮和进时间线后的不在同一列）。
    let phase_width = usable.saturating_sub(spinner_width + 1);
    let phase = clip_to_display_width(&state.phase, phase_width);
    let main_line = if phase.is_empty() {
        spinner_prefix
    } else {
        format!(
            "{} {}",
            spinner_prefix,
            paint_for_style(&phase, state.style)
        )
    };
    let mut lines = vec![main_line];
    match &state.sub_phase {
        Some(sub) if !sub.trim().is_empty() => {
            for line in sub.lines().filter(|line| !line.trim().is_empty()) {
                let line = clip_to_display_width(line, usable.saturating_sub(2));
                lines.push(format!("  {}", paint_for_style(&line, state.style)));
            }
        }
        _ => {}
    }
    let count = lines.len().min(u16::MAX as usize) as u16;
    (lines.join("\n"), count)
}

/// Renders the multi-block live layout: each `BLOCK_MARKER` line is a
/// running block header with its own animated glyph; blank lines separate
/// blocks; other lines already carry their own indentation (running-block
/// detail lines are indented by the builder, settled blocks are flush).
fn render_block_frame(frame: usize, sub: &str, terminal_width: usize) -> (String, u16) {
    let usable = terminal_width.saturating_sub(1).max(1);
    let glyph = paint_secondary(braille_frame(frame));
    let lines = sub
        .lines()
        .map(|line| block_row(line, usable, &glyph))
        .collect::<Vec<_>>();
    let count = lines.len().min(u16::MAX as usize) as u16;
    (lines.join("\n"), count)
}

/// 块模式的一行画成什么样。`glyph` 是这一帧的转轮字形（已上色）。
fn block_row(line: &str, usable: usize, glyph: &str) -> String {
    // 标记前面的缩进要留着：点阵转轮得落在 logo 那一列上，而不是行首。
    // 时间线就靠这个让「正在跑的那一步」原地把图标换成进度点阵。
    if let Some(index) = line.find(BLOCK_MARKER) {
        let (indent, rest) = line.split_at(index);
        let rest = &rest[BLOCK_MARKER.len_utf8()..];
        let width = usable.saturating_sub(indent.chars().count() + 2);
        let rest = clip_to_display_width(rest, width);
        format!(
            "{indent}{glyph} {}",
            paint_for_style(&rest, SpinnerStyle::Braille)
        )
    } else if let Some(index) = line.find(BLOCK_MARKER_IDLE) {
        // 转轮那一格留空：列位和带转轮的行一样，只是这一帧没有它。
        let (indent, rest) = line.split_at(index);
        let rest = &rest[BLOCK_MARKER_IDLE.len_utf8()..];
        let width = usable.saturating_sub(indent.chars().count() + 2);
        let rest = clip_to_display_width(rest, width);
        format!(
            "{indent}  {}",
            paint_for_style(&rest, SpinnerStyle::Braille)
        )
    } else if line.trim().is_empty() {
        String::new()
    } else {
        let clipped = clip_to_display_width(line, usable);
        paint_for_style(&clipped, SpinnerStyle::Braille)
    }
}

fn render_cell(char_index: usize, state: ScannerState) -> String {
    match color_index(char_index, state) {
        Some(index) if index < TRAIL_LEN => paint_active_dot(index),
        _ => paint_inactive_dot(),
    }
}

fn paint_active_dot(index: usize) -> String {
    let dot = ACTIVE_DOTS[index.min(ACTIVE_DOTS.len() - 1)];
    match index {
        0 => format!("\x1b[38;5;10m{dot}\x1b[0m"),
        1 => format!("\x1b[38;5;10m{dot}\x1b[0m"),
        2 => format!("\x1b[2m\x1b[38;5;10m{dot}\x1b[0m"),
        3 => format!("\x1b[2m\x1b[38;5;10m{dot}\x1b[0m"),
        _ => format!("\x1b[2m\x1b[38;5;10m{dot}\x1b[0m"),
    }
}

fn paint_inactive_dot() -> String {
    format!("\x1b[2m\x1b[38;5;10m{INACTIVE_DOT}\x1b[0m")
}

fn total_frames_scanner() -> usize {
    WIDTH + HOLD_END + (WIDTH - 1) + HOLD_START
}

fn total_frames_for_style(style: SpinnerStyle) -> usize {
    match style {
        SpinnerStyle::Scanner => total_frames_scanner(),
        SpinnerStyle::Braille => BRAILLE_FRAMES.len(),
    }
}

fn scanner_state(mut frame: usize) -> ScannerState {
    if frame < WIDTH {
        return ScannerState {
            active_position: frame,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: frame,
            movement_total: WIDTH,
            is_moving_forward: true,
        };
    }
    frame -= WIDTH;
    if frame < HOLD_END {
        return ScannerState {
            active_position: WIDTH - 1,
            is_holding: true,
            hold_progress: frame,
            hold_total: HOLD_END,
            movement_progress: 0,
            movement_total: 0,
            is_moving_forward: true,
        };
    }
    frame -= HOLD_END;
    if frame < WIDTH - 1 {
        return ScannerState {
            active_position: WIDTH - 2 - frame,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: frame,
            movement_total: WIDTH - 1,
            is_moving_forward: false,
        };
    }
    frame -= WIDTH - 1;
    ScannerState {
        active_position: 0,
        is_holding: true,
        hold_progress: frame,
        hold_total: HOLD_START,
        movement_progress: 0,
        movement_total: 0,
        is_moving_forward: false,
    }
}

fn color_index(char_index: usize, state: ScannerState) -> Option<usize> {
    let distance = if state.is_moving_forward {
        state.active_position as isize - char_index as isize
    } else {
        char_index as isize - state.active_position as isize
    };
    if state.is_holding {
        return usize::try_from(distance)
            .ok()
            .map(|distance| distance + state.hold_progress);
    }
    if distance == 0 {
        return Some(0);
    }
    if distance > 0 && distance < TRAIL_LEN as isize {
        return usize::try_from(distance).ok();
    }
    None
}

#[allow(dead_code)]
fn fade_factor(state: ScannerState) -> f64 {
    if state.is_holding && state.hold_total > 0 {
        let progress = (state.hold_progress as f64 / state.hold_total as f64).min(1.0);
        (1.0 - progress * (1.0 - MIN_FADE_ALPHA)).max(MIN_FADE_ALPHA)
    } else if !state.is_holding && state.movement_total > 0 {
        let denominator = state.movement_total.saturating_sub(1).max(1);
        let progress = (state.movement_progress as f64 / denominator as f64).min(1.0);
        MIN_FADE_ALPHA + progress * (1.0 - MIN_FADE_ALPHA)
    } else {
        1.0
    }
}

fn paint_secondary(text: &str) -> String {
    format!("\x1b[2m\x1b[36m{text}\x1b[0m")
}

fn paint_for_style(text: &str, style: SpinnerStyle) -> String {
    match style {
        SpinnerStyle::Scanner => format!("\x1b[38;5;10m{text}\x1b[0m"),
        SpinnerStyle::Braille => paint_secondary(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 行数没变时只重写变了的行：三行的块，第二帧只有转轮那一行动了。
    #[test]
    fn unchanged_rows_are_not_rewritten_between_ticks() {
        let mut spinner = WaitSpinner::start(String::new(), SpinnerStyle::Braille);
        spinner.set_sub_phase(Some(format!(
            "{BLOCK_MARKER}head\n  │ body one\n  │ body two"
        )));
        let mut first = Vec::new();
        spinner.tick(&mut first).unwrap();
        let mut second = Vec::new();
        spinner.tick(&mut second).unwrap();
        let clears = |bytes: &[u8]| String::from_utf8_lossy(bytes).matches("\x1b[2K").count();
        let first_text = String::from_utf8_lossy(&first);
        assert_eq!(clears(&first), 3, "{first_text:?}");
        // 一帧一个同步块：终端一次成帧，擦了还没写的中间态不露出来。
        assert!(
            first_text.starts_with("\x1b[?2026h") && first_text.ends_with("\x1b[?2026l"),
            "{first_text:?}"
        );
        let second_text = String::from_utf8_lossy(&second);
        assert_eq!(clears(&second), 1, "{second_text:?}");
        assert!(second_text.contains("head"), "{second_text:?}");
        assert!(!second_text.contains("body one"), "{second_text:?}");
        // 正文长了一行：只重写转轮那一行，新行用换行追加在末尾，不整片重画。
        spinner.set_sub_phase(Some(format!(
            "{BLOCK_MARKER}head\n  │ body one\n  │ body two\n  │ body three"
        )));
        let mut third = Vec::new();
        spinner.tick(&mut third).unwrap();
        let third_text = String::from_utf8_lossy(&third);
        assert_eq!(clears(&third), 2, "{third_text:?}");
        assert!(
            third_text.contains("body three") && !third_text.contains("body one"),
            "{third_text:?}"
        );
        assert!(
            third_text.contains("\x1b[2B") || third_text.matches("\x1b[1B").count() == 2,
            "{third_text:?}"
        );
    }

    /// 顶上的行就地交给 scrollback：只重写带转轮字形的那一行，账上划走，下一帧
    /// 从底下接着画（只有新的转轮行和变了的行）。
    #[test]
    fn leading_rows_are_committed_in_place_without_a_full_repaint() {
        let mut spinner = WaitSpinner::start(String::new(), SpinnerStyle::Braille);
        spinner.set_sub_phase(Some(format!("{BLOCK_MARKER}  │ one\n  │ two\n  │ three")));
        let mut first = Vec::new();
        spinner.tick(&mut first).unwrap();
        let clears = |bytes: &[u8]| String::from_utf8_lossy(bytes).matches("\x1b[2K").count();
        let mut commit = Vec::new();
        assert!(spinner
            .commit_leading_rows(&mut commit, &["  │ one".to_string()], 120)
            .unwrap());
        let commit_text = String::from_utf8_lossy(&commit);
        assert_eq!(clears(&commit), 1, "{commit_text:?}");
        assert!(
            commit_text.contains("\x1b[2A") && commit_text.contains("  │ one"),
            "{commit_text:?}"
        );
        assert_eq!(spinner.area.lines.len(), 2);
        // 下一帧：转轮挪到 two 上，three 没变。
        spinner.set_sub_phase(Some(format!("{BLOCK_MARKER}  │ two\n  │ three")));
        let mut next = Vec::new();
        spinner.tick(&mut next).unwrap();
        let next_text = String::from_utf8_lossy(&next);
        assert_eq!(clears(&next), 1, "{next_text:?}");
        assert!(
            next_text.contains("two") && !next_text.contains("three"),
            "{next_text:?}"
        );
        // 行数不够（整块都要交）就办不到。
        let mut none = Vec::new();
        assert!(!spinner
            .commit_leading_rows(
                &mut none,
                &["  │ two".to_string(), "  │ three".to_string()],
                120
            )
            .unwrap());
    }

    /// 空位标记：还是块模式，但这一帧没有转轮字形，那一格留空。
    #[test]
    fn idle_marker_keeps_block_mode_without_a_glyph() {
        let mut spinner = WaitSpinner::start("phase".to_string(), SpinnerStyle::Braille);
        spinner.set_sub_phase(Some(format!("{BLOCK_MARKER_IDLE}│ one\n  │ two")));
        let mut out = Vec::new();
        spinner.tick(&mut out).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains("phase"), "块模式不画相位行: {text:?}");
        assert!(
            !BRAILLE_FRAMES.iter().any(|glyph| text.contains(glyph)),
            "{text:?}"
        );
        assert!(text.contains("  \x1b[2m\x1b[36m│ one"), "{text:?}");
    }

    #[test]
    fn spinner_runs_at_least_twenty_four_frames_per_second() {
        assert!(SPINNER_INTERVAL <= std::time::Duration::from_millis(41));
    }

    fn make_spinner(phase: &str, sub_phase: Option<&str>, style: SpinnerStyle) -> WaitSpinner {
        WaitSpinner {
            phase: phase.to_string(),
            sub_phase: sub_phase.map(|s| s.to_string()),
            style,
            frame: 0,
            row_cache: Vec::new(),
            row_cache_width: 0,
            area: LiveArea::default(),
        }
    }

    #[test]
    fn render_frame_scanner_has_phase_without_face() {
        let spinner = make_spinner("思考", None, SpinnerStyle::Scanner);

        let (frame, lines) = render_frame(0, &spinner);

        assert!(frame.contains("思考"));
        assert!(frame.contains("\x1b[38;5;10m"));
        assert!(!frame.contains("\x1b[36m思考"));
        assert!(!frame.contains('('));
        assert_eq!(lines, 1);
    }

    #[test]
    fn render_frame_scanner_without_phase_has_no_separator() {
        let spinner = make_spinner("", None, SpinnerStyle::Scanner);

        let (frame, lines) = render_frame(0, &spinner);

        assert_eq!(crate::render::command_ansi_width(&frame), WIDTH);
        assert_eq!(lines, 1);
    }

    #[test]
    fn render_frame_braille_has_phase() {
        let spinner = make_spinner("~ 输入法诊断×1 运行中", None, SpinnerStyle::Braille);

        let (frame, lines) = render_frame(0, &spinner);

        assert!(frame.contains("输入法诊断"));
        assert!(frame.contains("⠋"));
        assert!(frame.contains("\x1b[2m\x1b[36m"));
        assert_eq!(lines, 1);
        // 转轮在第 0 列、文字从第 2 列起：和时间线里跑着的那一行同列。原来点阵
        // 档退两格，一进时间线转轮就往左跳（用户实测：shellhook 两个转轮不在同一列）。
        let plain = crate::render::strip_ansi_text(&frame);
        assert!(plain.starts_with("⠋ ~"), "转轮没落在第 0 列: {plain:?}");
    }

    #[test]
    fn render_frame_with_sub_phase_produces_two_lines() {
        let spinner = make_spinner(
            "~ 输入法诊断×1 运行中",
            Some("第 1 轮：诊断中"),
            SpinnerStyle::Scanner,
        );

        let (frame, lines) = render_frame(0, &spinner);

        assert!(frame.contains("输入法诊断"));
        assert!(frame.contains("第 1 轮"));
        assert_eq!(lines, 2);
    }

    #[test]
    fn detects_sub_phase_row_growth_before_tick() {
        let mut spinner = make_spinner(
            "~ Linux 游戏兼容性调查×1 运行中",
            Some("↳ Black Myth: Wukong"),
            SpinnerStyle::Braille,
        );
        let (rendered, _) = render_frame_at_width(0, &spinner, 80);
        spinner.area.widths = rendered
            .lines()
            .map(crate::render::command_ansi_width)
            .collect();
        assert!(!spinner.tick_changes_layout_at_width(80));

        spinner.set_sub_phase(Some(
            "↳ Black Myth: Wukong\n↳ 收集游戏兼容性信号".to_string(),
        ));

        assert!(spinner.tick_changes_layout_at_width(80));
    }

    #[test]
    fn long_unicode_phase_never_soft_wraps() {
        let spinner = make_spinner(
            &format!("思考：{}", "中文".repeat(40)),
            Some(&format!("↳ {}", "👨‍👩‍👧‍👦测试".repeat(30))),
            SpinnerStyle::Scanner,
        );
        for width in [20, 40, 80] {
            let (frame, lines) = render_frame_at_width(3, &spinner, width);
            assert_eq!(lines, 2);
            for line in frame.lines() {
                assert!(
                    crate::render::command_ansi_width(line) < width,
                    "line exceeded {width} columns: {line:?}"
                );
            }
        }
    }

    #[test]
    fn multiline_sub_phase_reports_every_rendered_row() {
        let spinner = make_spinner(
            "~ 子代理×1 运行中 · 4s",
            Some("↳ 查询磁盘占用\n↳ 工具 #2：运行命令 运行中"),
            SpinnerStyle::Braille,
        );

        let (frame, lines) = render_frame_at_width(0, &spinner, 80);

        assert_eq!(lines, 3);
        assert_eq!(frame.lines().count(), 3);
        assert_eq!(frame.matches("子代理×1").count(), 1);
        assert_eq!(frame.matches("4s").count(), 1);
    }

    #[test]
    fn braille_frames_loop_over_pattern() {
        let spinner = make_spinner("thinking", None, SpinnerStyle::Braille);

        let (f1, _) = render_frame(0, &spinner);
        let (f2, _) = render_frame(BRAILLE_FRAMES.len(), &spinner);

        assert_eq!(f1, f2);
    }

    #[test]
    fn scanner_frames_loop_over_pattern() {
        let spinner = make_spinner("thinking", None, SpinnerStyle::Scanner);

        let (f1, _) = render_frame(0, &spinner);
        let (f2, _) = render_frame(total_frames_scanner(), &spinner);

        assert_eq!(f1, f2);
    }

    #[test]
    fn scanner_has_trail_behind_active_position() {
        let state = scanner_state(4);

        assert_eq!(color_index(4, state), Some(0));
        assert_eq!(color_index(3, state), Some(1));
        assert_eq!(color_index(7, state), None);
    }

    #[test]
    fn active_and_inactive_dots_match_pr_style() {
        assert!(render_cell(4, scanner_state(4)).contains("▪"));
        assert!(paint_inactive_dot().contains(INACTIVE_DOT));
    }

    #[test]
    fn braille_cycles_through_all_frames() {
        let spinner = make_spinner("test", None, SpinnerStyle::Braille);

        let chars: std::collections::HashSet<&str> = (0..BRAILLE_FRAMES.len())
            .map(|i| {
                let (frame, _) = render_frame(i, &spinner);
                let first_char = frame.split_whitespace().next().unwrap_or("");
                BRAILLE_FRAMES
                    .iter()
                    .find(|&&b| first_char.contains(b))
                    .copied()
                    .unwrap_or("")
            })
            .collect();

        assert_eq!(chars.len(), BRAILLE_FRAMES.len());
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use test_support::*;
