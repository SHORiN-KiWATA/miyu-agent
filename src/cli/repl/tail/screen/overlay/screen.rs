//! 覆盖层挂在 `Screen` 上的那一面：开合、滚动、命中、绘制。从 `src/cli/repl/tail/screen/overlay.rs` 搬来（09-16 拆分），逻辑未改。

use super::*;

impl Screen {
    /// 打开一层。已经开着同一块就当作关闭（再点一次收起来）。
    pub(in crate::cli) fn open_overlay(&mut self, id: u64) -> bool {
        if self
            .overlay
            .as_ref()
            .is_some_and(|panel| panel.block_id() == Some(id))
        {
            return self.close_overlay();
        }
        let Some(mut panel) = Overlay::from_block(id, panel_inner_width(self.cols)) else {
            return false;
        };
        panel.display_expand = self.display_expand;
        panel.display_fold = self.display_fold;
        panel.display_command_lines = self.display_command_lines;
        self.overlay = Some(panel);
        self.invalidate();
        self.needs_clear = true;
        true
    }

    /// 打开一个后台任务的日志面板。再点同一个就收起来。
    pub(in crate::cli) fn open_log_overlay(
        &mut self,
        path: std::path::PathBuf,
        title: String,
        job_id: Option<String>,
        command: String,
    ) -> bool {
        if self
            .overlay
            .as_ref()
            .is_some_and(|panel| panel.file_path() == Some(path.as_path()))
        {
            return self.close_overlay();
        }
        let mut panel = Overlay::from_file(
            path,
            title,
            job_id,
            command,
            panel_inner_width(self.cols),
            self.display_expand,
            self.display_fold,
            self.display_command_lines,
        );
        panel.display_expand = self.display_expand;
        panel.display_fold = self.display_fold;
        panel.display_command_lines = self.display_command_lines;
        self.overlay = Some(panel);
        self.invalidate();
        self.needs_clear = true;
        true
    }

    /// 后台任务面板的抬头跟着任务快照走（量在涨，抬头得跟上）。
    ///
    /// 抬头是点开那一刻定下来的，之后再没人改过——于是「消耗词元」那一截一直
    /// 停在点开时的数（用户实测：后台子代理浮层上方的 token 计数没有动态刷新）。
    /// 前台那种面板的抬头跟着块一起更新，这条是给后台那种补上同样的事。
    pub(in crate::cli) fn refresh_overlay_title(&mut self, title: &str) {
        if let Some(panel) = &mut self.overlay {
            if panel.job_id.is_some() && panel.title != title {
                panel.title = title.to_string();
                self.invalidate();
            }
        }
    }

    /// 屏幕宽变了：开着的面板跟着重排。
    pub(in crate::cli) fn resize_overlay(&mut self, cols: u16) {
        let inner = panel_inner_width(cols);
        if let Some(panel) = &mut self.overlay {
            panel.set_cols(inner);
            // 高度是只涨不缩的，换了宽度之后那个值对不上新内容，放开重算一次。
            panel.height = 0;
        }
    }

    pub(in crate::cli) fn close_overlay(&mut self) -> bool {
        if self.overlay.take().is_some() {
            self.invalidate();
            self.needs_clear = true;
            return true;
        }
        false
    }

    /// 面板占多高：按内容来，最多吃掉屏幕的六成。
    ///
    /// 占满整屏没必要——面板讲的是**某一步**的细节，把主线全遮住反而让人忘了
    /// 自己在哪儿。留着上面那截正文，关掉时也不会有「换了个世界」的突兀感。
    /// 面板占多高。**按屏幕算，不按内容算**。
    ///
    /// 跟着内容长的话，刚点开时里面只有一两行，面板就只有指甲盖那么大，随后
    /// 一边读一边往上窜（用户原话「一开始这个浮层特别小」）。面板是个固定的
    /// 取景窗，大小该是稳的；内容多了滚就是了。
    /// 面板占多高。
    ///
    /// 五分之三看着太压人——它盖着的那截正文才是"我刚才在看什么"的上下文
    /// （用户：可以整体矮三分之一左右）。五分之二正好：面板里还能一眼看到
    /// 七八步，上面也留得下几行正文。
    pub(in crate::cli) fn overlay_height(&self, _content: usize, rows: u16) -> u16 {
        (rows * 2 / 5)
            .max(PANEL_CHROME + 2)
            .min(rows.saturating_sub(2).max(PANEL_CHROME + 2))
    }

