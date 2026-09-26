//! 全屏 TUI 里的选择面板骨架：/session /effort /persona /models 共用。
//!
//! 只管「面板放哪、每帧怎么画、按键怎么分发」。大厅里面板贴在提示行下方、与
//! 输入框左对齐（`lobby_panel_rows` 让 banner 给它腾位）；会话里按面板高度让
//! 正文让位、贴屏底，PageUp/PageDown 与滚轮滚的是正文。内容由 [`PanelModel`]
//! 给：每帧一组行（不含左侧竖条），按键交给它，它说结束才结束。
//!
//! 09-17 从 `session_picker` 抽出来：/effort 与 /persona 的行内菜单在大厅里从
//! 第 0 列起笔、画在星空上（错位），/models 则把整个大厅收掉、菜单出现在左上角
//! 空屏上。三条命令现在同走这条路，观感与 /session 一致。

use crate::cli::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) struct Panel {
    pub(in crate::cli) left: u16,
    pub(in crate::cli) top: u16,
    pub(in crate::cli) width: u16,
    pub(in crate::cli) rows: u16,
}

/// 一帧的几何。
pub(in crate::cli) struct PanelFrame {
    pub(in crate::cli) panel: Panel,
    /// 扣掉左侧竖条后的内容宽。
    pub(in crate::cli) width: usize,
    /// 抬头与帮助行之外能放几行条目。
    pub(in crate::cli) visible: usize,
}

pub(in crate::cli) trait PanelModel {
    type Output;
    /// 想要的高度（含抬头与帮助行）。每帧都问——条目随搜索增减时面板跟着伸缩。
    fn desired_rows(&self) -> u16;
    /// 这一帧的行，不含竖条；多出的截掉，不足的补空行。
    fn content(&mut self, frame: &PanelFrame) -> Vec<String>;
    /// 一次按键；返回 `Some` 就收面板。PageUp/PageDown 与鼠标滚轮不会到这里。
    fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Self::Output>;
}

/// 条目行后面补空行、最后一行放帮助——面板高度固定，帮助行永远贴底。
pub(in crate::cli) fn with_help_line(
    mut lines: Vec<String>,
    rows: u16,
    help: String,
) -> Vec<String> {
    lines.resize(usize::from(rows.saturating_sub(1)), String::new());
    lines.push(help);
    lines
}

/// 开面板、跑到模型说结束、把版面还回去。
pub(in crate::cli) fn pick<M: PanelModel>(
    live: &mut LiveReplTail,
    model: &mut M,
) -> Result<M::Output> {
    let result = run(live, model);
    live.lobby_panel_rows = 0;
    // 取消、选定、出错都要把几何与光标还回去；正文从来不归面板管。
    synchronized_terminal_update(CursorAfterUpdate::Shown, || live.resume())?;
    result
}

