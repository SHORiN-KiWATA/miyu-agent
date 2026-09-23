//! 空会话的画面：渐变 MIYU 艺术字、两侧稀疏的星空、周期扫光、模式行。
//!
//! 只在**会话没有任何回合**时存在。第一条消息一发它就撤，会话模式随之钉死
//! （中途换模式 = 系统提示词换血 = 全量缓存作废）；`/new` 开出空会话它又回来。
//!
//! 画面本身不认识后端：[`BannerScene::render`] 只产片段行，全屏后端把它铺进
//! 正文区（垂直居中），inline 后端把它塞在输入框上方（紧凑块）。
//!
//! 艺术字是可替换的：`config/banner.txt` 存在就用它（见 [`BannerArt::from_text`]），
//! 渐变、星空、扫光照给；`display.banner = false` 整个关掉。

use crate::cli::repl::tail::screen::ansi::{spans_to_ansi, AnsiSpan};
use miyu_base::config::AppConfig;
use miyu_base::config::PersonaLane;
use miyu_base::i18n::text as t;
use miyu_base::paths::MiyuPaths;
use miyu_base::terminal::palette::{Theme, BLUE, CORAL, DIM, FAINT, GOLD};
use miyu_base::terminal::starfield::{
    fade, gradient_banner, segs_width, star_seg, subtitle_rule, BannerArt, Seg,
};

pub(in crate::cli) mod preview;
use ratatui::style::Modifier;
use std::cell::RefCell;

/// 用户自带艺术字的文件名（放在配置目录下）。
pub(in crate::cli) use miyu_base::terminal::chrome::BANNER_FILE;

/// 扫光的周期（帧）与一次扫过的速度（列/帧）。空闲 tick 是 80ms 一帧。
const GLINT_PAUSE: usize = 25;
const GLINT_SPEED: f32 = 0.9;
/// 星空密度（值越大越稀）：inline 块小，星只在字两侧；全屏大厅铺满。
const STAR_SPARSITY: u32 = 9;
const STAR_SPARSITY_LOBBY: u32 = 8;
/// 大厅里的星更暗:只是氛围,进正文后还得盯着字看。
const STAR_DIM_LOBBY: f32 = 0.42;
/// 大厅的星只铺在艺术字周围这一圈(上下各几行、左右各几十列),不铺满屏。
const LOBBY_STAR_PAD_X: usize = 34;
const LOBBY_STAR_PAD_Y: usize = 11;
const STAR_DIM: f32 = 0.55;
/// 星域比艺术字大一圈（左右各这么多列）。
const STAR_PAD_X: usize = 14;

pub(in crate::cli) struct BannerScene {
    theme: Theme,
    art: BannerArt,
    mode: PersonaLane,
    /// 这个会话开着只读模式(09-23):模式行末尾那个「只读」点亮。
    readonly: bool,
    tick: usize,
    /// 出场淡入：前几帧从底色浮出来。
    born: usize,
    /// 这一帧的大厅算过一次就留着（键里带帧号，下一帧自然失效）。`tick_banner`
    /// 在同步块**外**先算一遍，块里 `paint_tail` 再要就是现成的——块越短越好，
    /// 见 `LiveReplTail::tick_banner`。
    lobby_cache: RefCell<Option<(LobbyKey, Lobby)>>,
}

/// (cols, rows, activity_rows, bottom_rows, tick, born, dev, readonly)
type LobbyKey = (usize, usize, usize, usize, usize, usize, bool, bool);

impl BannerScene {
    /// 按配置决定画不画、画哪份艺术字。`None` = 关掉了。
    pub fn load(config: &AppConfig, paths: &MiyuPaths, mode: PersonaLane) -> Option<Self> {
        if !config.display.banner {
            return None;
        }
        let theme = Theme::detect();
        let art = std::fs::read_to_string(paths.config_dir.join(BANNER_FILE))
            .ok()
            .and_then(|text| BannerArt::from_text(&text))
            .unwrap_or_else(|| BannerArt::builtin(theme.ascii));
        Some(Self {
            theme,
            art,
            mode,
            readonly: false,
            tick: 0,
            born: 0,
            lobby_cache: RefCell::new(None),
        })
    }

