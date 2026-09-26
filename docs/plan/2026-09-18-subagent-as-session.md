# 子代理会话化 + 孙代理施工单

2026-09-18。课题：允许子代理再开一层「孙代理」（孙代理不能再开），同时把子代理从
`SubagentRunner` 独立小循环改成**真会话**：看子代理 = 切到那条会话，给它发消息 =
往那条会话排一轮。目的有两个：normal 模式的 AI 用 `dev=true` 开出的开发子代理能自己
再分工；浮层那条 bug 多的额外渲染路径整个退役。

可行性核实（三线只读探查，file:line 全在下文）结论：**可行，无硬阻断**。硬前提四条都对上了：
端口机制是异步的（`host_ports/ports.rs:103-118` 返回 `BoxFuture`）；daemon 内部起轮不查
kind（`web/actor/mod.rs:46-70`，kind 只在客户端入口 `TURN_TARGET_KINDS` 挡）；`Agent`
每轮新建、构造无副作用（`agent/setup.rs:55-58`）；REPL 起轮本来就带显式 `session_id`
（`remote/one_shot.rs:57`），不靠 daemon 指针。

## 0. 定论（用户 09-18 拍板）

- 一步到位：后台孙代理一起做。深度写死，0 主会话 / 1 子代理 / 2 孙代理，孙代理面上没有
  `subagent` 工具，调用侧也拒。
- 子/孙会话全部落库，是普通会话（`kind='subagent'`），历史在 `turns` 表。**只在主会话
  reset 或删除会话时清理**；`/undo` 不动子会话；现有「子代理会话 7 天 GC」退役。
- 「任务完成」= 子会话没有活动回合 **且** 名下没有未完成的后台任务（后台命令、后台孙代理
  都算）。不设超时；不结束的后台命令由用户自己停。
- 不做并发上限；现有 `tools.subagent_concurrency`（一批 4 波）保留作为一批内的扇出节拍。
- 子/孙代理消耗都进统计账本（走 `pinned_for_turn` 记到属主账户）；footer 的 Σ 含整棵树。
- `/session` 只管主会话，**不显示**子会话；子会话另开 `/subagent`（树形选择器）与 `/back`。
- 前端：点时间线状态行或任务条行切进子会话；任务条首行「↑ 主会话」切回（Claude Code 的 main
  行）；访问子会话**不动车道指针**；WebUI 同样改切换形式，`.sub-blocks` 内联时间线退役。

## 1. 数据模型

`sessions` 表已有 `kind`、`parent_session_id`（FK ON DELETE CASCADE，`migrations/baseline.rs:149-168`）。
迁移 v39 加四列：

| 列 | 含义 |
|---|---|
| `depth INTEGER NOT NULL DEFAULT 0` | 0 主会话，1 子，2 孙；建行时 = 父 depth + 1 |
| `task_state TEXT` | `running` / `waiting` / `done` / `failed` / `cancelled` / `interrupted`；主会话为 NULL |
| `spawned_by_turn TEXT` | 父会话里开它的那一轮 turn_id，回放画链接用 |
| `background INTEGER NOT NULL DEFAULT 0` | 开的时候是不是后台 |

`turns.tool_flow` 的 `ToolFlowCall`（`conversation_db/types.rs:157-175`）加
`child_session_id: Option<String>`（serde default + skip_serializing_if，老行照读），替掉 `sub_trace`。

新增查询（`conversation_db/sessions.rs`）：
- `child_sessions(parent) -> Vec<SessionOverview + task_state + depth + turn_count + context_tokens>`
- `descendant_session_ids(root)`：递归 CTE，照抄 `persona_reset_session_ids`（`:340-365`）的形状，
  **去掉 `child.persona = ?1` 那条过滤**（普通主会话开的 dev 子代理人格是 `dev`，会被漏掉）。