fn run<M: PanelModel>(live: &mut LiveReplTail, model: &mut M) -> Result<M::Output> {
    const TICK: std::time::Duration = std::time::Duration::from_millis(40);
    // 回合跑着时面板是**嵌在回合循环里**跑的（09-20），那把 `LiveRawMode`
    // 还握在回合循环手里。这时再开一把、退出时 Drop 把 raw 关掉，回合循环接
    // 着读键就落回了行编辑 + 回显，键盘增强也被弹掉一层——和 `question_tui`
    // 的处理一样：终端已经在 raw 里就不碰它。空闲时开面板（输入循环的守卫已
    // 经放了，终端是 cooked 的）照旧自己开自己收。
    let _raw = if terminal::is_raw_mode_enabled().unwrap_or(false) {
        None
    } else {
        Some(LiveRawMode::start()?)
    };
    let mut body_delta = 0isize;
    let mut layout: Option<((u16, u16, u16), Panel)> = None;
    let mut next_tick = std::time::Instant::now() + TICK;
    let mut canvas = Canvas::default();
    loop {
        if expire_toast(live) {
            layout = None;
        }
        let mut page_rows = 1isize;
        // 这一帧画过的东西留一份:空闲期推大厅动画之后要把面板原样压回去。
        let mut painted: Option<(Panel, String, Vec<String>)> = None;
        synchronized_terminal_update(CursorAfterUpdate::Hidden, || {
            let (cols, rows) = terminal::size().unwrap_or((80, 24));
            let desired = model.desired_rows();
            let geometry = (cols, rows, desired);
            let panel = match layout {
                Some((previous, panel)) if previous == geometry && body_delta == 0 => panel,
                _ => {
                    // 重新布局会把整屏（或面板那一带）擦了重画，屏上的面板不算数了。
                    canvas.forget();
                    prepare(live, desired, body_delta)?
                }
            };
            layout = Some((geometry, panel));
            page_rows = panel.top.max(1) as isize;
            let bar = input_prompt_bar(live.mode());
            let frame = PanelFrame {
                panel,
                width: usize::from(panel.width).saturating_sub(visible_width(&bar)),
                visible: usize::from(panel.rows.saturating_sub(2)),
            };
            let mut lines = model.content(&frame);
            lines.resize(usize::from(panel.rows), String::new());
            canvas.paint(&panel, &bar, &lines, repaint_epoch(live), false)?;
            painted = Some((panel, bar, lines));
            Ok(())
        })?;
        body_delta = 0;
        // 面板拿着输入时 REPL 的事件泵是停的。大厅 banner 挂着就按 40ms 的节拍
        // 推帧(与输入泵一致),每拍推一帧星空——以前面板开着的那段星空与扫光是
        // 定格的(09-17 用户报)。节拍按**时刻**算,不按「等满 40ms 没按键」算:按住
        // j/k 时按键比 40ms 密,后一种算法一帧都推不出,扫光一顿一顿(用户实测)。
        // 面板占的是 banner 让出来的那几行,星空补丁不碰它(`cells`):这一拍正文层
        // 没整行擦写过面板那几行,面板就一个字节都不写(09-25,以前每拍整块压回去,
        // 不支持同步输出的终端上面板每拍闪一次)。没有 banner 就 100ms 只看通知条过期。
        loop {
            let wait = if live.banner.is_some() {
                let now = std::time::Instant::now();
                if now >= next_tick {
                    if let Some((panel, bar, lines)) = &painted {
                        synchronized_terminal_update(CursorAfterUpdate::Hidden, || {
                            live.tick_banner()?;
                            let rows = panel.top..panel.top.saturating_add(panel.rows);
                            let touched = live
                                .screen
                                .as_ref()
                                .is_some_and(|screen| screen.touched_any(rows));
                            canvas.paint(panel, bar, lines, repaint_epoch(live), touched)
                        })?;
                    }
                    next_tick = now + TICK;
                }
                next_tick.saturating_duration_since(std::time::Instant::now())
            } else {
                std::time::Duration::from_millis(100)
            };
            if event::poll(wait)? {
                break;
            }
            if expire_toast(live) {
                layout = None;
                break;
            }
        }
        if layout.is_none() {
            continue;
        }
        let event = event::read()?;
        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            if let Event::Mouse(mouse) = event {
                body_delta = match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => -3,
                    crossterm::event::MouseEventKind::ScrollDown => 3,
                    _ => 0,
                };
            }
            continue;
        };
        if kind == crossterm::event::KeyEventKind::Release {
            continue;
        }
        match code {
            KeyCode::PageUp => {
                body_delta = -page_rows;
                continue;
            }
            KeyCode::PageDown => {
                body_delta = page_rows;
                continue;
            }
            _ => {}
        }
        if let Some(output) = model.on_key(code, modifiers) {
            return Ok(output);
        }
    }
}

/// 面板的一行：竖条 + 内容，按面板宽截断、不够就用默认样式的空格补满——一笔写完，
/// 不先铺空格再写字（那样不支持同步输出的终端上看得见「先空了一下」）。
fn panel_row(bar: &str, line: &str, width: u16) -> String {
    let clipped = render::clip_to_display_width(&format!("{bar}{line}"), usize::from(width));
    let pad = usize::from(width).saturating_sub(visible_width(&clipped));
    format!("{clipped}\x1b[0m{}", " ".repeat(pad))
}

/// 屏上现在画着的面板：位置、竖条、每一行，外加画它那一刻屏幕整片失效的次数
/// （`Screen::repaint_epoch`）。下一次只写变了的行；位置或竖条变了、屏幕整片重画过、
/// 调用方说那几行被擦写过，就整块重画（09-25）。
#[derive(Default)]
struct Canvas {
    shown: Option<(Panel, String, Vec<String>, u64)>,
}

impl Canvas {
    /// 屏上的面板不算数了（重新布局会擦掉那一带）。
    fn forget(&mut self) {
        self.shown = None;
    }

    /// 这一版该写的字节：`full` = 屏上那一块已经被擦写过，整块重画。
    fn bytes(&self, panel: &Panel, bar: &str, lines: &[String], epoch: u64, full: bool) -> Vec<u8> {
        let reuse = match &self.shown {
            Some((shown_panel, shown_bar, shown_lines, shown_epoch))
                if !full && shown_panel == panel && shown_bar == bar && *shown_epoch == epoch =>
            {
                Some(shown_lines)
            }
            _ => None,
        };
        let mut out = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            if reuse.is_some_and(|shown| shown.get(row) == Some(line)) {
                continue;
            }
            let y = panel.top.saturating_add(row as u16);
            let _ = queue!(
                out,
                MoveTo(panel.left, y),
                Print(panel_row(bar, line, panel.width))
            );
        }
        out
    }

    fn paint(
        &mut self,
        panel: &Panel,
        bar: &str,
        lines: &[String],
        epoch: u64,
        full: bool,
    ) -> Result<()> {
        let bytes = self.bytes(panel, bar, lines, epoch, full);
        if !bytes.is_empty() {
            let mut stdout = crate::cli::repl::tail::term_out();
            stdout.write_all(&bytes)?;
            stdout.flush()?;
        }
        self.shown = Some((*panel, bar.to_string(), lines.to_vec(), epoch));
        Ok(())
    }
}