    pub(in crate::cli) fn set_mode(&mut self, mode: PersonaLane) {
        self.mode = mode;
    }

    pub(in crate::cli) fn set_readonly(&mut self, readonly: bool) {
        self.readonly = readonly;
    }

    /// 跳过出场淡入。`/reset`、`/new` 从非空会话回到大厅时用:画面本来就亮着,
    /// 再从底色浮一次像卡了一下。
    pub(in crate::cli) fn settle(&mut self) {
        self.born = 24;
    }

    /// 走一帧。返回这一帧要不要重画（每帧都画：40ms 一帧才顺）。
    pub fn tick(&mut self) -> bool {
        self.tick = self.tick.wrapping_add(1);
        if self.born < 24 {
            self.born += 1;
        }
        true
    }

    /// 测试用：不读配置文件的内置艺术字。
    #[cfg(test)]
    pub(in crate::cli) fn builtin_for_tests(mode: PersonaLane) -> Self {
        let theme = Theme::detect();
        Self {
            theme,
            art: BannerArt::builtin(theme.ascii),
            mode,
            readonly: false,
            tick: 0,
            born: 0,
            lobby_cache: RefCell::new(None),
        }
    }

    /// 走到第几帧了。
    #[cfg(test)]
    pub(in crate::cli) fn frame(&self) -> usize {
        self.tick
    }

    /// 扫光此刻在第几列（可能在字外面）。扫完停一小会儿马上再来，持续播放。
    fn glint(&self) -> Option<f32> {
        if !self.theme.depth.gradient_ok() {
            return None;
        }
        let travel = self.art.cols() as f32 + 12.0;
        let period = (travel / GLINT_SPEED) as usize + GLINT_PAUSE;
        let pos = (self.tick % period) as f32 * GLINT_SPEED - 6.0;
        (pos <= self.art.cols() as f32 + 6.0).then_some(pos)
    }

    pub(in crate::cli) fn mode(&self) -> PersonaLane {
        self.mode
    }

    fn fade_in(&self) -> f32 {
        (self.born as f32 / 24.0).min(1.0)
    }

    /// 艺术字有多宽。输入框窄框的宽度按它兜底（框不能比字还窄）。
    pub(in crate::cli) fn art_cols(&self) -> usize {
        self.art.cols()
    }

    /// 紧凑块的高度（不含星空外圈）：艺术字 + 副标题 + 版本 + 空行 + 模式行 + 提示。
    pub(in crate::cli) fn block_rows(&self) -> usize {
        self.art.rows() + 4
    }

    /// 艺术字以外的几行：副标题、版本、空行、模式行、提示。宽度都按 `cols` 居中。
    fn caption_rows(&self, cols: usize) -> Vec<Vec<Seg>> {
        let theme = self.theme;
        let fade_t = self.fade_in();
        let mut rows = Vec::with_capacity(4);
        let subtitle = subtitle_rule(theme, &self.art.subtitle);
        rows.push(center(
            vec![Seg::new(subtitle, fade(theme, DIM, fade_t))],
            cols,
        ));
        rows.push(Vec::new());
        rows.push(center(self.mode_row(), cols));
        rows.push(center(self.hint_row(), cols));
        rows
    }

    /// `/config 进入设置 · /sandbox <路径> 设置沙盒`,命令用金色(「可以按」的颜色)。
    fn hint_row(&self) -> Vec<Seg> {
        let theme = self.theme;
        let fade_t = self.fade_in();
        // 整行都是暗色:它是备注,不是号召。命令比说明亮半档,认得出是命令就行。
        let command = fade(theme, DIM, fade_t);
        let note = fade(theme, FAINT, fade_t);
        vec![
            Seg::new("/config", command),
            Seg::new(t(" open settings", " 进入设置"), note),
            Seg::new("  ·  ", note),
            Seg::new(t("/sandbox <path>", "/sandbox <路径>"), command),
            Seg::new(t(" bind a sandbox", " 设置沙盒"), note),
        ]
    }

