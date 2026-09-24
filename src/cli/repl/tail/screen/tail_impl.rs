//! 全屏后端挂在 `LiveReplTail` 上的那一面：重画一帧、剪贴板落地、屏幕事件分发。从 `src/cli/repl/tail/screen/mod.rs` 搬来（09-16 拆分），逻辑未改。

use super::*;

impl super::super::LiveReplTail {
    /// 重画一帧（正文 + 活动区）。
    fn repaint_screen(&mut self) -> Result<()> {
        let cursor = self.output_cursor;
        // 拖选重画是自己的帧：中间没人插手，走 diff。
        self.resume_at_own(cursor)
    }

    /// 把选好的文本送进剪贴板。
    ///
    /// 走 OSC 52：全屏程序没法调 `wl-copy` 那套（它们要能访问用户的会话，
    /// 而且开子进程会抢终端）。kitty 默认允许 write-clipboard。
    fn flush_clipboard(&mut self) -> Result<()> {
        let Some(text) = self.screen.as_mut().and_then(|s| s.pending_copy.take()) else {
            return Ok(());
        };
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        let mut stdout = std::io::stdout();
        write!(stdout, "\x1b]52;c;{encoded}\x07")?;
        stdout.flush()?;
        Ok(())
    }

    /// 全屏下的视口操作：回翻、拖选、复制。返回 `true` 表示事件已消费。
    ///
    /// `↑↓` **不在这里**：它们归输入历史（用户裁定），回翻走滚轮 /
    /// PgUp / PgDn / Ctrl+↑↓。inline 模式下这个函数什么都不做。
    pub(in crate::cli) fn handle_screen_event(
        &mut self,
        event: &crossterm::event::Event,
    ) -> Result<bool> {
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
        };
        // 切到别的窗口：提亮立刻熄掉。这条只覆盖「焦点真的变了」，而焦点跟随
        // 鼠标没开的桌面上（用户就是这种）指针飘出去焦点是不变的——那条靠
        // `expire_hover`。事件不吞掉：编辑器还要靠它更新「窗口有没有焦点」。
        if matches!(event, Event::FocusLost) {
            self.last_mouse_move = None;
            if self
                .screen
                .as_mut()
                .is_some_and(super::super::screen::Screen::clear_hover)
            {
                self.repaint_screen()?;
            }
            if self.job_hover.take().is_some() {
                self.tick_job_strip()?;
            }
            return Ok(false);
        }
        let Some(screen) = &self.screen else {
            return Ok(false);
        };
        // 翻半屏：整屏翻过去会把上下文全换掉，眼睛得重新找位置。留一半重叠
        // 才接得上。
        let page = isize::try_from((screen.rows / 2).max(1)).unwrap_or(10);
        let body = screen.body();