- `session_token_totals`（`:692-722`）改成对 `descendant_session_ids` 里所有会话的 `turns`
  求和；`sessions.*_tokens` 那一项只对 `depth IS NULL` 的旧审计行保留，避免重复计。
- `mark_tasks_interrupted_on_boot()`：`task_state IN ('running','waiting')` → `interrupted`。

新合成轮标签：`SUBAGENT_REPORT_TAG = "<subagent-report>"`，与 `BACKGROUND_JOB_REPORT_TAG`
并列进 `state/mod.rs:405-414` 的 `is_synthetic_user_content`，**同时**补进 `session_replay` 的
SQL LIKE（`conversation_db/mod.rs:840-843`）、`web/app.js` 的 `isSyntheticTurnContent`、
`src/cli/repl/jobs.rs` 的抬头解析；core 单测 `synthetic_turn_markers_match_the_replay_sql` 顺手钉住。

## 2. 后端

### 2.1 引擎到宿主的接缝：`SubagentHostPort`

放 `crates/miyu-base/src/host_ports/`，照 `QqOutreachPort` 的样子（全局 `RwLock<Option<Arc<dyn …>>>`
+ `install_*` + `*_port()`，`ports.rs:145-164`），daemon 启动时在 `web/server.rs:164-165` 旁边装：

```rust
pub trait SubagentHostPort: Send + Sync {
    /// 建子会话并起第一轮。前台：future 在任务终态才完成，返回最后一轮正文；
    /// 后台：立刻返回子会话 id。
    fn spawn(&self, req: SpawnChild) -> BoxFuture<'static, Result<SpawnOutcome>>;
    /// 给已有子会话追话：跑着就排 follow-up，闲着/中断就起新一轮（同样按前后台等或不等）。
    fn continue_child(&self, req: ContinueChild) -> BoxFuture<'static, Result<SpawnOutcome>>;
}
```

`SpawnChild` 带：父会话 id（`workspace::try_session()`，`subagent/mod.rs:298-303` 现成）、
描述、prompt、`dev`、`tier`、`background`、`max_steps`。

**前台取消级联**靠 Drop：工具里没有任何取消令牌（`GuardCtx` 只有 `used_tools`，`turn_loop`
零 cancel 逻辑），父回合被停就是整个 `chat` future 被 drop（`web/turns/task.rs:588-600`），
工具 future 跟着 drop。所以端口返回的 future 里持一个 `ChildRunGuard`，Drop 时对子会话的
活动 run 调 `request_cancel`（`runtime/run.rs:67`），正常完成前 `disarm()`。子的 `chat` 被 drop
又会 drop 它等孙代理的 future，级联自动递归。后台开的不装守卫。

### 2.2 宿主侧：起子轮

新 helper `spawn_internal_turn(state, session_id, content, display, origin, audience, label)`，
把 `job_wake.rs:523-586` / `goal_driver.rs:288-330` / `ipc_server.rs:951` / `turns/mod.rs:386` /
`platforms/turn_run.rs:107` 五份手写的「mint run + 插 RunInfo + 记 events_after + 发 StartTurn」
收成一份。子轮**不走** `run_platform_turn`（它有全局 `turn_permits` 信号量和 `admin_busy` 早退，
嵌套会自锁，`turn_run.rs:44-71`）。

建子会话必须经**父会话所属的库**：`stores.for_session(parent)` +
`create_session_for_owner(persona, name, "subagent", parent, owner)`（`state/sessions.rs:260-271`）
+ `stores.note_session_owner`。今天的审计行走 `StateStore::new` 落管理员库、owner 为空
（`subagent/audit.rs:28-34`），成员开的子代理在会话化后会找不到库、记错账户、跨库 join，必须改。

`RunInfo` 加 `first_event_id: u64`（发 StartTurn 前 `events.latest_id() + 1`）与
`parent_run: Option<String>`、`foreground: bool`；`TurnOrigin` 加 `Subagent { parent_session }`。
`turn_id` 插入时恒为 `None`、事后回填，读者要容忍。