    /// `◉ 普通   ○ 开发     Tab 切换   Shift+Tab ○ 只读`。当前模式用它自己的颜色
    /// （普通=雾蓝，开发=酒红）；只读开着时那个点和字点亮成金色（09-23）。
    fn mode_row(&self) -> Vec<Seg> {
        let theme = self.theme;
        let fade_t = self.fade_in();
        let mut segs = Vec::new();
        for (index, mode) in [PersonaLane::Active, PersonaLane::Dev]
            .into_iter()
            .enumerate()
        {
            if index > 0 {
                segs.push(Seg::raw("   "));
            }
            let here = mode == self.mode;
            let color = match mode {
                PersonaLane::Active => BLUE,
                PersonaLane::Dev => CORAL,
            };
            let (dot, style) = if here {
                (
                    theme.dot_here(),
                    fade(theme, color, fade_t).add_modifier(Modifier::BOLD),
                )
            } else {
                (theme.dot_todo(), fade(theme, FAINT, fade_t))
            };
            segs.push(Seg::new(dot, style));
            segs.push(Seg::raw(" "));
            segs.push(Seg::new(
                mode_name(mode),
                if here {
                    fade(theme, color, fade_t).add_modifier(Modifier::BOLD)
                } else {
                    fade(theme, FAINT, fade_t)
                },
            ));
        }
        segs.push(Seg::raw("     "));
        segs.push(Seg::new("Tab", fade(theme, GOLD, fade_t)));
        segs.push(Seg::raw(" "));
        segs.push(Seg::new(t("switch", "切换"), fade(theme, FAINT, fade_t)));
        segs.push(Seg::raw("   "));
        segs.push(Seg::new("Shift+Tab", fade(theme, GOLD, fade_t)));
        segs.push(Seg::raw(" "));
        // 开着时点与字都是金色(用户 09-23),跟状态行上的「只读」同色。
        let (dot, style) = if self.readonly {
            (
                theme.dot_here(),
                fade(theme, GOLD, fade_t).add_modifier(Modifier::BOLD),
            )
        } else {
            (theme.dot_todo(), fade(theme, FAINT, fade_t))
        };
        segs.push(Seg::new(dot, style));
        segs.push(Seg::raw(" "));
        segs.push(Seg::new(t("read-only", "只读"), style));
        segs
    }

    /// 画成 `rows` 行、每行 `cols` 列的片段行。
    ///
    /// `fill` = 全屏正文区：整块垂直居中，星铺满正文两侧的空白；否则是 inline
    /// 用的紧凑块，星只在艺术字那几行的两侧。行数不够时从艺术字底部往下裁。
    pub(in crate::cli) fn render(&self, cols: usize, rows: usize) -> Vec<Vec<Seg>> {
        let theme = self.theme;
        let art_cols = self.art.cols();
        let art_rows = self.art.rows();
        let glint = self.glint();
        let fade_t = self.fade_in();
        let mut banner = gradient_banner(&self.art, theme, glint);
        if fade_t < 1.0 {
            for row in &mut banner {
                for seg in row {
                    seg.style = seg.style.add_modifier(Modifier::DIM);
                }
            }
        }
        let captions = self.caption_rows(cols);
        let block_rows = art_rows + captions.len();
        let top = rows.saturating_sub(block_rows) / 2;
        let left = cols.saturating_sub(art_cols) / 2;
        // 星域：艺术字左右各 STAR_PAD_X 列，上下各两行，不贴字。
        let star_left = left.saturating_sub(STAR_PAD_X);
        let star_right = (left + art_cols + STAR_PAD_X).min(cols);
        let star_top = top.saturating_sub(2);
        let star_bottom = top + art_rows + 2;
        let mut out = Vec::with_capacity(rows);
        for y in 0..rows {
            if y >= top && y < top + art_rows {
                let mut row = Vec::with_capacity(cols);
                for x in 0..left {
                    row.push(self.star_or_blank(x, y, star_left, star_right, theme));
                }
                row.extend(banner[y - top].iter().cloned());
                for x in (left + art_cols)..cols {
                    row.push(self.star_or_blank(x, y, star_left, star_right, theme));
                }
                out.push(row);
            } else if y >= top + art_rows && y < top + block_rows {
                out.push(captions[y - top - art_rows].clone());
            } else if y >= star_top && y < star_bottom {
                let mut row = Vec::with_capacity(cols);
                for x in 0..cols {
                    row.push(self.star_or_blank(x, y, star_left, star_right, theme));
                }
                out.push(row);
            } else {
                out.push(Vec::new());
            }
        }
        out
    }

