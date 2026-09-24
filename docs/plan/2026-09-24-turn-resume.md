# daemon 重启后断点续跑（2026-09-24）

目标（用户 09-24 定为关键项）：daemon 一重启（换二进制、崩溃、被杀），跑到一半的回合
不再要人重发，新 daemon 起来后自己接着做。参考 opencode v2 的
`packages/core/src/session/execution/restart.ts` 与 `specs/v2/session.md`。

## 1. 改之前是什么样

- 「执行中」的回合在库里是 `turns.status = 'running'` + `owner_pid`。
- 回合 future 被丢下时，`PendingTurnGuard::drop` 把它收成「已中断」并记账。
- daemon 有序关停（`ActorCommand::Shutdown`）先把所有在跑的回合**按用户取消**收掉：删排队
  消息、发 `run.cancelled`、收成「已中断」。重启后库里跟人按停止的一模一样，分不出来。
- 崩溃 / SIGKILL：回合一直挂在「执行中」，**直到某个回合开跑**才被顺手收成「已中断」
  （`recover_stale_running_turns` 是懒的，调用点散在 daemon、成员库首次打开、TUI 进程
  自己的 `slash_context.rs` / `direct.rs` 等十几处）。实测：旧二进制 SIGKILL 后重启，那一轮
  一直是 running（黑盒 `crash_marks_the_orphan` 在旧二进制上 status=running）。
- 模型那边其实早就准备好了：中断回合按流水账回放（`interrupted_turn_replay_messages`），
  带 `<interrupted-turn-recovery>` 说明，没跑完的工具调用标成
  "tool execution was interrupted before a final result was persisted"，附最后进度和命令
  输出尾巴；每一轮工具跑完都 `checkpoint_tool_flow` 落盘。缺的只是「重启后没人接着跑」。

## 2. 做法

**认领记在流水账里，不升 schema。** 收尾孤儿回合的那一刻（`recover_stale_running_turns`，
谁先碰到库都一样）在它的流水账里追加一条 `restart_orphaned`（带「死之前最后一次动静」），
daemon 处理完再追加 `restart_resume`（resumed / skipped / failed + 原因）结案。升 v41 会让
还没合并的并行分支（只认到 v40）的二进制拒开这个库；流水账的读者（回放、投影、资产恢复）
都只认自己那几种事件，多两种看不见。

**有序关停保留认领。** `run()` 在叫 actor 关停之前立 `miyu_base::process::begin_daemon_shutdown()`：
- 回合守卫（`settle_unfinished_turn`）看到它只记用量、状态留在「执行中」；
- 回合任务被取消时直接让出运行登记，不删排队消息、不发 `run.cancelled`、不重投；
- `finish_turn_task` 关停时不动队列（孤儿收尾会把排队消息并进被打断那一轮，续跑时一起回）。

**daemon 起来后接着跑**（`web/actor/restart_resume.rs`，投递、平台、子代理宿主装好之后）：
翻出每个账号库里没结案的认领，**被重启打断的一律接着跑**。第一版设过次数上限（3 次）和时间
窗（本地 30 分钟、QQ 10 分钟），子代理和目标续轮也不接；验收时用户否掉了（09-24：「需要续的
是运行中因为重启 daemon 被打断的，没必要设置次数上限，也没必要设置时间范围……无论是这个轮
本身还是他开的子代理都能不受影响地继续正常跑，qq 那里同理」），v2 全部拿掉：

| 情形 | 去向 |
|---|---|
| 用户会话 | 投一条 `<service-restart attempt="N">`（`deliver()`，闲着就起一轮，工作目录沿用 `turns.workspace`） |
| 子代理会话（深的先接） | 挂回父会话名下起一条后台镜像任务（`subagent::reattach_background_child`），在子会话里接着跑，跑完照后台子代理那套把结果送回父会话 |
| 子代理自己的回合已结束、在等被接回的孙代理 | 给它挂一条只等不跑的任务（宿主端口 `watch_child`），它收尾时结果才回得到主会话 |
| 父会话那一轮也被打断了 | 续跑消息里列出接着跑的子代理（`- job <id>, session <id>: "<名字>"`），让它别再派一遍 |
| `/goal` 续轮 | 带着续轮的身份（`TurnOrigin::GoalRound`）接着跑，自动续轮重新武装（用户 09-24「接着跑，自动续轮也恢复」）；外壳带 `goal-round="true"`，再被打断还认得出 |
| QQ 会话 | 等账号连上（多久都等，放后台不挡别的认领），走 `onebot::wake_conversation_for_restart`；等的时候群里开了新的一轮就不接 |
| 续跑的那一轮又被打断 | 次数加一，照样接（界面写「第 n 次」），不设上限 |
| 一次性 `miyu "…"`、归档的、已删的 | 不续（接不了） |
| 之后又有了别的轮 | 不续（人已经接着聊下去了） |

续跑消息（模型可见，英文）："The Miyu service restarted while you were working, so your previous
reply was cut off. Continue from where it stopped. A tool call marked as interrupted may or may not
have taken effect, so check before repeating it." ——最后一句是底线：说不清做没做成的副作用，
系统不替模型重放。

**界面**：续跑那一轮画成一行提示「↻ Miyu 重启了，接着上一轮继续」（第 2 次起带次数），
不画成用户气泡：终端（实时挂上、库里补印、历史回放、排进在跑的轮）与网页（实时、刷新回看）。
TUI 靠 `jobs_overview` 的 `from_start` 挂上续跑轮；续跑轮在它挂上之前就跑完了，就从库里补印
（`background_report_replies_after` 认续跑轮）。

## 3. 验证

| 测具 | 结果 |
|---|---|
| core 单测（restart.rs 8 + service_restart.rs 7） | 全过；「三天前的孤儿照样翻出来」退回 48 小时回看窗口会红 |
| engine 单测（关停守卫 2） | 全过 |
| hosts 单测（去留判定 6） | 全过；次数上限、时间窗、子代理、目标续轮四条退回 v1 都红 |
| `testkit/turn-resume/run.py` 黑盒（SIGKILL / SIGTERM / 用户停止 / 前台子代理 / 目标续轮 / 连着被打断四次） | 13/13 |
| `testkit/turn-resume/tui.py` 全屏 TUI 走查 | 4/4 |
| `testkit/turn-resume/webui.py` 网页走查 | 4/4 |

## 4. 没做的 / 已知边界

- **不保证工具只执行一次**（opencode 同样不保证）：被打断的那次调用可能已经生效，系统不重放，
  由模型先查再决定。
- QQ 群聊续跑时发起者身份不知道（库里没存哪条消息触发的那一轮），降级成机器人自己、受限
  工具面，不凭空给权限；私聊按对端。
- QQ 群聊那一轮的正文前面垫着群聊记录（real_context 插件），续跑外壳落在末尾：数「第几次」
  用 `restart_chain_attempt`（开头或收尾那个完整外壳都认），界面显示仍只认开头
  （`service_restart_attempt`），正文里引用到这个标签的普通消息不会被画成续跑提示。
- 后台**命令**（`run_command` 后台）随 daemon 一起死，重启后不报「被打断了」（opencode 会补
  一条「命令因服务重启被取消」）；只在等后台命令、自己没有被打断的回合的子代理也不接。想要
  的话是下一步。
- 没有次数上限：一个每跑必把 daemon 弄死的回合会拖着 daemon 反复重来（用户 09-24 拍板不设）。
- `miyu daemon stop` 之后不管多久再启动也会接着跑（和重启不分）。
- 直连模式（`MIYU_DIRECT=1`）的客户端进程死掉留下的孤儿，下一个 daemon 也会接着跑。