### 2.3 素档：子会话的 Agent

不加第三条 `PersonaLane`（两值 lane 全仓 20 多处按 `DEV_PERSONA` 分叉，`turn_mode_for_session`
每次 StartTurn 都按人格重推 lane，`web/sessions/mod.rs:338-352`）。改为一根**profile 轴**：
`Agent::new_for_audience` 加参数 `AgentProfile::{Persona, Subagent}`，`Dev` 仍由 lane 推。
`Subagent` 档在构造期（`agent/setup.rs:47-96`）做四件事：

- 系统提示词：非 dev 用 `src/prompts/subagent-general.md`（`persona_system_prompt` 加分支，
  `agent/prompt.rs:34-47`）；dev 子代理就是 dev 人格提示词本身，比今天「dev-prompt + host env
  + 一句约定」更接近 dev 会话的字节，前缀缓存反而更好共享。
- 子系统快照全关（记忆不建库、不注入、不写日记；人格提醒、语音、情绪关；技能不注册）。
- 预设对话跳过；`situational_tool_specs` 按 dev 那条走。
- 尾巴与 host env 照 dev 子代理今天的口径（dev 带 host env，普通不带）。

**工具面**：task.rs 的每轮注册处（`web/turns/task.rs:283-318`，`remember_fact` 那两行旁边）
按 session kind 处理：kind=subagent → 用父车道那张表（normal 或 dev，`TurnResourceCache`），
循环 `unregister` `SUBAGENT_EXCLUDED`（`subagent/mod.rs:91-106`，可见性从 `pub(in crate::tools)`
放宽）；depth ≥ 2 再 `unregister("subagent")`。这与今天模型看到的面一致（父的快照减排除表），
只是从「只滤定义」变成真摘掉。引擎侧再加任务本地 `workspace::subagent_depth()`，`subagent`
工具入口 depth ≥ 2 直接报错，两道闸。

### 2.4 监督器：任务状态机与父会话唤醒

新驱动 `web/subagent_supervisor.rs`，照 `goal_driver.rs:31-103` 的样子 `subscribe_live()`
只认终态**事件**（不认 `runs_changed`：`finish_run` 在 `publish_completed` 之前触发，
`task.rs:759-760`，靠它会读到空 `active_runs` 却没有正文）。

```
run.completed|failed|cancelled 且 session.kind == subagent
  → pending = jobs::running_jobs_for_session(sid) ∪ child_sessions(sid).filter(非终态)
  → pending 非空: task_state = waiting
  → pending 为空: task_state = done|failed|cancelled
      → 有前台等待者(HashMap<child_sid, oneshot>):resolve(最后一轮正文 = session_replay(1))
      → 否则(后台):对父会话发 <subagent-report> 合成轮
          父跑着:enqueue_turn_update(Followup)(job_wake.rs:481-521 那条)
          父闲着:spawn_internal_turn(父, …, TurnOrigin::JobWake 同款,job_wake:true 让 REPL 能发现)
          父是平台会话:广播文本(job_wake.rs:589 那条)
```

后台命令完成唤醒子会话走**现有** `job_wake`：job 的 `session_id` 是 `try_session()`
（`jobs/mod.rs:505,606`），子会话有自己的 id 后天然对；父闲则起轮、父忙则 follow-up。
这顺手修掉现状 bug：今天子代理开的后台命令套的是父会话 scope，报告发到父会话头上，子代理
自己早退了没人跟进。

`job_wake.rs:499` 那个「turn is still starting → 返回 None 靠下次重试」在监督器里没有重试
轮询，要改成等 `queue_target` 就位（`runs_changed` 上等）。

最后一轮正文用 `session_replay(1)`（`state/history.rs:130`），它过滤
`status IN ('completed','interrupted')`，被取消的子会话也能拿到半截正文
（`PendingTurnGuard::Drop` 会落 `interrupt_turn`，`agent/control.rs:59-70`）。