    /// 面板在屏幕上占的行区间（含首尾）。没开面板就是 `None`。
    ///
    /// 用**画的时候那个**高度，和 `overlay_click` 同一口径——高度是只涨不缩的，
    /// 按当前内容重算出来的值和屏幕上那个框对不上。
    pub(in crate::cli) fn overlay_span(&self) -> Option<(u16, u16)> {
        let panel = self.overlay.as_ref()?;
        let height = panel.height.max(1);
        let bottom = self.rows.saturating_sub(2);
        Some((bottom.saturating_sub(height.saturating_sub(1)), bottom))
    }

    /// 面板对应的后台任务（有的话）。按 x 停的就是它。
    pub(in crate::cli) fn overlay_job_id(&self) -> Option<String> {
        self.overlay.as_ref().and_then(|panel| panel.job_id.clone())
    }

    /// 面板里点了一下：命中哪一块就开合哪一块。返回真表示这一下被面板吃掉了。
    ///
    /// `row` 是屏幕行；面板第 0 行是标题，内容从第 1 行起。
    pub(in crate::cli) fn overlay_click(&mut self, row: u16) -> bool {
        let rows = self.rows;
        let Some(panel) = &self.overlay else {
            return false;
        };
        // 用**画的时候那个**高度，不是按当前内容重算的——面板高度是只涨不缩的
        // （见 `Overlay::height`），重算出来的值和屏幕上的框对不上，点击就会
        // 整体差几行。
        let height = panel.height.max(1);
        let bottom = rows.saturating_sub(2);
        let top = bottom.saturating_sub(height.saturating_sub(1));
        // 面板上面那截正文也得跟着新内容走。
        //
        // 平时是 `paint` 在管跟随，而面板开着时那条路整个不走——于是正文冻在
        // 点开面板的那一刻，看着像"流式输出停了、时间线不动了"（用户实测）。
        //
        // 落点见 `follow_target`：**和面板没关系**。
        if self.follow {
            self.scroll = self.follow_target();
        }
        // 点在面板外面：让它落回正文的逻辑去。
        if row < top || row > bottom {
            return false;
        }
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        // 上下那两条线、以及紧挨着它们的两行留白，都不是内容。
        let first = top.saturating_add(1 + PANEL_PAD);
        let last = bottom.saturating_sub(1 + PANEL_PAD);
        if row < first || row > last {
            return true;
        }
        let index = panel.scroll + usize::from(row - first);
        let hit = layer_hit(&Layer::Body(&panel.body), &panel.expanded, index);
        if let Some((id, _)) = hit {
            if super::super::expand::toggle_in(&mut panel.expanded, id, panel.cols) {
                self.invalidate();
            }
        }
        true
    }

    pub(in crate::cli) fn overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    /// 把当前的两个显示开关交给后台面板。见 `Screen::display_expand`。
    ///
    /// 每轮都交一次：`/config` 改完下一轮就该生效，和渲染器那侧同一个节奏。
    pub(in crate::cli) fn set_display_expand(
        &mut self,
        reasoning: bool,
        tools: bool,
        fold: bool,
        command_lines: usize,
    ) {
        if self.display_expand == (reasoning, tools)
            && self.display_fold == fold
            && self.display_command_lines == command_lines
        {
            return;
        }
        self.display_expand = (reasoning, tools);
        self.display_fold = fold;
        self.display_command_lines = command_lines;
        // 档位变了：已经排好的那份要按新档位重排一次，不然要等下一次日志变动。
        if let Some(panel) = &mut self.overlay {
            panel.display_expand = (reasoning, tools);
            panel.display_fold = fold;
            panel.display_command_lines = command_lines;
            panel.force_reload();
        }
        self.invalidate();
    }

