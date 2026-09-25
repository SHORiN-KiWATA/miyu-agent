//! 全屏后端的绘制面：整帧重画、行级缓存、光标定位、选区上色。从 `src/cli/repl/tail/screen/mod.rs` 搬来（09-16 拆分），逻辑未改。

use super::*;

impl Screen {
    /// 这一屏幕行画出来取决于什么：哪一行、那一行的第几版、以及它这一帧的
    /// 装饰（悬浮／选区）。三样都没变，画出来必然一模一样。
    fn row_key(&self, index: usize) -> (usize, u64, u64) {
        let stamp = self.row_source_stamp(index);
        let mut decoration = 0u64;
        if let Some(hovered) = self.hovered() {
            if self.block_at(index).map(|(id, _)| id) == Some(hovered) {
                decoration |= 1;
            }
        }
        if let Some(selection) = self.selection {
            let (start, end) = selection.ordered();
            if index >= start.0 && index <= end.0 {
                decoration |= 2;
                decoration |= u64::from(start.1) << 8;
                decoration |= u64::from(end.1) << 24;
                if index == start.0 {
                    decoration |= 1 << 40;
                }
                if index == end.0 {
                    decoration |= 1 << 41;
                }
            }
        }
        (index, stamp, decoration)
    }

    /// 内容不满一屏时，正文上面垫掉多少行。
    ///
    /// 09-14 定为 **0**：正文顶部对齐、输入框钉死在底部，中间允许留白。
    /// 09-11 那版是贴着活动区往上长（垫 `body - content_rows` 行），为的是
    /// 和 inline REPL 一样"最后一行紧挨输入框"；空会话大厅落地后用户拍板改成
    /// 第一条消息落在屏幕顶上。留着这个函数是因为 `suspend`/点选换算/面板上方
    /// 重画都从这里取偏移，以后要改回去只动这一处。
    pub(in crate::cli) fn top_pad(&self) -> usize {
        0
    }