**父会话状态行的实时窥视**：监督器另发一条薄事件 `subagent.progress`
`{session_id: 父, child_session_id, state, step, tokens, elapsed}`，由子会话的
`tool.started` / `assistant.delta` 尾巴 / `chat.round_usage` 折出来，100 ms 节流。这一条替掉
`__subagent_*` / `__subtool_*` 十来种标记。

### 2.5 停止、重启、清理

- 停止：客户端 `Cancel` 只打当前会话的 run，前台后代靠 2.1 的 Drop 级联；后台子树不动。
- 重启：`server::run` 起步调 `mark_tasks_interrupted_on_boot()`，不自动续。
- `DeleteSession`（`session_cmds.rs:449-504`）与 `reset_actor_conversation`（`actor/mod.rs:510-564`）
  都加树遍历：对每个后代 `stop_session_runs` → `jobs::stop_session_jobs` →
  `forget_relay_sessions` / 中转池 `retire_session` → 删行（delete 走 cascade；reset 父行留着，
  子行显式删）。**注意现状：删会话只停回合、不停后台任务、不收中转进程**
  （`stop_session_jobs` 全仓只有 IPC `StopSessionJobs` 一个调用方），这次一并补。
- 关停：`ActorCommand::Shutdown` 扫全部 run（`actor/mod.rs:305-318`），子轮也在里面，
  5 秒兜底后落 interrupted；`jobs::shutdown_all` 与 `shutdown_relay_processes` 现成。
- `delete_subagent_sessions_older_than` 的启动调用（`web/server.rs:27-42`）去掉。

### 2.6 工具面向模型的形状

`subagent` 一把工具，参数：`description`、`prompt`、`dev`、`tier`、`background`、`max_steps`、
**`session_id`**（续接已有子会话：跑着就排 follow-up，闲着/中断就起新一轮）。`resume_id` 与
`send_subagent_message` 都并进 `session_id`，磁盘检查点整套退役（历史在库里就是断点）。
成功输出照旧纯文本：`subagent <state> · 子会话 <id>\nresult:\n<最后一轮正文>`；后台立刻返回
`{"ok":true,"kind":"background_subagent","session_id":…}`。提示词里加一句：起了后台任务的话，
任务要等它们回来才算完，不需要的后台任务记得停掉。

### 2.7 daemon 之外

`miyu tool-call subagent` 与 `MIYU_DIRECT` 直连 REPL 没有端口。工具里留 15 行兜底：本地库建
子会话 + `Agent::new_for_audience(…, AgentProfile::Subagent)` + `chat_stream` 跑到底；
`background=true` 报「后台子代理需要 daemon」。`SubagentContext`（`subagent/mod.rs:107-112`）
要补 `StateStore`（照 `goal/mod.rs:49-53` 的 `open_at_home` 写法）。`testkit/subagent-dev`
靠这条继续能跑。

中转线（claude-code / codex / agy）无需特殊处理：桥路径调工具前已套 `with_session` 等任务本地
（`session_cmds.rs:294-332`），端口在 daemon 里可得；子会话在中转线上就是另一个 CLI 进程。
前台子代理在对面 CLI 眼里仍是一次 MCP 工具调用，对面的工具超时是现状就有的风险，不变。

## 3. 前端

### 3.1 TUI

- **访问**：`visit_session(id)` = `apply_repl_session_switch`（`session.rs:360-437`）去掉最后一步
  `SetReplSession`；`RemoteRepl` 加 `visit_stack: Vec<String>`；`/back` 弹栈再走同一段。
  `SetReplSession` 继续拒 subagent kind，这正是「不动车道指针」的保证。`GetSessionState` 的
  kind 表放开 subagent；IPC 起轮的 `TURN_TARGET_KINDS`（`web/sessions/mod.rs:188`）放开
  subagent（要能给子会话发消息）。`repl_active_or_default_state`（`session.rs:934-955`）的
  `GetStatus` 兜底会报 `changed=true` 把人拽回去，访问期间要压住。