    fn star_or_blank(&self, x: usize, y: usize, left: usize, right: usize, theme: Theme) -> Seg {
        if x >= left && x < right {
            star_seg(
                x,
                y,
                self.tick,
                theme,
                STAR_DIM * self.fade_in(),
                STAR_SPARSITY,
            )
        } else {
            Seg::raw(" ")
        }
    }

    /// 全屏大厅：整块垂直居中，输入框（`activity_rows` 行）**嵌在 banner 下面**、
    /// 收窄居中；星空铺满全屏，只避开正文那一列。回车发第一句话后整块撤掉，
    /// 输入框回到屏底、恢复全宽。
    pub(in crate::cli) fn lobby(&self, cols: usize, rows: usize, activity_rows: usize) -> Lobby {
        self.lobby_with_bottom_space(cols, rows, activity_rows, 0)
    }

    /// Keep a temporary selector below the complete lobby, shifting the block
    /// upward only when the normal centered layout leaves insufficient room.
    pub(in crate::cli) fn lobby_with_bottom_space(
        &self,
        cols: usize,
        rows: usize,
        activity_rows: usize,
        bottom_rows: usize,
    ) -> Lobby {
        let key: LobbyKey = (
            cols,
            rows,
            activity_rows,
            bottom_rows,
            self.tick,
            self.born,
            matches!(self.mode, PersonaLane::Dev),
            self.readonly,
        );
        if let Some((cached_key, lobby)) = &*self.lobby_cache.borrow() {
            if *cached_key == key {
                return lobby.clone();
            }
        }
        let lobby = self.compose_lobby(cols, rows, activity_rows, bottom_rows);
        self.lobby_cache.replace(Some((key, lobby.clone())));
        lobby
    }

    /// 同步块外先把这一帧算好；块里 `paint_tail` 用同样的参数来要时直接命中。
    pub(in crate::cli) fn warm_lobby(
        &self,
        cols: usize,
        rows: usize,
        activity_rows: usize,
        bottom_rows: usize,
    ) {
        let _ = self.lobby_with_bottom_space(cols, rows, activity_rows, bottom_rows);
    }

