//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/cli/repl/tail/screen/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl Screen {
    /// 不碰终端的构造，只给测试用。视图映射那套行号算术值得单独钉住——
    /// 它算错一行的表现是「点哪儿选中的都是上一行」，从画面上很难看出来。
    pub(in crate::cli) fn detached(cols: u16, rows: u16) -> Self {
        Self {
            term: Term::default(),
            scroll: 0,
            follow: true,
            painted: Vec::new(),
            cols,
            rows,
            suspended: false,
            selection: None,
            pending_copy: None,
            needs_clear: true,
            overlay_spinner_started: None,
            body: None,
            expanded: std::collections::HashMap::new(),
            expanded_gen: 0,
            view_index: std::cell::RefCell::new(None),
            seed_stamp: None,
            open_seeded: std::collections::HashSet::new(),
            display_expand: (false, false),
            display_fold: true,
            display_command_lines: 8,
            display_thought_lines: 10,
            overlay: None,
            hover: None,
            input_rows: Vec::new(),
            input_selection: None,
            input_dragging: false,
            toast: None,
            floor: 0,
            older: None,
            row_keys: Vec::new(),
            command_hint: Vec::new(),
            hint_dismissed: false,
            force: true,
            banner: None,
            banner_shown: None,
            float_anchor: None,
        }
    }

    /// 测试入口：走的是和实况**同一条**路（含图形分流、行数封顶）。
    pub(in crate::cli) fn feed_for_test(&mut self, bytes: &[u8]) {
        self.feed(bytes);
    }

    /// 把「下一帧整屏重画」那一位拿走（测试用：模拟画过一帧）。
    pub(in crate::cli) fn take_force_for_test(&mut self) -> bool {
        std::mem::replace(&mut self.force, false)
    }

    /// 下一帧会不会整屏重画（测试用）。
    pub(in crate::cli) fn repaint_pending(&self) -> bool {
        self.force
    }

    /// 视口顶端落在视图第几行（量尺用）。
    pub(in crate::cli) fn scroll_for_test(&self) -> usize {
        self.scroll
    }

    /// 展开表里有几块（量尺用）。
    pub(in crate::cli) fn expanded_count(&self) -> usize {
        self.expanded.len()
    }

    /// 缓冲里有几块（量尺用）。
    pub(in crate::cli) fn block_count(&self) -> usize {
        self.term.blocks().len()
    }
}