- **切进去的画面**：回放落库轮（`replay_recent_turns` 无 kind 依赖），第一条 user 按 kind 判
  画成暗色「⚙ 来自主会话的任务 · 标题」（`history_replay.rs:117-136` 加第三分支，**按 kind
  不按内容**）；有活动 run 就 `follow_wake_run`（`wake.rs:16-102`）接上，run id 从新 IPC
  `ListSubagentSessions` 拿；`follow_run`（`web/server.rs:232-315`）改从 `first_event_id` 回放，
  事件环 4096 条 / 4 MB 被挤掉就退回「从现在起」+ 库回放兜底。ESC 脱离不停轮，打字排 follow-up，
  都是现成行为。
- **任务条**：行模型从 `Vec<JobOverview>` 改成 `StripRow::{Parent, Child, Job}` 枚举，
  `background_job_lines`（`jobs.rs:25-82`）与点击命中（`tail_impl.rs:342-381` 那句
  `offset - 1 → jobs[i]` 是会断的地方）都用它；`LiveReplOutcome` 加 `VisitSession` /
  `BackToParent`。在子会话里首行「↑ 主会话 · 标题 · 状态」；主会话里列直系子会话（状态词
  运行中 / 等待后台 ×N / 已中断 / 完成）和自己的后台命令。数据来自 `SharedJobsFeed` 新增的
  子会话轮询槽（IPC `ListSubagentSessions`）。
- **时间线状态行**：保留 `timeline/live.rs:508-570` 那一行，窥视、token、秒数改吃
  `subagent.progress`；点击 = `VisitSession`（`tail_impl.rs:230-239` 那个 overlay 分支改掉）。
  跑完的行变静态行，`ToolFlowCall.child_session_id` 供回放画链接，回放也能点。
- **`/subagent`**：`PanelModel`（`panel.rs:31-62`）一个新模型，树用缩进的扁平行
  （`  ↳ 开发中·标题 · 12k · 等待后台 ×1`），Enter 访问，Esc 取消；命令表在
  `miyu-core/src/slash_commands.rs:41,139` 加两行（`web: false`），`interactive.rs:327-353`
  两个 match 臂，`direct.rs` 的穷举 match 跟着补。
- **footer**：`ReplFooterStatus` 加 `visit_depth`，`repl_footer_left_parts` 多推一段「子代理 ↳1」，
  窄终端降级的 `fixed_width` 算式（`footer.rs:293-300`）要算进去；每次 `from_config` 重建后
  照 `update_thinking_variant` 的样子重设。
- **shellhook 静态面**：子代理照旧一行，不可点；要看进 TUI。

### 3.2 WebUI

- 后端：`GET /api/sessions/{id}/subagents`（按父会话过 `require_local_web_session`）；
  `/turns` 与 `/context` 换一个 `require_visitable_web_session`（kind ∈ user/subagent，owner 按
  父会话核）；`/api/sessions` 列表不变（子会话不进 `state.sessions`，否则拖拽排序/删除全乱）。
- 前端：侧栏子会话缩进挂父下、可折叠，默认父有正在跑或已中断的子代理时展开；
  `buildSessionItem` 原样复用；打开走 `openSessionView`（`state.viewSessionId` 本来就是视图
  指针，`retireLiveRunsForSwitch` 保活离屏 run）；子会话视图顶部面包屑「← 主会话名」；
  任务条同样加「↑ 主会话」chip。
- 父会话里的子代理工具卡收成状态行（标题、窥视、token、秒数已有），点击打开子会话；
  `renderSubagentProgress` / `.sub-blocks` / `jobStreamSink` / `seedJobTrace` / `sub_trace`
  回放整套退役；`isSubagentTool` 判定与卡片头保留。