        if let Event::Mouse(mouse) = event {
            let (column, row) = (mouse.column, mouse.row);
            if matches!(mouse.kind, crossterm::event::MouseEventKind::Moved) {
                self.last_mouse_move = Some(((column, row), std::time::Instant::now()));
            }
            if std::env::var_os("MIYU_SCREEN_TRACE").is_some() {
                use std::io::Write as _;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/miyu-screen-trace.log")
                {
                    let _ = writeln!(
                        f,
                        "mouse {:?} col={column} row={row} body={body} scroll={}",
                        mouse.kind, screen.scroll,
                    );
                }
            }
            // 覆盖层开着：滚轮翻页、左键**拖选或开合**面板里的块，其余吞掉。
            //
            // 面板原来整个不做选区，而它装的正是最想复制走的东西：子代理的输出、
            // 后台命令的日志（用户 09-17：「这样的浮层无法选中文字」）。
            // 「原地点一下 = 开合这一块 / 拖过 = 选区复制」的消歧和正文那侧同一
            // 条规矩：按下-松开是同一次，只能靠有没有拖动来分。
            if self
                .screen
                .as_ref()
                .is_some_and(super::super::screen::Screen::overlay_open)
            {
                let delta = match mouse.kind {
                    MouseEventKind::ScrollUp => -3,
                    MouseEventKind::ScrollDown => 3,
                    // 悬浮提亮：面板里也得有。不提亮的话「哪儿能点」全靠猜，
                    // 而正文那侧一直是有的（用户 09-17）。
                    MouseEventKind::Moved => {
                        if self
                            .screen
                            .as_mut()
                            .is_some_and(|screen| screen.overlay_hover_at(row))
                        {
                            self.repaint_screen()?;
                        }
                        return Ok(true);
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(screen) = &mut self.screen {
                            screen.overlay_select_begin(column, row);
                        }
                        self.repaint_screen()?;
                        return Ok(true);
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(screen) = &mut self.screen {
                            screen.overlay_select_extend(column, row);
                        }
                        // 同正文那侧：后面还堆着事件就先不画，不然跟不上手。
                        if !crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                            self.repaint_screen()?;
                        }
                        return Ok(true);
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        let clicked = self
                            .screen
                            .as_mut()
                            .and_then(super::super::screen::Screen::overlay_select_finish);
                        if clicked.is_some() {
                            if let Some(screen) = &mut self.screen {
                                screen.overlay_click(row);
                            }
                        }
                        self.repaint_screen()?;
                        self.flush_clipboard()?;
                        return Ok(true);
                    }
                    _ => return Ok(true),
                };
                // 滚轮**按指针在哪**分流：指在面板里就翻面板，指在面板外面就翻
                // 它上面那截正文。一律翻面板的话，面板一开正文就锁死了，而面板
                // 讲的往往正是上面那几行的后续（用户实测）。
                let span = self
                    .screen
                    .as_ref()
                    .and_then(super::super::screen::Screen::overlay_span);
                let inside = span.is_some_and(|(top, bottom)| row >= top && row <= bottom);
                if inside {
                    if let Some(screen) = &mut self.screen {
                        screen.scroll_overlay(delta);
                    }
                    self.repaint_screen()?;
                } else if let Some((top, _)) = span {
                    // 只重画面板上面那一截，面板自己那几行不碰。
                    let rows = crossterm::terminal::size()
                        .map(|(_, rows)| rows)
                        .unwrap_or(24);
                    let panel_rows = rows.saturating_sub(top);
                    if let Some(screen) = &mut self.screen {
                        screen.scroll_above_panel(delta, panel_rows)?;
                    }
                }
                return Ok(true);
            }
            // 悬浮：鼠标扫过可点的行就提亮它。不提亮的话「哪儿能点」全靠猜。
            if matches!(mouse.kind, MouseEventKind::Moved) {
                // 底部任务条那几行在正文区之外,单独判:悬在哪一条上那一条就不 dim
                // (用户 09-18:任务条行悬浮没有高亮)。
                let strip_hover = (self.job_strip_rows > 0 && row >= self.job_strip_start)
                    .then(|| usize::from(row - self.job_strip_start).checked_sub(1))
                    .flatten()
                    .filter(|index| *index < self.jobs.len());
                if strip_hover != self.job_hover {
                    self.job_hover = strip_hover;
                    self.tick_job_strip()?;
                }
                let index = self
                    .screen
                    .as_ref()
                    .and_then(|screen| screen.body_index(row, body));
                let changed = self
                    .screen
                    .as_mut()
                    .is_some_and(|screen| screen.hover_at(index));
                if changed {
                    self.repaint_screen()?;
                }
                return Ok(true);
            }
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_screen(-3)?;
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_screen(3)?;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(screen) = &mut self.screen {
                        // 输入框里的字也该能选——那是自己刚打的东西，
                        // 想复制走再正常不过。
                        if !screen.input_select_begin(column, row) {
                            screen.selection_begin(column, row, body);
                        }
                    }
                    self.repaint_screen()?;
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(screen) = &mut self.screen {
                        if !screen.input_select_extend(column, row) {
                            screen.selection_extend(column, row, body);
                        }
                    }
                    // 拖一下鼠标一秒能发上百个事件，每个都整屏重画就跟不上手了
                    // ——AI 同时在流式输出时两边叠在一起，手上就是"好卡"。
                    // 后面还堆着事件就先不画：下一个事件马上到，这一帧画了也白画。
                    if !crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                        self.repaint_screen()?;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if self
                        .screen
                        .as_mut()
                        .is_some_and(super::super::screen::Screen::input_select_finish)
                    {
                        self.repaint_screen()?;
                        self.flush_clipboard()?;
                        return Ok(true);
                    }
                    // 原地点一下是「展开/收起这一块」，拖过才是选区复制。
                    // 两者共用一次按下-松开，只能靠有没有拖动来分。
                    let click = self
                        .screen
                        .as_mut()
                        .and_then(super::super::screen::Screen::selection_finish);
                    // 点在后台状态行上：开那个任务的日志面板。状态行在活动区
                    // 里，不在正文缓冲里，所以走单独的命中判断。
                    if self.open_job_overlay_at(row)? {
                        return Ok(true);
                    }
                    if let Some(row) = click {
                        // 点在链接上就去开链接。全屏把鼠标捕获走了，终端自己
                        // 那套点链接失效了，得自己认（用户：点链接没反应）。
                        if let Some(url) = self
                            .screen
                            .as_ref()
                            .map(|screen| screen.view_row(row))
                            .and_then(|spans| super::super::screen::select::url_at(&spans, column))
                        {
                            super::super::screen::select::open_url(&url);
                            if let Some(screen) = &mut self.screen {
                                screen.toast(miyu_base::i18n::text("opening link", "正在打开链接"));
                            }
                            self.repaint_screen()?;
                            return Ok(true);
                        }
                        if let Some(screen) = &mut self.screen {
                            if let Some((id, _)) = screen.block_at(row) {
                                // 子代理点开的是覆盖层，不是就地展开。
                                if miyu_hosts::render::blocks::is_overlay(id) {
                                    screen.open_overlay(id);
                                } else {
                                    screen.toggle_block(id);
                                }
                            }
                        }
                    }
                    self.repaint_screen()?;
                    self.flush_clipboard()?;
                }
                // 其余鼠标事件（移动、中右键）吞掉：不吞会变成一串转义序列
                // 灌进输入框。
                _ => {}
            }
            return Ok(true);
        }

        let delta = match event {
            Event::Key(KeyEvent {
                kind: KeyEventKind::Release,
                ..
            }) => return Ok(false),
            // Esc 先清选区——有选区时按 Esc 的意思是「取消选择」，
            // 而不是中断回合。
            // 面板开着时按 x：停掉它讲的那个后台任务。面板本来就是"这一个
            // 任务"的详情，停别的没有意义。
            Event::Key(KeyEvent {
                code: KeyCode::Char('x'),
                modifiers,
                ..
            }) if modifiers.is_empty()
                && self
                    .screen
                    .as_ref()
                    .is_some_and(super::super::screen::Screen::overlay_open) =>
            {
                if let Some(job_id) = self
                    .screen
                    .as_ref()
                    .and_then(super::super::screen::Screen::overlay_job_id)
                {
                    self.pending_stop_job = Some(job_id);
                    // 停完就退出去：任务都停了还盯着它的日志看没有意义，
                    // 而且面板还压着正文。
                    if let Some(screen) = &mut self.screen {
                        screen.close_overlay();
                    }
                    self.repaint_screen()?;
                }
                return Ok(true);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => {
                // Esc 的优先级：覆盖层 → 命令候选 → 选区，都没有才轮到
                // 「中断回合」。由近及远，先关最上面那层。
                if self
                    .screen
                    .as_mut()
                    .is_some_and(super::super::screen::Screen::close_overlay)
                {
                    self.repaint_screen()?;
                    return Ok(true);
                }
                if self
                    .screen
                    .as_mut()
                    .is_some_and(super::super::screen::Screen::dismiss_command_hint)
                {
                    self.repaint_screen()?;
                    return Ok(true);
                }
                let cleared = self
                    .screen
                    .as_mut()
                    .is_some_and(super::super::screen::Screen::selection_clear);
                if cleared {
                    self.repaint_screen()?;
                    return Ok(true);
                }
                return Ok(false);
            }
            Event::Key(KeyEvent {
                code: KeyCode::PageUp,
                ..
            }) => -page,
            Event::Key(KeyEvent {
                code: KeyCode::PageDown,
                ..
            }) => page,
            Event::Key(KeyEvent {
                code: KeyCode::Up,
                modifiers,
                ..
            }) if modifiers.contains(KeyModifiers::CONTROL) => -3,
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                modifiers,
                ..
            }) if modifiers.contains(KeyModifiers::CONTROL) => 3,
            _ => return Ok(false),
        };
        self.scroll_screen(delta)?;
        Ok(true)
    }

    /// 点在后台状态行上就开日志面板。返回真表示这一下被状态行吃掉了。
    ///
    fn open_job_overlay_at(&mut self, row: u16) -> Result<bool> {
        if std::env::var_os("MIYU_SCREEN_TRACE").is_some() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/miyu-screen-trace.log")
            {
                let _ = writeln!(
                    f,
                    "jobclick row={row} strip_start={} strip_rows={} jobs={}",
                    self.job_strip_start,
                    self.job_strip_rows,
                    self.jobs.len()
                );
            }
        }
        if self.job_strip_rows == 0 || row < self.job_strip_start {
            return Ok(false);
        }
        let offset = usize::from(row - self.job_strip_start);
        if offset >= usize::from(self.job_strip_rows) {
            return Ok(false);
        }
        // `background_job_lines` 头一行是空的分隔行，任务从第二行起。
        let Some(job) = offset.checked_sub(1).and_then(|index| self.jobs.get(index)) else {
            return Ok(false);
        };
        let Some(path) = job.log_path.clone() else {
            return Ok(false);
        };
        let title = job_panel_title(job);
        let job_id = job.job_id.clone();
        let command = job.command.clone();
        if let Some(screen) = &mut self.screen {
            screen.open_log_overlay(std::path::PathBuf::from(path), title, Some(job_id), command);
        }
        self.repaint_screen()?;
        Ok(true)
    }

    /// 回翻。覆盖层开着时翻的是面板，不是正文——屏幕归谁，翻页就归谁。
    fn scroll_screen(&mut self, delta: isize) -> Result<()> {
        if let Some(screen) = &mut self.screen {
            if screen.overlay_open() {
                screen.scroll_overlay(delta);
            } else {
                screen.scroll_by(delta);
                // 翻到顶了，更早的还在库里：往前补一页（会话项目第 2 段）。
                if delta < 0 {
                    screen.load_older_at_top();
                }
            }
        }
        self.repaint_screen()
    }
}