/// 屏幕整片失效的次数（没有全屏就是 0）。
fn repaint_epoch(live: &LiveReplTail) -> u64 {
    live.screen
        .as_ref()
        .map_or(0, |screen| screen.repaint_epoch())
}

/// 整块画一遍面板那几行。回合里开在活动区位置上的面板（`tail::turn_panel`，会话项目
/// 第 3 段 B4）用它：那块位置每一帧都跟着活动区重新布局，没有上一版可比。
pub(in crate::cli) fn paint_panel(panel: &Panel, bar: &str, lines: &[String]) -> Result<()> {
    Canvas::default().paint(panel, bar, lines, 0, true)
}

fn expire_toast(live: &mut LiveReplTail) -> bool {
    live.screen
        .as_mut()
        .is_some_and(|screen| screen.expire_toast())
}

fn prepare(live: &mut LiveReplTail, desired: u16, delta: isize) -> Result<Panel> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    if live.banner.is_some() {
        live.lobby_panel_rows = desired.saturating_add(1);
        live.resume()?;
        let lobby = live
            .banner
            .as_ref()
            .expect("lobby was not changed")
            .lobby_with_bottom_space(
                usize::from(cols),
                usize::from(rows),
                usize::from(live.tail_rows),
                usize::from(live.lobby_panel_rows),
            );
        // 终端太矮时位置可能不够；面板宁可截短也不写到最后一行之外。
        let top = lobby.below.saturating_add(1).min(rows.saturating_sub(1));
        return Ok(Panel {
            left: lobby.left,
            top,
            width: lobby.width.min(cols.saturating_sub(lobby.left)),
            rows: desired.min(rows.saturating_sub(top)),
        });
    }
    let panel = body_panel(cols, rows, desired);
    if let Some(screen) = &mut live.screen {
        screen.scroll_question_body(delta, panel.rows.saturating_add(1))?;
    }
    let mut stdout = crate::cli::repl::tail::term_out();
    for row in panel.top..rows {
        queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    }
    Ok(panel)
}

fn body_panel(cols: u16, rows: u16, desired: u16) -> Panel {
    let height = desired.min(rows.saturating_sub(2).max(1)).min(rows);
    Panel {
        left: 0,
        top: rows.saturating_sub(height).saturating_sub(1),
        width: cols,
        rows: height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_panel_reserves_only_its_height_and_bottom_padding() {
        let panel = body_panel(100, 32, 4);
        assert_eq!(panel.top, 27);
        assert_eq!(panel.rows, 4);
        for rows in 0..50 {
            let panel = body_panel(80, rows, 12);
            assert!(panel.top + panel.rows <= rows);
        }
    }

    /// 面板没变就一个字节都不写；只变了一行就只写那一行；屏上被擦写过、位置变了、
    /// 屏幕整片失效过就整块重画。每一行都是一笔写满面板宽，不先铺空格。
    #[test]
    fn the_canvas_writes_only_what_changed_on_screen() {
        let panel = Panel {
            left: 10,
            top: 20,
            width: 30,
            rows: 3,
        };
        let lines: Vec<String> = vec!["选择会话".into(), "› 第一条".into(), "帮助".into()];
        let mut canvas = Canvas::default();
        assert!(
            !canvas.bytes(&panel, "┃ ", &lines, 4, false).is_empty(),
            "第一次整块写"
        );
        canvas.shown = Some((panel, "┃ ".into(), lines.clone(), 4));
        assert!(
            canvas.bytes(&panel, "┃ ", &lines, 4, false).is_empty(),
            "没变就不写"
        );
        let mut moved = lines.clone();
        moved[1] = "  第一条".into();
        let one = String::from_utf8(canvas.bytes(&panel, "┃ ", &moved, 4, false)).unwrap();
        assert!(one.starts_with("\x1b[22;11H"), "只写第二行：{one:?}");
        assert!(
            !one.contains("选择会话") && !one.contains("帮助"),
            "{one:?}"
        );
        for full in [
            canvas.bytes(&panel, "┃ ", &lines, 4, true),
            canvas.bytes(&panel, "┃ ", &lines, 5, false),
            canvas.bytes(&Panel { top: 21, ..panel }, "┃ ", &lines, 4, false),
        ] {
            let text = String::from_utf8(full).unwrap();
            assert!(
                text.contains("选择会话") && text.contains("帮助"),
                "{text:?}"
            );
        }
        // 一笔写满面板宽：定位之后没有先铺一整行空格。
        let row = panel_row("┃ ", "第一条", 30);
        assert_eq!(visible_width(&row), 30);
        assert!(!row.starts_with(' '), "{row:?}");
    }

    #[test]
    fn help_line_sits_on_the_last_row() {
        let lines = with_help_line(vec!["a".into()], 4, "help".into());
        assert_eq!(lines, vec!["a", "", "", "help"]);
        let lines = with_help_line(vec!["a".into(), "b".into(), "c".into()], 3, "help".into());
        assert_eq!(lines, vec!["a", "b", "help"]);
    }
}