    /// 屏幕第 `row` 行对应面板里第几行**内容**。框线、上下留白、面板外都是 `None`。
    ///
    /// 和 `overlay_click` 用同一套几何：高度取**画的时候那个**（只涨不缩），
    /// 按当前内容重算的话和屏幕上的框对不上，整体差几行。
    fn overlay_content_index(&self, row: u16) -> Option<usize> {
        let panel = self.overlay.as_ref()?;
        let height = panel.height.max(1);
        let bottom = self.rows.saturating_sub(2);
        let top = bottom.saturating_sub(height.saturating_sub(1));
        let first = top.saturating_add(1 + PANEL_PAD);
        let last = bottom.saturating_sub(1 + PANEL_PAD);
        if row < first || row > last {
            return None;
        }
        Some((panel.scroll + usize::from(row - first)).min(panel.len().saturating_sub(1)))
    }

    /// 鼠标移到了面板里第 `row` 行。返回真表示悬浮目标变了，要重画。
    ///
    /// 和正文那侧同一条规矩（`Screen::hover_at`）：提亮的是**整块**，不是一行
    /// ——一步的抬头和它露出来的几行是同一件事，只亮一行看着像断了。
    /// 面板里有没有提亮着的一行。
    pub(in crate::cli) fn overlay_hovered(&self) -> bool {
        self.overlay
            .as_ref()
            .is_some_and(|panel| panel.hover.is_some())
    }

    /// 指针离开窗口：正文和面板的提亮一起熄掉。
    ///
    /// 写在这儿是因为面板那份提亮的字段只对 `overlay` 这一层可见。
    pub(in crate::cli) fn clear_hover(&mut self) -> bool {
        let mut changed = self.hover.take().is_some();
        if let Some(panel) = &mut self.overlay {
            changed |= panel.hover.take().is_some();
        }
        if changed {
            self.invalidate();
        }
        changed
    }

    pub(in crate::cli) fn overlay_hover_at(&mut self, row: u16) -> bool {
        let index = self.overlay_content_index(row);
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        let next = index.and_then(|index| {
            layer_hit(&Layer::Body(&panel.body), &panel.expanded, index).map(|(id, _)| id)
        });
        if next == panel.hover {
            return false;
        }
        panel.hover = next;
        self.invalidate();
        true
    }

    /// 屏幕列 → 面板内容列。内容画在 `PANEL_MARGIN` 那一列起。
    fn overlay_column(column: u16) -> u16 {
        column.saturating_sub(PANEL_MARGIN)
    }

    /// 在面板里按下左键：起一个选区。返回真表示这一下归面板管。
    pub(in crate::cli) fn overlay_select_begin(&mut self, column: u16, row: u16) -> bool {
        let Some(index) = self.overlay_content_index(row) else {
            // 按在框线或者面板外：把旧选区清掉，别留一片反显在那儿。
            return self.overlay_select_clear();
        };
        let column = Self::overlay_column(column);
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        panel.selection = Some(super::super::select::Selection {
            anchor: (index, column),
            cursor: (index, column),
            dragging: true,
        });
        self.invalidate();
        true
    }

    /// 拖动：把选区的另一头挪过去。拖出面板就钉在最近的那一行上，别让选区断掉。
    pub(in crate::cli) fn overlay_select_extend(&mut self, column: u16, row: u16) {
        let index = self.overlay_content_index(row).unwrap_or_else(|| {
            let panel = self.overlay.as_ref();
            let (scroll, len) = panel.map_or((0, 1), |panel| (panel.scroll, panel.len().max(1)));
            // 往上拖就钉在这一屏的第一行，往下拖钉在最后一行。
            let height = panel.map_or(1, |panel| usize::from(panel.height.max(1)));
            if row < self.rows.saturating_sub(2) / 2 {
                scroll
            } else {
                (scroll + height).min(len - 1)
            }
        });
        let column = Self::overlay_column(column);
        let Some(panel) = &mut self.overlay else {
            return;
        };
        if let Some(selection) = &mut panel.selection {
            if selection.dragging {
                selection.cursor = (index, column);
                self.invalidate();
            }
        }
    }