    /// 把终端让给外部输出（选择器 / 提问面板 / 图片自己往 stdout 打）。
    ///
    /// 语义照抄 inline：**擦掉活动区、光标回到正文末尾**，外部输出接着正文
    /// 往下打。清屏 + 光标归零是错的——那样选择器和提问面板会跑到屏幕左上角，
    /// 而不是长在输入框那一带。
    ///
    /// 不退出 alt screen：那些组件打的是普通 ANSI，在备用屏上一样显示；
    /// 它们撑空行把画面顶上去也没关系，`resume` 会整屏重画。
    pub(in crate::cli) fn suspend(&mut self) -> Result<()> {
        if std::env::var_os("MIYU_SCREEN_TRACE").is_some() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/miyu-screen-trace.log")
            {
                let _ = writeln!(f, "suspend");
            }
        }
        self.suspended = true;
        self.invalidate();
        // 从**正文末尾**往下擦，不是从光标往下。
        //
        // 活动区的实时几行被擦掉之后光标会退回那一段的开头——拿它当起点的话，
        // 一次提问就能把大半屏正文一起抹了（用户实测：面板一弹，上面全空）。
        // 内容到哪儿为止是 `content_rows`，那才是外部输出该接着写的地方。
        let tail_top = self.body_height(0).saturating_sub(1);
        let bottom = self
            .top_pad()
            .saturating_add(self.cursor_rows().saturating_sub(self.scroll))
            .min(usize::from(tail_top));
        let row = u16::try_from(bottom).unwrap_or(0);
        let mut stdout = crate::cli::repl::tail::term_out();
        // 活动区那几行擦掉，外部输出才不会跟旧的输入框叠在一起。
        for offset in row..self.rows {
            queue!(stdout, MoveTo(0, offset), Clear(ClearType::CurrentLine))?;
        }
        queue!(stdout, MoveTo(0, row))?;
        stdout.flush()?;
        Ok(())
    }

    /// 把屏幕拿回来。
    ///
    /// `external` = 「这一帧之前可能有别人往终端打过字」。那就只能整屏擦：
    /// 残留不在自己的账上，逐行 diff 盖不住。`/help` 这类命令直接 `println!`
    /// 且**不走 `suspend`**，所以不能只看 `suspended`。
    ///
    /// 反过来，自己写的帧（流式输出、拖选重画）必须走 diff——每帧
    /// `Clear(All)` + 全量重绘会让光标一路闪、拖选卡到没法用。
    pub(in crate::cli) fn resume(&mut self, external: bool) {
        if std::env::var_os("MIYU_SCREEN_TRACE").is_some() {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/miyu-screen-trace.log")
            {
                let _ = writeln!(f, "resume susp={}", self.suspended);
            }
        }
        if !self.suspended && !external {
            return;
        }
        self.suspended = false;
        self.invalidate();
        self.needs_clear = true;
    }

    /// 画正文窗口，返回活动区该从第几行开始。
    ///
    /// 活动区自己不画——`render_repl_input_with_footer` 会 `MoveTo` 到这个
    /// 行号再打，和 inline 下一模一样。
    /// `paint` 的前半段：对块版本、替用户开「默认开着」的块、算正文高与滚动
    /// 位置。不写终端——帧成本量尺和测试也走它。返回正文高。
    pub(in crate::cli) fn prepare_frame(&mut self, tail_height: u16) -> u16 {
        // Streaming and overlay frames also own toast expiry. The idle input
        // loop may not run again until a long reply has finished.
        self.expire_toast();
        // 展开着的块内容可能还在长（正在想的那一步）——画之前先对一次版本。
        self.refresh_expanded();
        // 「完整」那一档落下来的步：出来就是展开态，不用点。只开一次。
        self.seed_open_blocks();
        let body = self.body_height(tail_height);
        self.body = Some(body);
        // 正文区的尺寸交出去：图片、表格、公式按它算才不会顶出可视范围。
        // 左右各两列边距：左边那条是装订边（`indent_body` 加的），右边留着是为了
        // 让折行有个落点——正好顶到最后一列的话，看着像是被屏幕切掉的。
        miyu_base::terminal::set_content_viewport(Some((
            super::content_cols(self.cols) as u16,
            body.saturating_sub(1).max(4),
        )));
        let max = self.follow_target();
        if self.follow {
            self.scroll = max;
        } else {
            // 按键那一刻算的「底」用的是上一帧的正文高，这一帧的底可能更近：视口
            // 被夹到底了就是到底了，跟随得跟着扶正。原来只夹 `scroll` 不动
            // `follow`，于是「屏幕上在底部、状态上不跟随」成了个不动点——之后
            // 的输出视口再也不跟，直到下一次提交（用户 09-17：「AI 接下来的所有
            // 输出都会刷新在整个屏幕上；中断后重新触发回复才恢复」）。
            self.scroll = self.scroll.min(max);
            self.follow = self.scroll >= max;
        }
        body
    }

    /// 视口里的一行画成 ANSI（不写终端）：视图行 → 展开底色 → 悬浮提亮 → 选区反显。
    pub(in crate::cli) fn frame_line(&self, index: usize) -> String {
        let row = self.expansion_paint(index, self.view_row(index));
        let spans = self.highlight(index, self.hover_paint(index, row));
        spans_to_ansi(&spans)
    }

    pub(in crate::cli) fn paint(&mut self, tail_height: u16) -> Result<u16> {
        let mut stdout = crate::cli::repl::tail::term_out();
        self.paint_into(&mut stdout, tail_height)
    }

    /// `paint` 的本体：往给定的 writer 写（测试拿缓冲区收这一帧的字节）。
    pub(in crate::cli) fn paint_into(
        &mut self,
        stdout: &mut impl std::io::Write,
        tail_height: u16,
    ) -> Result<u16> {
        let body = self.prepare_frame(tail_height);
        // 这一帧整行擦写过哪些行，从头记（见 `touched`）。
        self.touched.clear();
        self.touched.resize(usize::from(self.rows), false);
        if self.suspended {
            return Ok(body);
        }

        if std::env::var_os("MIYU_SCREEN_TRACE").is_some() {
            let total = self.content_rows();
            let note = format!(
                "{} paint body={body} total={total} scroll={} follow={} lines={} cursor={} clear={} susp={}\n",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
                self.scroll,
                self.follow,
                self.term.line_count(),
                self.term.cursor_row(),
                self.needs_clear,
                self.suspended
            );
            // 排查「点开正在想的那一步之后屏幕上出现两份」用的：块的行号范围
            // 与展开着的是哪几个。缓冲里的块跨度对不上活动区此刻的位置时，
            // 展开出来的那一片就会落在别处，而活动区照旧在下面画自己那一份。
            let note = format!(
                "{note}   blocks={:?} expanded={:?} live_rows={}\n",
                self.term
                    .blocks()
                    .iter()
                    .map(|block| (block.id, block.start, block.end, block.open))
                    .collect::<Vec<_>>(),
                self.expanded.keys().collect::<Vec<_>>(),
                self.term.filled_rows(),
            );
            let path = std::path::Path::new("/tmp/miyu-screen-trace.log");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = std::io::Write::write_all(&mut file, note.as_bytes());
            }
        }
        if self.force {
            // 哨兵值：任何真实行都不会等于它，于是每一行都会被重画。
            self.painted.clear();
            self.painted.resize(usize::from(body), "\u{0}".into());
            self.row_keys.clear();
            self.force = false;
        } else {
            self.painted.resize(usize::from(body), String::new());
        }
        self.row_keys.resize(usize::from(body), None);

        if std::env::var_os("MIYU_SCREEN_TRACE").is_some() {
            queue!(
                stdout,
                Print(format!("\x1b]1337;paint={}\x07", self.scroll))
            )?;
        }
        // 重画期间把光标藏起来。不藏的话它会跟着每一行的 MoveTo 在屏上乱跳，
        // 流式输出时尤其刺眼（用户原话「光标在反复上下跳动」）。活动区渲染
        // 收尾时会把它重新 Show 出来并放到输入位置。
        queue!(stdout, crossterm::cursor::Hide)?;
        if self.needs_clear {
            // 外部输出（选择器 / 提问面板）可能把画面整个顶上去过，
            // 逐行重画盖不住那些残留，只能整屏擦一次。
            queue!(stdout, Clear(ClearType::All))?;
            self.needs_clear = false;
            self.touched.fill(true);
        }
        // 正文顶部对齐（`top_pad` = 0）。
        if let Some(banner) = self.banner.clone() {
            // 空会话:正文区就是 banner 那几行,逐行比对往上写。
            //
            // 这一行上一帧画的就是上一版大厅的同一行时,只按格子补变了的星星——
            // 整行重写一帧 15KB、每秒三百多 KB(09-23 实测),终端被迫跟着重解析。
            // 对不上(整屏擦过、被浮层盖过标了哨兵)就整行重写,不在不知道底色的
            // 地方打补丁。
            let shown = self.banner_shown.take();
            let no_spans = Vec::new();
            for y in 0..body {
                let slot = usize::from(y);
                let line = banner.text.get(slot).map(String::as_str).unwrap_or("");
                if let Some(key) = self.row_keys.get_mut(slot) {
                    *key = None;
                }
                if self.painted[slot] == line {
                    continue;
                }
                let base = shown.as_ref().filter(|shown| {
                    shown.text.get(slot).map(String::as_str).unwrap_or("") == self.painted[slot]
                });
                match base {
                    // 按格子补丁只写变了的星星、不擦行尾（`cells`），叠在这一行上的
                    // 输入框和面板原样留着，不算擦写过。
                    Some(shown) => queue!(
                        stdout,
                        Print(super::cells::patch_row(
                            shown.spans.get(slot).unwrap_or(&no_spans),
                            banner.spans.get(slot).unwrap_or(&no_spans),
                            y,
                        ))
                    )?,
                    None => {
                        queue!(
                            stdout,
                            MoveTo(0, y),
                            Clear(ClearType::UntilNewLine),
                            Print(line)
                        )?;
                        self.touch(y);
                    }
                }
                self.painted[slot] = line.to_string();
            }
            self.banner_shown = Some(banner);
            self.paint_toast(stdout, body)?;
            self.paint_command_hint(stdout, body)?;
            stdout.flush()?;
            return Ok(body);
        }
        // 正文顶部对齐(见 `top_pad`)。
        let pad = 0usize;
        for y in 0..body {
            let slot = usize::from(y);
            let line = match usize::from(y).checked_sub(pad) {
                Some(offset) => {
                    let index = self.scroll + offset;
                    // 这一行和上一帧一模一样就直接跳过——流式输出时真正变的只有
                    // 最后一两行，其余三十几行每帧重排一遍纯属白干（也正是拖选
                    // 发涩的来源）。
                    // 展开着东西也照样缓存：展开出来的行拿块的内容版本当版本号
                    //（`row_source_stamp`）。原来一展开就整个不缓存，视口里三十几行
                    // 每帧全部重排。
                    let key = Some(self.row_key(index));
                    if self.row_keys.get(slot).copied().flatten() == key {
                        continue;
                    }
                    let line = self.frame_line(index);
                    if let Some(slot) = self.row_keys.get_mut(slot) {
                        *slot = key;
                    }
                    line
                }
                None => {
                    if let Some(slot) = self.row_keys.get_mut(slot) {
                        *slot = None;
                    }
                    String::new()
                }
            };
            if self.painted[usize::from(y)] == line {
                continue;
            }
            queue!(
                stdout,
                MoveTo(0, y),
                Clear(ClearType::UntilNewLine),
                Print(&line)
            )?;
            self.touch(y);
            self.painted[usize::from(y)] = line;
        }
        self.paint_toast(stdout, body)?;
        self.paint_command_hint(stdout, body)?;

        stdout.flush()?;
        Ok(body)
    }

    /// 输入区里有没有选区。反显是另写的一笔，活动区按行记账得知道。
    pub(in crate::cli) fn has_input_selection(&self) -> bool {
        self.input_selection.is_some()
    }

    /// 输入区里被选中的那几行反白重画一遍。
    ///
    /// **必须在活动区画完之后调**。活动区（输入框 + footer）是
    /// `render_repl_input_with_footer` 在 `paint` 返回之后才画的——反显要是跟着
    /// `paint` 一起画，下一笔就被输入框原样盖掉，屏幕上看着像"选不中"
    /// （剪贴板其实是对的，所以走查一直是绿的，只有用眼睛看才发现）。
    pub(in crate::cli) fn paint_input_selection(&self) -> Result<()> {
        let mut stdout = crate::cli::repl::tail::term_out();
        let stdout = &mut stdout;
        self.paint_input_selection_into(stdout)?;
        use std::io::Write as _;
        stdout.flush()?;
        Ok(())
    }

    fn paint_input_selection_into(
        &self,
        stdout: &mut crate::cli::repl::tail::TermOut,
    ) -> Result<()> {
        if self.input_selection.is_none() {
            return Ok(());
        }
        for (row, text) in &self.input_rows {
            let Some((from, to)) = self.input_selection_span(*row) else {
                continue;
            };
            let spans = ansi::parse_ansi_line(text);
            let skip = decoration_of(&spans);
            let highlighted = highlight_columns(spans, from.max(skip), to);
            queue!(
                stdout,
                MoveTo(0, *row),
                Clear(ClearType::UntilNewLine),
                Print(spans_to_ansi(&highlighted))
            )?;
        }
        Ok(())
    }
}
