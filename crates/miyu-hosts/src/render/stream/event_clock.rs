//! 事件时钟：补发的事件按它自己发生的时刻掐表（会话项目第 3 段收尾，09-25）。
//!
//! 挂上来的终端从头补一轮时（切进正跑着的子会话、回合中回到主会话、第二个 TUI），事件是一口
//! 气喂进来的。按收到的那一刻掐表，做完的步骤全记成「<1ms」，还在跑的那一步从补发那一刻重新
//! 读秒。daemon 给每个事件记下发生的时刻，终端喂之前换算成本机的 `Instant` 设在这里。
//!
//! 只管「这件事发生在何时」：计时的起点、跑完时定格的耗时。「到现在跑了多久」的实时读数照旧
//! 拿真实时间减起点。

use super::StreamRenderer;
use std::time::Instant;

impl StreamRenderer {
    /// 接下来喂的这个事件是什么时候发生的；`None` = 就是现在（直连模式、老 daemon）。喂完要
    /// 清掉，别让转轮那一拍之类的非事件动作拿到过期的时刻。
    pub fn set_event_clock(&mut self, at: Option<Instant>) {
        self.event_clock = at;
    }

    /// 正在喂的这个事件的时刻（排队的正文片段冲刷时是它自己那一刻），没有就是现在。
    pub fn event_clock(&self) -> Option<Instant> {
        self.event_clock
    }

    /// 「这件事发生在何时」：计时的起点、跑完时定格的终点都用它。
    pub fn event_now(&self) -> Instant {
        self.event_clock.unwrap_or_else(Instant::now)
    }
}