    /// 松手。拖过就把选中的文字交给剪贴板；**原地点一下**返回那一行，
    /// 由调用方去开合那一块——两者共用一次按下-松开，只能靠有没有拖动来分。
    pub(in crate::cli) fn overlay_select_finish(&mut self) -> Option<u16> {
        let panel = self.overlay.as_mut()?;
        let mut selection = panel.selection?;
        selection.dragging = false;
        if selection.anchor == selection.cursor {
            panel.selection = None;
            self.invalidate();
            return Some(0);
        }
        panel.selection = Some(selection);
        let text = self.overlay_selection_text(selection);
        if !text.trim().is_empty() {
            self.pending_copy = Some(text);
        }
        self.invalidate();
        None
    }

    /// 清掉面板里的选区。返回真表示确实清掉了。
    pub(in crate::cli) fn overlay_select_clear(&mut self) -> bool {
        let Some(panel) = &mut self.overlay else {
            return false;
        };
        if panel.selection.take().is_some() {
            self.invalidate();
            return true;
        }
        false
    }

    /// 选中的文字。逐行按显示列切，跳过每行的装饰列——和正文那侧同一套
    /// （`Screen::selection_text`），只是取行的地方换成面板自己那张画布。
    fn overlay_selection_text(&self, selection: super::super::select::Selection) -> String {
        let Some(panel) = &self.overlay else {
            return String::new();
        };
        let (start, end) = selection.ordered();
        let mut out = Vec::new();
        for index in start.0..=end.0.min(panel.len().saturating_sub(1)) {
            let spans = panel.row(index);
            if spans.is_empty() {
                out.push(String::new());
                continue;
            }
            let skip = super::super::select::decoration_of(&spans);
            let from = if index == start.0 {
                start.1.max(skip)
            } else {
                skip
            };
            let to = if index == end.0 { end.1 } else { u16::MAX };
            out.push(super::super::select::slice_columns(&spans, from, to, 0));
        }
        out.join("\n")
    }

    pub(in crate::cli) fn scroll_overlay(&mut self, delta: isize) {
        let rows = self.rows;
        let Some(panel) = &self.overlay else {
            return;
        };
        let page = usize::from(
            self.overlay_height(panel.len(), rows)
                .saturating_sub(2)
                .max(1),
        );
        let Some(panel) = &mut self.overlay else {
            return;
        };
        let max = panel.len().saturating_sub(page);
        panel.scroll = if delta < 0 {
            panel.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            panel.scroll.saturating_add(delta as usize)
        }
        .min(max);
        panel.follow = panel.scroll >= max;
        self.invalidate();
    }

    /// 画覆盖层。返回真表示这一帧由面板接管，正文和活动区都不用画了。
    /// 面板里转轮当前该画哪一帧：按时间算，80ms 一帧。
    ///
    /// 原来是「每次画面板、且距上次换帧 ≥80ms 才进一帧」：画面板的节拍是空闲
    /// 轮询（80ms）决定的，差一毫秒就跳过一帧，下一帧要等 160ms——转轮一顿一顿
    ///（用户实测：后台子代理的点阵不顺畅）。按开面板以来的时间定帧，什么时候
    /// 画都画在该在的位置上。
    fn overlay_spinner_frame(&mut self) -> usize {
        let started = *self
            .overlay_spinner_started
            .get_or_insert_with(std::time::Instant::now);
        (started.elapsed().as_millis() / 80) as usize
    }