    fn compose_lobby(
        &self,
        cols: usize,
        rows: usize,
        activity_rows: usize,
        bottom_rows: usize,
    ) -> Lobby {
        let theme = self.theme;
        let art_cols = self.art.cols();
        let art_rows = self.art.rows();
        // 输入框的宽度/左边距是 `input_layout` 说了算：活动区那边要先按这个
        // 宽度测行数，再回头调这里排纵向位置，两处必须是同一个数。
        let width = crate::cli::repl::input_layout::lobby_box_width(cols, art_cols);
        let left = crate::cli::repl::input_layout::lobby_box_left(cols, width);
        let art_left = cols.saturating_sub(art_cols) / 2;
        // 艺术字 + 副标题 + 版本 + 空行 + 输入框 + 空行 + 模式行 + 提示。
        let block_rows = art_rows + 2 + activity_rows + 3;
        let top = (rows.saturating_sub(block_rows) / 2)
            .min(rows.saturating_sub(block_rows.saturating_add(bottom_rows)));
        let tail_start = top + art_rows + 2;
        // 星只铺在艺术字周围这一圈,不铺满屏——进正文前的画面不能太花。
        let star_rows = top.saturating_sub(LOBBY_STAR_PAD_Y)..(top + block_rows + LOBBY_STAR_PAD_Y);
        let star_cols =
            art_left.saturating_sub(LOBBY_STAR_PAD_X)..(art_left + art_cols + LOBBY_STAR_PAD_X);
        let captions = self.caption_rows(cols);
        // 正文那一列（艺术字与输入框取较宽者，两侧各留 2 列缓冲）不铺星。
        let column_left = left.min(art_left).saturating_sub(2);
        let column_right = ((left + width).max(art_left + art_cols) + 2).min(cols);
        let fade_t = self.fade_in();
        let mut banner = gradient_banner(&self.art, theme, self.glint());
        if fade_t < 1.0 {
            for row in &mut banner {
                for seg in row {
                    seg.style = seg.style.add_modifier(Modifier::DIM);
                }
            }
        }
        let star = |x: usize, y: usize| -> Seg {
            if !star_rows.contains(&y) || !star_cols.contains(&x) {
                return Seg::raw(" ");
            }
            star_seg(
                x,
                y,
                self.tick,
                theme,
                STAR_DIM_LOBBY * fade_t,
                STAR_SPARSITY_LOBBY,
            )
        };
        let mut out = Vec::with_capacity(rows);
        // 面板那几行(`bottom_rows`):面板占的那一段留白,星星只留两侧。否则每帧
        // diff 都把面板那几行的星星重写一遍、面板再压回去——支持同步输出的终端看
        // 不出来,不支持的(和 pyte 抓屏)就是一闪一闪。
        let band = (top + block_rows)..(top + block_rows + bottom_rows);
        for y in 0..rows {
            let in_block = y >= top && y < top + block_rows;
            let mut row: Vec<Seg> = Vec::with_capacity(cols);
            if !in_block {
                let in_band = bottom_rows > 0 && band.contains(&y);
                for x in 0..cols {
                    row.push(if in_band && x >= left && x < left + width {
                        Seg::raw(" ")
                    } else {
                        star(x, y)
                    });
                }
                out.push(row);
                continue;
            }
            // 正文列以外照铺星，列内按行填内容。
            for x in 0..column_left.min(cols) {
                row.push(star(x, y));
            }
            let offset = y - top;
            let content: Vec<Seg> = if offset < art_rows {
                let mut segs = vec![Seg::raw(" ".repeat(art_left.saturating_sub(column_left)))];
                segs.extend(banner[offset].iter().cloned());
                segs
            } else if offset < art_rows + 2 {
                // 副标题、版本：caption_rows 已经按整屏居中，去掉前面属于星区的那截。
                trim_left(captions[offset - art_rows].clone(), column_left)
            } else if offset >= art_rows + 2 + activity_rows + 1 {
                let index = 2 + (offset - (art_rows + 2 + activity_rows + 1));
                trim_left(
                    captions.get(index).cloned().unwrap_or_default(),
                    column_left,
                )
            } else {
                // 空行与输入框那几行：留白，输入框自己往上画。
                Vec::new()
            };
            let used = segs_width(&content);
            row.extend(content);
            let filled = column_left.min(cols) + used;
            if filled < column_right {
                row.push(Seg::raw(" ".repeat(column_right - filled)));
            }
            for x in column_right.max(filled)..cols {
                row.push(star(x, y));
            }
            out.push(row);
        }
        Lobby {
            rows: out
                .into_iter()
                .map(|segs| {
                    let spans: Vec<AnsiSpan> = segs
                        .into_iter()
                        .map(|seg| AnsiSpan::styled(seg.text, seg.style))
                        .collect();
                    spans_to_ansi(&spans).trim_end().to_string()
                })
                .collect(),
            tail_start: tail_start.min(u16::MAX as usize) as u16,
            left: left.min(u16::MAX as usize) as u16,
            width: width.min(u16::MAX as usize) as u16,
            below: (top + block_rows).min(u16::MAX as usize) as u16,
        }
    }

