//! 全屏往前翻页：回放只画最近一屏，往上翻到顶，再从库里补更早的一页。
//!
//! 会话项目第 2 段（用户 09-24 定：全屏完整回放、非全屏只印最近一屏）。以前固定
//! 回放 `display.repl_replay_turns` 轮，更早的往上翻也翻不到。

use super::*;
use crate::cli::history_replay::ReplayScreenPage;

/// 往前取一页：给游标（`before_seq`），还一页；没有了就还 None。
pub(in crate::cli) type OlderPageLoader =
    Box<dyn FnMut(i64) -> anyhow::Result<Option<ReplayScreenPage>>>;

/// 这条会话里还没画出来的更早那部分。
pub(in crate::cli) struct OlderPages {
    before: i64,
    load: OlderPageLoader,
}

impl OlderPages {
    pub(in crate::cli) fn new(before: i64, load: OlderPageLoader) -> Self {
        Self { before, load }
    }
}

impl Screen {
    /// 回放完第一页之后交进来；None = 已经是最早的了。
    pub(in crate::cli) fn set_older_pages(&mut self, older: Option<OlderPages>) {
        self.older = older;
    }

    /// 视口顶到了最上面、更早的还在库里：往前补一页，看到的内容原地不动。返回补
    /// 没补。
    ///
    /// 缓冲快满时就不再补：再往前接，`feed` 超过上限会从最前面裁，等于白接。这时
    /// 停下来提示一句。
    pub(in crate::cli) fn load_older_at_top(&mut self) -> bool {
        if self.scroll > 0 {
            return false;
        }
        let Some(mut older) = self.older.take() else {
            return false;
        };
        let page = match (older.load)(older.before) {
            Ok(Some(page)) => page,
            Ok(None) => return false,
            Err(error) => {
                tracing::debug!(error = %error, "loading an older replay page failed");
                return false;
            }
        };
        let mut earlier = Term::default();
        earlier.set_content_cols(content_cols(self.cols));
        earlier.set_cols(usize::from(self.cols));
        let frame = self.take_graphics(&page.frame);
        earlier.feed(&frame);
        if self.term.line_count() + earlier.line_count() > MAX_LINES {
            self.toast(miyu_base::i18n::text(
                "Nothing older fits here; /history has the full record",
                "更早的放不下了，完整记录用 /history 看",
            ));
            return false;
        }
        let view_before = self.view_len();
        let added = self.term.prepend(earlier);
        // 视图索引、行缓存都按缓冲行号记，行号整体挪了，全部作废。
        *self.view_index.borrow_mut() = None;
        self.row_keys.clear();
        let grown = self.view_len().saturating_sub(view_before);
        self.scroll += grown;
        if self.floor > 0 {
            self.floor += grown;
        }
        if let Some(selection) = self.selection.as_mut() {
            selection.anchor.0 += added;
            selection.cursor.0 += added;
        }
        if let Some(before) = page.older {
            older.before = before;
            self.older = Some(older);
        }
        self.invalidate();
        added > 0
    }
}

impl crate::cli::repl::tail::LiveReplTail {
    /// 回放完第一页之后交进来。非全屏没有屏，交了也不存：只印最近一屏。
    pub(in crate::cli) fn set_older_pages(&mut self, older: Option<OlderPages>) {
        if let Some(screen) = self.screen.as_mut() {
            screen.set_older_pages(older);
        }
    }
}