    pub(in crate::cli) fn paint_overlay(&mut self) -> anyhow::Result<bool> {
        let rows = self.rows;
        let cols = self.cols;
        let spinner = format!(
            "\x1b[36m{}\x1b[39m",
            miyu_hosts::render::wait_spinner::braille_frame(self.overlay_spinner_frame())
        );
        let Some(panel) = &mut self.overlay else {
            return Ok(false);
        };
        panel.refresh();
        let content = panel.len();
        let wanted = self.overlay_height(content, rows);
        let Some(panel) = &mut self.overlay else {
            return Ok(false);
        };
        // 只涨不缩：后台任务每写一行日志就重算高度的话，面板上边沿会跟着往上
        // 跳，AI 一边输出一边写日志时就是一直在抖。
        panel.height = panel.height.max(wanted).min(rows.saturating_sub(2).max(6));
        let height = panel.height;
        let body = height.saturating_sub(PANEL_CHROME).max(1);
        let total = panel.len();
        let max = total.saturating_sub(usize::from(body));
        if panel.follow {
            panel.scroll = max;
        } else {
            panel.scroll = panel.scroll.min(max);
        }
        let scroll = panel.scroll;
        let selection = panel.selection;
        let hover = panel.hover;
        let title = panel.title.clone();
        let stoppable = panel.job_id.is_some();
        // 左右各留 `PANEL_MARGIN` 列。没有竖线，这一列留白就是边界。
        let left = PANEL_MARGIN;
        let inner = panel_inner_width(cols);
        let lines: Vec<String> = (0..usize::from(body))
            .map(|offset| {
                let index = scroll + offset;
                if index >= total {
                    return String::new();
                }
                let spans = panel.row(index);
                // 点开的那一片在面板里也该有暗底，和正文里一个样子——不然同一个
                // 东西换个地方看就换了张脸。
                let spans =
                    if super::super::expand::body_in_expansion(&panel.body, &panel.expanded, index)
                    {
                        super::super::select::paint_expansion_bg(spans, inner)
                    } else {
                        spans
                    };
                // 鼠标停在这一块上：整块提亮（去掉暗色），和正文那侧一样。
                let spans = match hover {
                    Some(id)
                        if layer_hit(&Layer::Body(&panel.body), &panel.expanded, index)
                            .map(|(hit, _)| hit)
                            == Some(id) =>
                    {
                        undim(spans)
                    }
                    _ => spans,
                };
                // 选区反显。只反显可复制的那几列——左边的装饰亮起来会让人以为
                // 竖条也复制进去了（和正文那侧同一条规矩）。
                let spans = match selection {
                    Some(selection) => highlight_row(selection, index, spans),
                    None => spans,
                };
                // 「正在进行」那一行左边距上的占位格换成当帧的点阵字形。
                miyu_hosts::render::clip_to_display_width(&spans_to_ansi(&spans), inner)
                    .replace(miyu_hosts::render::timeline::LIVE_SPINNER_CELL, &spinner)
            })
            .collect();

        let bottom = rows.saturating_sub(2);
        let top = bottom.saturating_sub(height.saturating_sub(1));
        // 落点见 `follow_target`：**和面板没关系**。
        if self.follow {
            self.scroll = self.follow_target();
        }
        let mut stdout = crate::cli::repl::tail::term_out();
        queue!(stdout, crossterm::cursor::Hide)?;
        if self.needs_clear {
            queue!(stdout, Clear(ClearType::All))?;
            self.needs_clear = false;
        }
        // 面板上下那两截正文照常画，不然那儿会是一片空白。
        self.paint_body_above(&mut stdout, top, bottom)?;

        let range = format!(
            "{}–{}/{total}",
            (scroll + 1).min(total.max(1)),
            (scroll + usize::from(body)).min(total),
        );
        let hint = if stoppable {
            t(
                "Esc close · wheel/PgUp scroll · x stop",
                "Esc 关闭 · 滚轮/PgUp 翻页 · x 停止",
            )
        } else {
            t("Esc close · wheel/PgUp scroll", "Esc 关闭 · 滚轮/PgUp 翻页")
        };
        queue!(
            stdout,
            MoveTo(0, top),
            Clear(ClearType::UntilNewLine),
            MoveTo(left, top),
            Print(frame_line(inner, &title, Some(&range)))
        )?;
        // 上下各一行空白，内容夹在中间。
        for row in [top + 1, bottom.saturating_sub(1)] {
            queue!(stdout, MoveTo(0, row), Clear(ClearType::UntilNewLine))?;
        }
        for (offset, line) in lines.iter().enumerate() {
            let row = top + 1 + PANEL_PAD + u16::try_from(offset).unwrap_or(0);
            if row + PANEL_PAD >= bottom {
                break;
            }
            queue!(
                stdout,
                MoveTo(0, row),
                Clear(ClearType::UntilNewLine),
                MoveTo(left, row),
                Print(line)
            )?;
        }
        queue!(
            stdout,
            MoveTo(0, bottom),
            Clear(ClearType::UntilNewLine),
            MoveTo(left, bottom),
            Print(frame_line(inner, hint, None))
        )?;
        stdout.flush()?;
        Ok(true)
    }