- 子会话的 run 事件带它自己的 `session_id`，视图切过去就是普通实时流，不用新通道。

## 4. 删除清单（验收后单独列给用户拍板，不夹进功能提交）

- engine：`subagent_runner.rs`（Runner、检查点、收件箱）、`subagent/{audit,log,protocol}.rs`、
  `send_subagent_message`、`jobs::spawn_background_subagent` 与 `JobKind::Subagent`、
  `record_subagent_usage`、`turn_loop/parallel.rs` 的 `tee_subagent_trace`、`tool_report.rs`
  的 `sub_trace` 组装。
- hosts：`render/stream/timeline/subagent.rs`（789 行）、`PANEL_CAPS` 第三张面、
  `blocks::Entry::overlay` / `register_overlay` / `is_overlay`、`JobTrace` IPC 与
  `jobs::job_trace*` 环、`delete_subagent_sessions_older_than` 的启动调用、
  `event_map` 里子代理标记的转发。
- cli：`screen/overlay/*`（2133 行）里子代理那半（**命令日志面板保留**，后台命令不是会话）、
  `tail_impl.rs` 的 overlay 点击分支、`repl/jobs.rs` 的 `trace_job`/`trace` 槽。
- web：`SUBAGENT_MARKERS` / `parseSubagentEvent` / `renderSubagentProgress` /
  `subEnd*` / `jobStreamSink` / `seedJobTrace` / `.sub-blocks` 相关 CSS。

## 5. 分阶段与验收

每段单独构建、部署、验收；后一段不开工。

1. **core + engine + hosts 骨架**：v39、查询、端口、`subagent` 工具重写（含直连兜底）、
   `AgentProfile::Subagent`、task.rs 工具面钩子、`spawn_internal_turn`、监督器、级联清理、
   `first_event_id` + `follow_run` 回放、IPC `ListSubagentSessions`、kind 表放开。
   前端此时只保证不炸：状态行照旧显示、标记流没了就只剩标题/token/秒数，浮层暂时点不开。
   验收：CLI 黑盒 `testkit/subagent-session/run.py`（下文）。
2. **TUI**：访问栈、`/subagent`、`/back`、任务条行枚举、footer、回放标签、状态行吃
   `subagent.progress`、`FollowRun` 接活轮。验收：`testkit/tui/subagent_visit.py` + 真机。
3. **WebUI**：接口两条、侧栏、面包屑、任务条 chip、卡片改行。验收：Playwright 走查 + 真机。
4. **删除清单**落地（用户拍板后）。

release note：`## 重要更新` 一条（子代理成会话、可切进去看和聊、孙代理），`## 修复` 一条
（子代理开的后台命令报告发到父会话）。

## 6. 测具

- `testkit/subagent-session/run.py`：沙箱 daemon + 桩 LLM（按提示词分阶段调工具：主 → 子 →
  孙，含 background 与 run_command background）。判定：
  `child_rows_have_depth_and_parent`、`grandchild_has_no_subagent_tool`（桩记录工具表）、
  `foreground_parent_blocks_until_done`、`background_child_wakes_parent_with_report`、
  `child_waits_for_background_command`（子结束回合 → waiting → 命令完成 → 子再跑 → done → 父收报告）、
  `grandchild_background_wakes_child_not_parent`、`cancel_parent_cascades_foreground_only`、
  `reset_kills_tree_and_stops_jobs`、`restart_marks_interrupted_and_reply_resumes`、
  `tokens_rollup_recursive`（Σ = 三层 turns 之和）、`member_child_in_member_db`、
  `no_orphans`（daemon 停后无残留进程）。
- `testkit/tui/subagent_visit.py`（pyte）：状态行点击 → 画面换成子会话且首行「⚙ 来自主会话的任务」
  → 任务条首行「↑ 主会话」→ 点它回来 → `/subagent` 面板列出树 → `/back`；footer 带「↳1」；
  `/session` 面板里**没有**子会话。