    /// 同 [`Self::render`]，但每行已经转成带 SGR 的字符串（活动区与全屏正文都吃这个）。
    pub(in crate::cli) fn render_ansi(&self, cols: usize, rows: usize) -> Vec<String> {
        self.render(cols, rows)
            .into_iter()
            .map(|segs| {
                let spans: Vec<AnsiSpan> = segs
                    .into_iter()
                    .map(|seg| AnsiSpan::styled(seg.text, seg.style))
                    .collect();
                // 去掉行尾的纯空格片段：省字节，也让 diff 更容易命中「没变」。
                spans_to_ansi(&spans).trim_end().to_string()
            })
            .collect()
    }
}

fn mode_name(mode: PersonaLane) -> &'static str {
    match mode {
        PersonaLane::Active => t("normal", "普通模式"),
        PersonaLane::Dev => t("dev", "开发模式"),
    }
}

/// 全屏大厅的一帧：整屏的行，加上输入框该落在哪。
#[derive(Clone)]
pub(in crate::cli) struct Lobby {
    pub rows: Vec<String>,
    /// 活动区（空行 + 输入框 + footer）从第几行开始。
    pub tail_start: u16,
    /// 输入框的左边距与宽度。
    pub left: u16,
    pub width: u16,
    /// 整块结束的下一行:斜杠命令候选之类的浮层从这里往下摆。
    pub below: u16,
}

/// 居中过的一行去掉开头 `skip` 列的空白（那部分归星区画）。
fn trim_left(segs: Vec<Seg>, skip: usize) -> Vec<Seg> {
    let mut remaining = skip;
    let mut out = Vec::with_capacity(segs.len());
    for mut seg in segs {
        if remaining == 0 {
            out.push(seg);
            continue;
        }
        let width = segs_width(std::slice::from_ref(&seg));
        if width <= remaining {
            remaining -= width;
            continue;
        }
        // 只可能是开头那段纯空格：按列数切。
        seg.text = seg.text.chars().skip(remaining).collect();
        remaining = 0;
        out.push(seg);
    }
    out
}

/// 把一行片段在 `cols` 列里居中（左边补空格）。
fn center(segs: Vec<Seg>, cols: usize) -> Vec<Seg> {
    let width = segs_width(&segs);
    let left = cols.saturating_sub(width) / 2;
    let mut out = Vec::with_capacity(segs.len() + 1);
    if left > 0 {
        out.push(Seg::raw(" ".repeat(left)));
    }
    out.extend(segs);
    out
}