    /// 滚一下视口，然后只重画**面板上面**那一截。
    ///
    /// 提问面板开着的时候屏幕是让出去的，正文那边不画；但用户还是想往回翻
    /// （要答的问题往往就指着上面那几行）。面板自己那几行不碰。
    pub(in crate::cli) fn scroll_above_panel(
        &mut self,
        delta: isize,
        panel_rows: u16,
    ) -> anyhow::Result<()> {
        let top = self.rows.saturating_sub(panel_rows);
        // 临界点和 `paint_overlay` 用同一个口径（没有面板时那个高度）。
        //
        // 两处不一致的话：翻上去之后 `follow` 仍是真、下一帧又被拉回底部
        //（用户实测：面板一开外面就翻不动了）；反过来口径比 paint 宽的话，
        // 翻回底部会停在一个 paint 下一帧又要改的位置上，屏幕跳一下。
        let max = self.follow_target();
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll.saturating_add(delta as usize)
        };
        self.scroll = next.min(max);
        self.follow = self.scroll >= max;
        self.invalidate();
        let mut stdout = crate::cli::repl::tail::term_out();
        let rows = self.rows;
        self.paint_body_above(&mut stdout, top, rows)?;
        stdout.flush()?;
        Ok(())
    }

    /// 面板上方那截正文。
    pub(in super::super) fn paint_body_above(
        &mut self,
        stdout: &mut crate::cli::repl::tail::TermOut,
        top: u16,
        bottom: u16,
    ) -> anyhow::Result<()> {
        let pad = self.top_pad();
        let paint = |stdout: &mut crate::cli::repl::tail::TermOut, y: u16| -> anyhow::Result<()> {
            let line = match usize::from(y).checked_sub(pad) {
                Some(offset) => spans_to_ansi(&self.view_row(self.scroll_of() + offset)),
                None => String::new(),
            };
            queue!(
                stdout,
                MoveTo(0, y),
                Clear(ClearType::UntilNewLine),
                Print(&line)
            )?;
            Ok(())
        };
        for y in 0..top {
            paint(stdout, y)?;
        }
        // 面板下面那一行也得擦：面板收起来之前它一直是上一帧的残留。
        for y in bottom.saturating_add(1)..self.rows {
            queue!(stdout, MoveTo(0, y), Clear(ClearType::UntilNewLine))?;
        }
        // 这几行归面板管了，正文那边的缓存作废，收起面板时才会重画。
        self.invalidate();
        Ok(())
    }
}

/// 面板里某一行的选区反显。和 `Screen::highlight` 同一条规矩，只是行号是面板
/// 自己的内容行。
fn highlight_row(
    selection: super::super::select::Selection,
    index: usize,
    spans: Vec<super::super::ansi::AnsiSpan>,
) -> Vec<super::super::ansi::AnsiSpan> {
    let (start, end) = selection.ordered();
    if index < start.0 || index > end.0 || spans.is_empty() {
        return spans;
    }
    let skip = super::super::select::decoration_of(&spans);
    let from = if index == start.0 {
        start.1.max(skip)
    } else {
        skip
    };
    let to = if index == end.0 { end.1 } else { u16::MAX };
    super::super::select::highlight_columns(spans, from, to)
}

/// 去掉暗色 = 提亮。悬浮那一块用它。
fn undim(spans: Vec<super::super::ansi::AnsiSpan>) -> Vec<super::super::ansi::AnsiSpan> {
    spans
        .into_iter()
        .map(|span| super::super::ansi::AnsiSpan {
            style: span.style.remove_modifier(ratatui::style::Modifier::DIM),
            ..span
        })
        .collect()
}
