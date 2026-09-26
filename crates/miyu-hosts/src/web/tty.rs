//! 网页端回写到发起它的终端。
//!
//! 从网页发的消息，如果这个会话本来是在某个终端里开的，输出也该出现在那个终端
//! 上。`origin_shell_at_prompt` 判断那个终端此刻是不是停在提示符处——正在跑别的
//! 命令时插一段输出会把人家的界面搅乱。
//!
//! `parse_stat_pgrp_tpgid` 读 /proc 比较进程组与前台进程组，这是「shell 空闲」
//! 的可靠判据。

pub(in crate::web) enum TtyWriteOp {
    /// 原样写一段（抬头之类）。
    #[allow(dead_code)] // 回写指令的一档,测试构造
    Write(String),
    /// 回合里的一条事件（和 IPC 发给终端的 `(kind, data)` 同一份），写线程上的
    /// 渲染器把它画出来。
    Event {
        kind: String,
        data: serde_json::Value,
    },
    /// 正常收尾:flush 后给 shell 发 SIGWINCH 促使重绘提示符。`interrupted` = 回合
    /// 没正常跑完（失败/取消），末尾标一句。
    Finish { interrupted: bool },
    /// 中途收笔(前台被占/超时):已写的留在屏上,不再动那个终端。
    Abort,
}

/// 回写线程上那台渲染器的配置：档位照用户的配置，宽度按那个 tty 量。
pub(in crate::web) struct TtyRenderSetup {
    pub(in crate::web) reasoning_mode: crate::render::ReasoningDisplayMode,
    pub(in crate::web) tool_call_mode: crate::render::ToolCallDisplayMode,
    pub(in crate::web) readable_tool_names: bool,
    pub(in crate::web) command_output_lines: usize,
    pub(in crate::web) thinking_scroll_lines: usize,
    pub(in crate::web) cross_session_preview_lines: usize,
    pub(in crate::web) cols: u16,
    /// 抬头上的任务名。
    pub(in crate::web) title: String,
}

impl TtyRenderSetup {
    pub(in crate::web) fn from_config(
        config: &miyu_base::config::AppConfig,
        cols: u16,
        title: String,
    ) -> Self {
        Self {
            reasoning_mode: crate::render::ReasoningDisplayMode::from_expand(
                config.display.expand_reasoning,
            ),
            tool_call_mode: crate::render::ToolCallDisplayMode::from_expand(
                config.display.expand_tool_calls,
            ),
            readable_tool_names: config.display.readable_tool_names,
            command_output_lines: config.display.command_output_lines,
            thinking_scroll_lines: config.display.thinking_scroll_lines,
            cross_session_preview_lines: config.display.cross_session_preview_lines,
            cols,
            title,
        }
    }
}

/// 那个 tty 有多宽（`TIOCGWINSZ`）。量不到就按 100 列——总比 daemon 自己那个
/// 不存在的 stdout 靠谱。
pub(in crate::web) fn tty_cols(tty: &std::fs::File) -> u16 {
    use std::os::unix::io::AsRawFd;
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } == 0;
    if ok && size.ws_col > 0 {
        size.ws_col
    } else {
        100
    }
}

/// 三道闸的第 2、3 道:shell 活着、还挂在记录的 tty 上、且自己就是终端前台
/// 进程组(即停在提示符,没在跑别的程序)。
pub(in crate::web) fn origin_shell_at_prompt(origin: &miyu_core::ipc::OriginTty) -> bool {
    let pid = origin.shell_pid;
    let Ok(stdin_target) = std::fs::read_link(format!("/proc/{pid}/fd/0")) else {
        return false;
    };
    if stdin_target != origin.path {
        return false;
    }
    in_terminal_foreground(pid)
}

/// 发起这一轮的一次性命令还在那个终端的前台吗——在的话后面的轮它自己在画（等子代理，
/// 见 `OriginTty::follower_pid`）。进程没了、pid 被别的终端里的进程复用、被挂到后台都
/// 算不在。它的 stdin 常是管道（`--stdin` 喂正文），和记指纹时一样按 stderr→stdout→stdin 认。
pub(in crate::web) fn origin_follower_in_foreground(origin: &miyu_core::ipc::OriginTty) -> bool {
    let Some(pid) = origin.follower_pid else {
        return false;
    };
    let on_origin_tty = [2, 1, 0].into_iter().any(|fd| {
        std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).is_ok_and(|target| target == origin.path)
    });
    on_origin_tty && in_terminal_foreground(pid)
}

/// 进程自己就是它那个终端的前台进程组。
fn in_terminal_foreground(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    matches!(parse_stat_pgrp_tpgid(&stat), Some((pgrp, tpgid)) if pgrp == tpgid)
}

/// /proc/pid/stat 的 comm 字段可含空格和括号,必须从最后一个 \')\' 之后再按空白
/// 切:其后第 3 个字段是 pgrp,第 6 个是 tpgid。
pub(in crate::web) fn parse_stat_pgrp_tpgid(stat: &str) -> Option<(i64, i64)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let pgrp = fields.nth(2)?.parse().ok()?;
    let tpgid = fields.nth(2)?.parse().ok()?;
    Some((pgrp, tpgid))
}
