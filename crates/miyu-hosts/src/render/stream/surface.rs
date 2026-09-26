//! 场所能力位：这个渲染面**能做什么**，而不是「它是哪一档」。
//!
//! 在此之前，过程渲染靠四个模式位（`plain` / `live_summary` / `tool_call_mode`
//! / 全局块开关）现场派生两个谓词（`timeline_enabled` / `timeline_static`），
//! 然后在二十几处就地问它们。问题不在于谓词本身，而在于**同一个谓词在不同地方
//! 回答的是不同的问题**：`timeline_static()` 在一处的意思是「这步跑完立刻落
//! scrollback」，在另一处是「段末不写 `Worked for`」，在第三处是「详情就地印、
//! 没处点开」。三件事碰巧在今天的档位组合下同真同假，于是被写成了一个判断——
//! 哪天要拆开（比如「全屏但不自动收起」），就得把二十几处逐个重读一遍才知道
//! 该改哪些。
//!
//! 这里把它们拆成各自有名字的能力位。**第一步只是改名**：`caps()` 从同样的四个
//! 位算出来，与旧谓词逐字节等价（`surface_caps_match_the_old_predicates` 穷举
//! 24 组合钉死）。收益在下一步——要加「不自动收起」，改的是 `caps()` 里一行，
//! 不是二十几个调用点。
//!
//! ## 一共有几个面，各自谁在用
//!
//! 用户数的是五条路（shellhook、inline、TUI、前台子代理浮层、后台子代理浮层）。
//! 按渲染层真正的判定重新数，**主线是六个面，浮层是两个组装器**——而且
//! shellhook 与 inline **从来不是两个面**：渲染层零分支，`try_run_remote_chat`
//! 也是同一个循环，唯一差别是宿主给不给活动区（`live: Option<&mut LiveReplTail>`）。
//! 过去文档一直拿宿主的名字称呼同一个面，才显得像两样东西
//! （`docs/plan/2026-09-17-render-unification.md` §5）。
//!
//! | 面 | plain | 目的地是终端 | 全屏 | 过程怎么画 | 宿主 |
//! |---|---|---|---|---|---|
//! | S0 Plain | 是 | – | 否 | 只有正文 | `--plain` |
//! | S1 Pipe | 否 | 否 | 否 | 老的一行摘要 `~ 工具×1 ok` | stdout 接管道 |
//! | **S3 Static** | 否 | 是 | 否 | 静态时间线：每步落 scrollback、无块、无收缩行 | shellhook／单次 `miyu "…"`／inline REPL／唤醒跟进／daemon 回写 |
//! | **S4 Full** | 否 | 是 | 是 | 可展开时间线 + 收缩行 | 全屏 TUI |
//!
//! **这张表里再没有「档位」这一列了。** 原来有两个多出来的面：S2 Cards（非全屏 +
//! `tool_calls=full` → 旧工具卡片、整条时间线消失）和 S5（全屏 + `tool_calls=full`
//! → 命令走时间线、别的工具打卡片，两种版式叠在一屏上）。两个都**不是设计出来
//! 的**，是「显示档位」顺手决定了「走哪条路」的副产品。
//!
//! 09-17 两步收干净：先是 `captures_tools` / `captures_reasoning` 改成「有时间线
//! 就收进去」（S5 消失），再是 `timeline_static()` 去掉 `tool_call_mode == Summary`
//! 那道闸（S2 消失，用户原话：「非全屏和全屏的路径不是已经统一了吗，为什么你还
//! 在分」）。现在档位只有一件事可管：**内容默认看不看得见**——能点开的面上是
//! 「这一步出来就是展开态」（`Step::open`），点不开的面上是「正文就地印在抬头
//! 底下」。
//!
//! 「目的地是终端」这一列就是 `live_summary`：它问的不是「我的 stdout 是不是
//! 终端」，而是「这些字节最后进不进一个有活动区的终端」——daemon 往 shellhook
//! 的 tty 回写时两者分家，所以外面只能用 `use_terminal_surface` /
//! `use_piped_surface` 选，拿不到那个字段。
//!
//! `caps()` 是方法不是构造时快照：全局块开关在测试里是线程局部、用例内开关
//! （`blocks.rs` 的 `set_enabled`），快照会把那批用例全打翻。

/// 这一步的详情放哪。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetailPlacement {
    /// 就地印在抬头底下——没处点开，看不到就是真看不到。
    Inline,
    /// 收进块里，点开才看。
    Behind,
}

/// 一个渲染面能做什么。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceCaps {
    /// 有登记处、能点开。
    pub expandable: bool,
    /// 步跑完立刻落 scrollback（而不是攒在活动区里等收段）。
    pub commit_immediately: bool,
    /// 段末写收缩行（`› Ran N commands · …`）。
    pub fold: bool,
    /// 详情放哪。
    pub detail: DetailPlacement,
}

impl SurfaceCaps {
    /// 详情就地印吗。
    pub fn detail_inline(self) -> bool {
        self.detail == DetailPlacement::Inline
    }
}

impl crate::render::StreamRenderer {
    /// 这个面能做什么。**只由「能不能点开」和「收不收段」两位决定**——
    /// 显示档位一位都不参与（见模块头）。
    pub fn caps(&self) -> SurfaceCaps {
        let expandable = crate::render::blocks::enabled();
        // 用户 todolist:21「TUI 不自动收起 Worked for」——不收段 = 每一步跑完
        // 就地落下去，和逐步落地的面一个走法。**它只管收不收段**：那些步照样
        // 挂块、照样点得开，详情照样收在块后面（见 `commit_static_steps`）。
        // 「有时间线但点不开」的面（shellhook／单次／inline）：详情没处收，只能
        // 就地印。管道那种连时间线都没有的不算——那儿走的是老的一行摘要。
        //
        // 09-17 之前 `timeline_static()` 还挂着 `tool_call_mode == Summary`，
        // 于是档位一调就换面；现在它只问「不是全屏、但字节进终端」。
        let inline_detail = self.timeline_static();
        let keep_open = expandable && !self.fold_timeline;
        let commit_immediately = inline_detail || keep_open;
        SurfaceCaps {
            expandable,
            commit_immediately,
            fold: self.timeline_enabled() && !commit_immediately,
            // 详情摆哪只看**能不能点开**，不看档位。档位（展开 / 收起）决定的是
            // 「默认看不看得见」，不是「走哪条路」——那正是 09-17 拆掉的耦合。
            detail: if inline_detail {
                DetailPlacement::Inline
            } else {
                DetailPlacement::Behind
            },
        }
    }
}