- `testkit/subagent-dev/` 继续跑直连兜底那条。

## 7. 风险与已知取舍

- 子代理每轮变成完整回合：进 `max_rounds`、每工具 180 s 默认超时（今天是 120 s）、自动压缩。
  长开发子代理能压缩是收益；`subagent` 工具自身 `timeout_seconds: 0` 保持。
- 前缀缓存：同一车道下所有子会话的工具面字节一致，子会话之间共享前缀；孙代理少一个工具，
  自成一套前缀，数量少可接受。
- 事件环回放长度有限，长回合切进去可能看不到最前面，靠库回放兜底。
- 平台父会话（QQ）收到的子代理完成是广播文本，与今天后台命令一样；QQ 里看不了子会话。
- 监督器与前台等待者是全新机制，是这次唯一「没有先例可抄」的部分，测具里那条
  `child_waits_for_background_command` 是它的主判定。

## 8. 施工记录(第一段,09-18)

与上文的偏差,都是施工时发现更省的路:

- **后台子代理仍走后台任务注册表**:`background=true` 时把「建子会话 + 等终态」整个包进
  `jobs::spawn_background_subagent` 的镜像任务里。于是任务条、`job(action=stop)`、完成唤醒
  (`<background-job-report>` 合成轮)全部沿用后台命令那一套,**没有新的合成轮标签**;唤醒
  报告里带的是子会话最后一轮的正文(写进任务日志的结果段)。后台孙代理的镜像任务归属
  子会话,完成自然唤醒子会话。「名下有没有未完成的后台任务」也就一处判据:
  `jobs::running_job_count_for_session`(命令 + 镜像任务)。§4 删除清单里
  `JobKind::Subagent` / `spawn_background_subagent` **不删**。
- **`subagent.progress` 薄事件没做**,改由宿主把子会话的回合事件折回老标记
  (`__subagent_reasoning__` / `__subtool_call__` / `__subagent_metric__` …)喂回父回合的
  进度通道(`web/subagent_host.rs::relay_child_events`)。现有 TUI 浮层 / WebUI 子过程时间线
  / 后台日志桥一字不改照旧能画,第二段前端切换时再把这条中继瘦成状态行要的那几样。
- 多了一个 `__subagent_session__<id>` 标记:子会话一建好就报,`tool_report` 据它落
  `ToolFlowCall.child_session_id`;后台镜像任务据它登记 job_id → 子会话,模型拿 job_id 追话
  两种 id 都认。
- `subagent` 工具参数 `session_id` 续接已有子会话:跑着就排 follow-up(立刻返回),闲着/
  中断就起新一轮(前台等、后台镜像)。`send_subagent_message` 暂留,daemon 里等价于
  `subagent(session_id=…, background=true)`。`resume_id` 只对 daemon 之外的老循环有效。
- daemon 之外(REPL 直连、`miyu tool-call`)沿用进程内老循环,没写 Agent 内联兜底。
- `RunInfo.first_event_id` 没加(六处手写 RunInfo);中继自己在入队前取 `events_after`。
  `follow_run` 回放留给第二段。
- `max_steps` 在会话化路径上暂不生效(子会话按 `tools.max_rounds` 封顶)。
- 顺手:`TURN_TARGET_KINDS` 放行 subagent(`miyu ask --session <子会话 id>` 能给中断的子代理
  回复;CLI 侧列表里找不到的 `sess_` id 走 `GetSessionState` 直取),`GetSessionState` 认子会话,
  删会话/reset 沿树停回合、停后台任务、收中转进程、删行,根会话自己的后台任务也停。
- 测具 `testkit/subagent-session/{run,stub}.py` 13 项全过;`testkit/subagent-dev` 仍走直连老循环。

## 9. 第二段（TUI）施工记录

见 `docs/plan/2026-09-25-会话项目第3段-子代理会话化.md`（会话项目第 3 段，09-25）。