/// 空会话提示行（inline 后端在输入框上方、没有 banner 时也要有一句）。
#[allow(dead_code)]
pub(in crate::cli) fn plain_mode_hint(mode: PersonaLane) -> String {
    format!(
        "{} · {}",
        mode_name(mode),
        t("Tab switches normal/dev", "Tab 切换 普通/开发")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use miyu_base::terminal::palette::Depth;

    fn scene() -> BannerScene {
        BannerScene {
            theme: Theme {
                depth: Depth::True,
                ascii: false,
            },
            art: BannerArt::builtin(false),
            mode: PersonaLane::Active,
            readonly: false,
            tick: 0,
            born: 24,
            lobby_cache: RefCell::new(None),
        }
    }

    #[test]
    fn render_fills_exactly_the_requested_rows() {
        let scene = scene();
        let rows = scene.render(80, 30);
        assert_eq!(rows.len(), 30);
        let ansi = scene.render_ansi(80, 30);
        assert_eq!(ansi.len(), 30);
        // 艺术字那几行的显示宽度不超过终端列数。
        for row in &rows {
            assert!(
                segs_width(row) <= 80,
                "row wider than terminal: {}",
                segs_width(row)
            );
        }
    }

    #[test]
    fn narrow_terminal_still_renders_without_panicking() {
        let scene = scene();
        let rows = scene.render(20, 8);
        assert_eq!(rows.len(), 8);
        let rows = scene.render(0, 0);
        assert!(rows.is_empty());
    }

    #[test]
    fn mode_row_marks_the_active_mode() {
        let mut scene = scene();
        let normal: String = scene
            .mode_row()
            .iter()
            .map(|seg| seg.text.as_str())
            .collect();
        assert!(normal.starts_with("◉ 普通模式") || normal.starts_with("◉ normal"));
        scene.set_mode(PersonaLane::Dev);
        let dev: String = scene
            .mode_row()
            .iter()
            .map(|seg| seg.text.as_str())
            .collect();
        assert!(dev.contains("◉ 开发模式") || dev.contains("◉ dev"));
    }

    #[test]
    fn glint_keeps_sweeping_with_a_short_pause() {
        let mut scene = scene();
        let period = ((scene.art.cols() as f32 + 12.0) / GLINT_SPEED) as usize + GLINT_PAUSE;
        let mut lit = 0usize;
        for _ in 0..period {
            if scene.glint().is_some() {
                lit += 1;
            }
            scene.tick();
        }
        assert!(lit > 0 && lit < period);
        let mut again = false;
        for _ in 0..period {
            again |= scene.glint().is_some();
            scene.tick();
        }
        assert!(again);
    }

    #[test]
    fn lobby_reserves_the_activity_band_inside_the_block() {
        let scene = scene();
        let lobby = scene.lobby(100, 36, 5);
        assert_eq!(lobby.rows.len(), 36);
        assert!(lobby.width >= 40 && lobby.width <= 84);
        assert!(usize::from(lobby.left) + usize::from(lobby.width) <= 100);
        let band = &lobby.rows[usize::from(lobby.tail_start) + 1];
        let plain: String = crate::cli::repl::tail::screen::ansi::parse_ansi_line(band)
            .into_iter()
            .map(|span| span.text)
            .collect();
        let inside = plain
            .chars()
            .skip(usize::from(lobby.left))
            .take(usize::from(lobby.width))
            .collect::<String>();
        assert!(inside.trim().is_empty(), "band not blank: {inside:?}");
        let tiny = scene.lobby(30, 10, 4);
        assert_eq!(tiny.rows.len(), 10);
    }

    #[test]
    fn lobby_selector_reserves_space_below_all_captions() {
        let scene = scene();
        for rows in [24, 32, 40] {
            let original = scene.lobby(100, rows, 5);
            let panel = scene.lobby_with_bottom_space(100, rows, 5, 5);
            assert_eq!(panel.rows.len(), rows);
            assert_eq!((panel.left, panel.width), (original.left, original.width));
            assert!(usize::from(panel.below) + 5 <= rows);
            if usize::from(original.below) + 5 <= rows {
                assert_eq!(panel.tail_start, original.tail_start);
            }
        }
    }

    #[test]
    fn text_art_file_is_used_when_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(BANNER_FILE), "subtitle: MINE\nAB\nCD\n").unwrap();
        let mut config = AppConfig::default();
        config.display.banner = true;
        let mut paths = miyu_base::paths::MiyuPaths::new().unwrap();
        paths.config_dir = dir.path().to_path_buf();
        let scene = BannerScene::load(&config, &paths, PersonaLane::Active).unwrap();
        assert_eq!(scene.art.subtitle, "MINE");
        assert_eq!(scene.art.rows(), 2);
        config.display.banner = false;
        assert!(BannerScene::load(&config, &paths, PersonaLane::Active).is_none());
    }
}
