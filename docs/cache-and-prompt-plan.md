# Miyu 改进方案 v7（定稿）：高缓存命中率 + 精简提示词

> 五轮流程的最终定稿：四轮 Claude 研究（广度 → Miyu 对抗验证 → pi/Reasonix 实现深潜+供应商核查 → v3 落地性核查）+ 第五轮对两份外部 review 的裁定合并：
> - `cache-and-prompt-plan-deepseekreviewed.md`（v5，DeepSeek）
> - `cache-and-prompt-plan-gpt-reviewed.md`（v6，GPT）
> 本文取代 v4 成为唯一施工依据；v5/v6 文件保留作为审计记录。file:line 以 main (f851722) 为准。
> 大前提：**不影响 Miyu 现有功能和语义**。

## 〇、对两份外部 review 的裁定

### DeepSeek v5

| 主张 | 裁定 |
|---|---|
| "C3 重大错误：声称 adapter 归一化已完成实为零实现" | **不成立（措辞歧义）**。v4 原文"Phase 0.1 完成，1.7 只需复核"是排期语（归一化排在 0.1 做），从未声称代码已存在；Anthropic 侧从零实现本就是 v4 1.7 的内容。但歧义确实存在，v7 已改写。其派生论点——**Anthropic cache_control 排期降级、先吃 DeepSeek/OpenAI 兼容路径的自动前缀缓存（默认 provider 即 OpenAI 兼容）**——独立成立，**采纳**（→ Release 5 数据触发） |
| "B6 半真：signature 已捕获未回传" | **成立（亲测）**。`signature_delta` 在 `openai_compatible.rs:4363-4365` 捕获进 `state.thinking_signature`（测试 :5917-5928），但无任何请求侧消费点。v4 "流式保留 signature(:4272)" 表述不准；修法采用 GPT 的 provider-native envelope（→ Release 0） |
| "路径错误 src/web/onebot.rs 等" | **不成立**。v4 用的是缩写路径非错误路径。v7 统一全路径 |
| "43 工具引用 19 组 → 实为 13 组" | **成立（亲测）**：13 个组被懒工具引用，groups.json 24 条。修正 |
| "task tier amend 位置误标" | **部分成立**：v4 A4 引用列表漏 `task.rs:182`，补上 |
| "v4 缺 compactStuck 锁"（R9） | **不成立**（v4 4.1 已有）。R9 的阈值数字（0.5/0.6/0.8/0.9）采纳 |
| R5 "MCP 全部走稳定代理" | **部分采纳**。GPT 纠正：Reasonix 实为 pinned lazy registry + use_capability **双路径**（`plugin/lazy.go:168-196` 与 `usecapability.go:529-555`）。v7：MCP 走探针快照 pinned 语义 + **执行时实时鉴权**；代理化为数据触发项 |
| R7 tokPerChar 自校准 | **不采纳为默认**：Miyu 已内嵌精确 o200k BPE（token_counter.rs）。留作注记 |
| R1（字节稳定性基建前置）/R3（TRANSIENT 正则自动生成+幂等测试）/R4（探针快照瞬态合并）/R6（压缩逐字保留地板）/R12（辅助请求独立 scheduler 状态+重试复用自身前缀）/P1（工具三件套与静态化同批）/P2（miyu.md 审查表交付物）/P4（辅助隔离测试断言）/P7（append-only 工程规约）/P8（deferred tools 并列 experiments）/§〇遗漏 2（cooldown 纳入亲和判定）/遗漏 6（responses continuation.endpoint_id 锚点复用） | **全部采纳**（归属见正文） |

### GPT v6

| 主张 | 裁定 |
|---|---|
| Release 0 协议正确性独立先行 + **provider-native assistant envelope**（text/tool_calls/reasoning_content/thinking/redacted_thinking/signature/Responses opaque items 全链路贯穿） | **采纳**。单加 reasoning_content 字段确实不够（signature、Responses encrypted content 无处安放） |
| **三份内容模型**（raw_content / display_content / model_context sidecar） | **采纳**。v4 §三规则 2"transient 随 user_content 落库"会污染联想记忆/日记/整理器（它们消费 user_content）——v4 真实缺陷 |
| **QQ 字段拆分**：宿主产生的 trusted 字段（principal/admin/moderation 结论）与用户可控的 untrusted 字段（昵称/群名片/回复正文）分离承载；权限永远由代码判定 | **采纳**，替代 v4 2.1 的"发送者 JSON 整体搬 user tail"。trusted → 尾部 system（DeepSeek/OpenAI 兼容路径 mid-history system 已在用）；untrusted → user 尾部块 |
| **指纹拆分**（persona_compatibility_id + wire_prompt_hash，兼容集合而非单值）作为 C1 首选修法；`context_start_seq` 指针延后 | **采纳**。Phase 3 只需要"改 prompt 不删数据"；指针的 10+ 消费点状态机（状态层核查已列全）为数据触发项，实施前必须先写状态机规格 |
| 独立迁移（不共享 v12）；JSONL 结构化日志先行、llm_requests 表数据触发；telemetry keyed-HMAC/0600/轮转/保留期 | **采纳**（表 schema 保留备用） |
| 主 session 必须真实 endpoint sticky（挑战用户决策 2） | **部分采纳**：决策 2 保持（默认轮转是按次计费供应商的刻意设计，用户已确认）。采纳其 **cache_sticky=on 时的完整规则**（§Release 2b）；文档明示：默认轮转下多 key 配置的缓存收益天然受限，在意缓存请开 cache_sticky |
| "pi 无行为规训 / 450 token"表述纠正 | **采纳**：pi 有少量恒定 guideline + 条件注入，核心是"条件化注入 + 约束交给代码"，非极端删减 |
| v4 1.3-b.4 "新 load 攒批到 turn 边界"语义破坏（模型本 turn 就要用） | **采纳 GPT 修法**：每 turn 最多一次 S0→S1 schema 变更（首次 load 立即生效并锁定本 turn，后续排下一 turn 且返回明确状态） |
| "tools 会话内冻结"改名"目录静态 + 每 turn ≤1 次计划内变更" | **采纳**（不做虚假承诺） |
| sessions 单行快照跨 audience/mode 互相覆盖 → 进程内 generation cache 先行 | **采纳**（持久快照表数据触发） |
| B8 需 fragment/full-replay 双 fixture 后再定算法 | **采纳**（直接 replace 会坏真分片网关） |
| thinking capability 显式建模（Disabled/EnabledBudget/Adaptive + interleaved + signed），杜绝"默认 adaptive + 400 静默降级" | **采纳**（B6 的根治） |
| OpenAI prompt_cache_key / explicit mode / cache_write_tokens 已官方化 | **采纳**（capability 表按文档配置；实现时留一行核验） |
| usage 归一化互斥桶 + `read+write<=total` 守恒校验（不满足标 malformed，不做饱和减法掩盖） | **采纳** |
| Anthropic 首版仅 1 个 system 断点（+可选 automatic），不预建四槽滚动 | **采纳**（>20 block miss 由数据触发扩展） |
| persona/identity 变更"下会话生效"→"当前 turn 冻结、下一 turn 原子切换 generation" | **采纳**（Miyu 会话长驻） |
| coding 工具包（schema validation 前置、mutation queue、只读并行、bash 落盘、overflow 单次重试） | **采纳**（→ Release 4 附带） |

## 一、核心不变量（全工程约束）

1. **前缀即契约**：相同 generation + 相同历史 + 相同工具与设置，adapter 最终发出的稳定前缀必须逐字节相同。
2. **append-only**：所有新增动态上下文只能尾部追加，禁止 `messages.insert(n)` 中段插入；压缩/水位线推进是唯一例外且必须单调。
3. **authority 由代码承担**：sender/admin/permission 由真实 principal 在执行层判定；prompt 中的任何标签块不承担鉴权；TRANSIENT_TAGS 只解决边界与展示。
4. **三份内容分离**：`raw_content`（用户原始语义，记忆/联想/日记只读它）/ `display_content`（UI）/ `model_context` sidecar（宿主生成的结构化上下文，仅供确定性重渲染与 redo）。
5. **辅助请求隔离**：source ∈ {compact,judge,title,subagent,affection,organizer,vision} 一律独立 cache/session/scheduler 状态，且有测试断言。
6. **provider capability 显式建模**：late_system_role / developer_role / signed_thinking / thinking_mode / interleaved_thinking / cache_control / session_header_format / send_reasoning_content 按 provider+protocol+model family 配置，未知字段默认不发。

## 二、问题清单（v4 修订版，实施对照用）

沿用 v4 编号，仅列修正；未列项与 v4 一致。

- **A4** 补引用 `task.rs:182`（tier amend，per-Agent）；组数修正为 **43 个 Group 懒工具引用 13 个组**（groups.json 24 条；实施时以 `groups.json` 现算为准，不写死）。churn 五源：loaded 集合 / rescan_scripts（仅 Normal）/ **skills 目录经 `load_target_tool_xml` 双层嵌套**（registry.rs:647-654 + skills.rs:88-130，最大隐藏源）/ vision（A3）/ task tier。
- **A17/B6** 修法升级为 Release 0 的 provider-native envelope（signature 捕获已存在于 :4363-4365，缺的是持久化与请求侧重建全链路）。
- **C1** 首选修法改为**指纹拆分**（persona_compatibility_id + wire_prompt_hash，兼容集合）；`context_start_seq` 指针降为数据触发项（Release 5，实施前置 = 完整状态机规格：/clear、redo/undo、compact、running turn、session list、loaded items 全覆盖）。
- **C3** 措辞修正：Anthropic usage 公式归一化在 Release 1 实现；cache_control 断点在 Release 5（数据触发）从零实现。
- **B8** 修法改为双 fixture（fragment 累加 / full-name 重发）验证后选兼容算法。
- 新增 **C10**：transient 块不得写入 `raw_content`（记忆污染）；一律走 model_context sidecar。

## 三、目标请求布局

```
tools[]                                  [S] 注册期 canonicalize + 按名排序；每 turn ≤1 次计划内变更
top-level system                         [S] persona_core（人格/关系/风格）→ audience_policy → platform_policy(静态)
                                             → active_tool_guidelines（仅激活工具条件注入）→ kb/skills 三元组索引
history                                  [A] append-only；render_turn 确定性展开；snip/prune 走单调水位线
current user raw message                 [E] 本轮输入（= raw_content）
untrusted context tail                   [E] 昵称/群名片/回复正文/群聊历史块(2.2 后在 raw 前部)/联想记忆(带过时前言)
                                             /artifact manifest/图片提示 —— user 角色或 user 内块
trusted transport/control tail           [E] canonical principal/admin/moderation 结论/<mode-update> —— 尾部 system
                                             （provider 无 late-system authority 时留 top system 并接受该路径 miss）
runtime tail                             [E] <runtime now/cwd>（分钟级，决策 5）；hints；meme reminder（概率，产品语义）
```

- [S] 稳定前缀 / [A] 只增历史 / [E] 本轮瞬时尾部。目标是把不可避免的变化压到尾部，不是妄称全部不变。
- TRANSIENT_TAGS 单一真相源：正则从白名单自动生成（R3），全集开+闭标签转义（H1），幂等剥离测试；strip 仅用于 UI/标题。
- QQ 字段拆分（GPT §4.2.6）：principal_id/canonical_user_id/admin_role/moderation_decision = trusted typed sidecar 字段；nickname/card/group name/reply body/正文 = untrusted。禁止混合 JSON 整体提权。

## 四、发布序列（Release 0-5）

### Release 0：协议正确性（独立发布，不与缓存改动混发）

1. **provider-native assistant envelope**：text / tool_calls / reasoning_content(Option<String>，区分无键与空串) / thinking / redacted_thinking / signature / Responses opaque items（id/status/phase/encrypted_content）。贯穿 stream result → live history → journal → redo checkpoint（`#[serde(default)]` 兼容旧 BLOB）→ 崩溃恢复 → 下次请求。
2. DeepSeek thinking+tools：tool_calls 轮完整回传 reasoning_content（空串也发键）；最终答复轮不发；`content` 键在 tool_calls 轮发空串（严格网关）。per-endpoint compat 门控。
3. Anthropic：thinking/redacted_thinking block 请求侧重建 + signature 全链路；**thinking capability 显式建模**（Disabled/EnabledBudget/Adaptive/interleaved/signed），收紧 `anthropic_thinking_unsupported` 判据（协议形态错误 ≠ 模型不支持）。
4. 统一 finish/stop reason；`length` 时拒绝执行全部 tool call。
5. Responses continuation：store=false 时保留 encrypted content；同 namespace 锁 endpoint；跨 namespace 仅在完整 stateless replay 等价已验证时允许，否则 fail closed。
6. B8 双 fixture 后修复 name 累积。
7. thinking on↔off 混合会话不回溯改写历史字节。
- 验收：DeepSeek/Anthropic thinking+tools 多轮 fixture 无 400；旧 checkpoint 可读；length 零工具执行；signed-thinking mid-turn failover 有 fixture。

### Release 1：观测与字节回归网（无 DB 迁移）

1. Usage 扩展 + adapter 层归一化（互斥桶 input_uncached/cache_read/cache_write + input_total + usage_reported；守恒校验不满足标 malformed；四协议映射表见 v6 §6.5；DeepSeek hit/miss 顶层、reasoning_tokens、cache_write_tokens 预留）。
2. RequestOptions{scope, logical_request_id, cache_alias(keyed-HMAC+domain separation), cache_policy, store}；辅助请求默认 DisableIfSupported + 无 key + 独立 cursor；**测试断言辅助请求隔离**（P4）；DisableIfSupported 为 best-effort，DeepSeek 等记录 effective=ProviderDefault 不伪报。
3. **字节稳定性基建（R1 前置）**：system prompt 组装 / sorted tools / final provider request 三类 golden；两次相同构建逐字节相同；firstDivergence ±40 字符窗口报错。
4. WirePrefixShape 在 adapter 最终序列化后捕获（字段清单见 v6 §6.4：logical/physical attempt、keyed hash、layout_version、breakpoint_indexes、registry_generation、per_mcp_server_digest、first_divergence_component…）；**tool round 粒度**；首轮 prev=cur；仅成功路径更新。
5. telemetry：JSONL 结构化日志先行（0600、按大小轮转、默认保留 14 天、可关；不记 prompt 正文；keyed HMAC 防字典）；`llm_requests` 表（schema 见 v4）为数据触发项。**cache-stats**（detectMiss/min(prev,cur)-read、reportedCache 粘性、per-provider 噪声地板、compaction/generation 切换基准清零、换模型不豁免、UI 绝对值+金额）先基于日志实现。
6. mock byte-prefix e2e：断言第 N 轮请求（含 canonicalized tools）是 N-1 的字节前缀延伸；场景 = terminal/Web/QQ private/QQ group/tools churn/thinking on-off/failover/compact + `-no-reasoning` 对照；辅助请求识别并隔离；尾 3 轮均值 ≥90% 门禁，默认 warn-only、环境变量升 strict、输出 `CACHE_GUARD_RESULT:` 机器行。
7. 零风险速修包（v4 0.6）：`definitions_except` 排序（registry.rs:403，同修 subagent/deep_diagnose/linux_game 确定性）；`LOADER_TOOL_NAME` 常量（6 处字面量）；schema 字节确定性断言（serde_json 已是 BTreeMap，锁死防 preserve_order 意外开启）；`ToolDescription` 加 `summary` 字段 + `load_target_tool_xml` 改用（**摘掉嵌套 skills 目录，单项收益最大**）；A14 BTreeMap。

### Release 2：首批高收益缓存优化

**2a 动态 system 分层 + sidecar**
1. 独立迁移加 nullable `model_context_json`；start_turn 前 typed structs 一次组装冻结；redo/崩溃恢复读原 sidecar 不重查记忆/平台状态；老行 NULL 走旧 render；association/diary 只读 raw_content（C10）。
2. QQ/Web 静态 policy 留冻结 msg[0]；动态实例按 trusted/untrusted 拆分入 [E] 区（§三）；`<artifact-workspace>` 清单 → 尾部瞬时块。（09-24 C3：与请求里最近一份逐字节相同时不发，见 `STATE_SNAPSHOT_TAGS`。）
3. 联想记忆：不再 insert(1)；查询基于 raw；结果入当前轮 sidecar 尾部（带过时前言）；同步修 B1 replay_start；mode!=Chat 门控保留。
4. persona/identity 热编辑：当前 turn 冻结、下一 turn 原子切换 generation + 一次性 `<memory-update>` 注入；进程内 generation cache（key = persona+identity+audience+mode+tools_digest+model_override）；探针类（locale/MCP tools-list/脚本扫描）跨进程磁盘快照、瞬态失败不覆盖成功观测（R4；MCP 执行时仍实时鉴权——快照只保展示 schema，A19）。
5. SHELL/TERM 并入静态区；now 保持分钟级永不进 msg[0]（决策 5）。

**2b 端点与亲和（决策 2 框架内）**
6. `llm.cache_sticky` 开关（默认关）。**开启时**按 GPT §6.2 全规则：key=provider+model family+protocol+cache alias；continuation 存在时锁 endpoint；401/403 不同身份长重试；429/408/5xx 仅无 active continuation 时 rebind；rebind 记录 `endpoint_rebind` 并清该 session 缓存基准；cooldown 状态纳入亲和判定；复用 `continuation.endpoint_id`（openai_compatible.rs:1360-1365）现成锚点。**默认关闭时**：轮转保持，仍透传 prompt_cache_key/x-session-id（帮助网关内部亲和；文档明示多 key 轮转下缓存收益受限）。
7. 辅助请求独立 scheduler 状态；同一辅助任务的重试复用自身前缀（R12）。

**2c tools 目录静态化（一次计划内 cold start）**
8. `load_targets_xml`/`loadable_tools`/`lazy_definitions` 与 loaded 集合解耦（目录永远列全集）；C9 三条"减"路径修复（loaded 集合会话级 source_turn_id=NULL；retain 后移；冻结下强制 persist）；vision_analyze 恒注册 4 处（含 restricted registry 测试同步）；tool_limit_reached 保持 tools 只加尾部提示；**每 logical turn 开始完成 rescan 并冻结 registry generation；load_tools 每 turn 最多一次 S0→S1 变更**；`query_affection` 工具（决策 4）并入同一 cold start。
- 验收：不同 message_id 的两次 QQ 请求在历史尾部前逐字节相同；伪造昵称/回复体进不了 trusted 块；redo/restart 的 model context 字节不漂移；lazy load 每 turn ≤1 次 schema 变更；权限测试不回归。

### Release 3：Prompt 精简（先离线，后生产）

1. 先建 persona/safety/tool-routing eval；离线 replay + 新会话分桶，不直接动生产。
2. **miyu.md 去留表**（GPT v6 §5.2 为基准草案，实施时逐条确认）：人格/关系/语气核心保留压实；对话示例缩到高区分度少量；"逻辑自检/固定 80-90% 置信/每问必双搜"删除或改触发式；todo/loader 教程、AUR 流程、游戏数据源顺序、计算器强制 → 下沉到工具 snippet/guideline 或状态机；易过期 Linux 事实 → KB 加时间戳；外貌/喜好 → 按需 appendix；**Emoji 全禁 vs AUR/游戏要求彩色 Emoji 的冲突 → 实施时请用户定夺统一规则**。
3. **指纹拆分**（C1 修复，生产 rollout 硬前置）：persona_compatibility_id（兼容集合，回滚不删数据）+ wire_prompt_hash（变化只记 cold start）；`reset_history` 不再被 prompt 文案变化触发；A/B 分配按 session keyed 稳定跨重启。
4. ToolDescription 三件套（description/snippet/guideline）——与 2c 同一数据结构改造（P1）；工具清单 opt-in 进 system。
5. always_loaded 瘦身以 per-tool token 成本表定名单（83 工具合计 27.8k 字符基线；成本表由测试/诊断现算，不写死）。
- 验收：token 下降不是唯一门槛；persona/External 安全/工具路由 eval 不劣化；无效搜索与无效提问减少。

### Release 4：上下文维护 + coding 工具

1. `render_turn(turn, RenderMode{Context,Full})` 合并四份展开（先接口后存储）；A16 确定性失败/占位。
2. snip/prune 存储层单调水位线（v4 4.1 全量保留：rendered 列判定、MAX 推进、KeepErrors、SnipHinter 契约测试）；**压缩逐字保留地板**（R6：system/既有摘要/首个短用户轮/预算内小用户轮永不摘要化——"用户说过的话绝不蒸发"）；阈值 soft 0.5/snip 0.6/prune 0.8/force 0.9 全配置化；prune 后重评估跳过付费摘要；foldEconomics；compactStuck。
3. 冷恢复剪枝：`last_request_at` 独立迁移（DB 层写点 complete_turn_with_usage + interrupt；MAX 防回退；NULL→跳过）；vendor TTL 配置化不写死（DeepSeek 官方仅 "hours to days"，从时间序列估计）；cache_sticky 关闭时禁用。
4. compact 迭代式摘要 + 纯文本化"勿续聊"约束 + 重试 + 并发保护。
5. token 会计：真实 prompt_tokens 基准 + 增量 BPE（B5 三处调用点）；B2 群聊 token 会计修复（真实注入上下文计入）。
6. A18 幂等截断（private_reasoning_memory/private_tool_memory，只依赖 turn 自身内容）；观测 turn 边界 miss 占比。
7. coding 工具包（GPT §7.1）：registry 层 JSON Schema 校验（warn→error 渐进）+ 保留 prepareArguments 纠偏；错误统一回灌 tool error；写路径 mutation queue；bash 超窗落盘临时文件（保留 timeout/上限）；相邻只读工具并发、结果按序提交；overflow 仅一次 compact-and-retry。

### Release 5：数据触发的高级项（触发条件 → 项目）

| 触发证据 | 项目 |
|---|---|
| A18 turn 边界 miss 为主要成分 | 基于 sidecar 保存/重放完整 live 工具序列 |
| 删除式 reset / 历史回收成本高 | `context_start_seq` 指针（先写全状态机规格：/clear、redo/undo、compact、并发 turn、list、loaded items）+ `search_session_history` |
| 重启后 prompt rebuild churn 显著 | 独立 generation snapshot 表（非 sessions 单行） |
| Anthropic 主动使用且收益证明 | cache_control：首版 1 个 system 断点（+可选 automatic tail），>20 block miss 再加第二历史断点；不预建四槽 |
| 某 MCP server schema churn 为 tools miss 主因 | 该 server pinned placeholder 或稳定代理（执行时实时鉴权） |
| provider 原生 deferred tools 覆盖当前模型 | capability flag 实验（P8，与设计 a 并列；设计 a 若做必须入口 unwrap + 真实目标权限判定——Plan/Chat 提权风险） |
| schema token 仍是主要成本 | 以 per-tool 成本重审 always_loaded，不先造通用代理 |

## 五、迁移策略

当前 schema v11。每个持久化功能独立递增迁移（model_context_json / last_request_at / 指纹列 / telemetry 表各自独立），带 v11→latest、重复 open、rollback 测试；telemetry 不存 prompt 正文、turn 删除不 CASCADE 删账单；`model_context_json` NULL=旧 render 不回填。

## 六、测试与指标

- 必测矩阵（provider × scene × reasoning × tools × history）与 Cache 指标分桶见 v6 §10（首请求/低于最小缓存长度/显式 cold start/failover/模型切换/compact 后单列分桶不删除）。
- golden 四件套 + byte-prefix mock 门禁 + CI Cache-impact PR 元数据（cache-sensitive 路径：config.rs / registry.rs / openai_compatible.rs / agent/mod.rs 消息组装）。
- 上线门槛：先采 ≥7 天基线；correctness release 以协议 fixture 为门槛；缓存 release 需主 cohort cache_read 或 warm cost 统计可见改善且 endpoint_rebind 不升、任务/人格/安全不劣化；CI 先 warn-only 稳定两个 release 后升 hard gate。
- 目标参考：终端 DeepSeek 命中 >85%；群聊 Release 2 后 >60%（以 7 天基线修正）。

## 七、决策记录

用户决策 1-6 不变：① 群聊历史在前（experiments+A/B）② 端点默认轮转 + cache_sticky 开关（GPT 异议记录在案，opt-in 路径实现其全规则）③ 撤回墓碑 ④ 好感度移出注入改查询工具 ⑤ 时间分钟级不进 msg[0] ⑥ redo 联想记忆冻结（经 sidecar 实现）。
本轮内部定型：⑦ Release 0-5 发布序列 ⑧ provider-native envelope ⑨ 三份内容模型/sidecar ⑩ QQ trusted/untrusted 字段拆分 ⑪ 指纹拆分先行、指针延后 ⑫ 每 turn ≤1 次 tools 变更 ⑬ JSONL telemetry 先行 ⑭ MCP 快照 pinned + 实时鉴权，代理化数据触发。
待用户定夺（到相应阶段再问）：Emoji 规则统一（Release 3 审查表内）。

## 八、实施顺序

| # | 工作包 | cold start | 依赖 |
|---|---|---|---|
| 1 | Release 0 协议正确性 | 否 | — |
| 2 | Release 1 观测与回归网（含零风险速修包） | 否 | 1 |
| 3 | Release 2a sidecar + system 分层 + 联想记忆尾移 | 否（新 generation 自然 miss 一次） | 2 |
| 4 | Release 2b sticky/亲和/辅助隔离 | 否 | 2 |
| 5 | Release 2c tools 静态化 + query_affection | **是（一次）** | 2 |
| 6 | Release 3a 离线 eval + 三件套（与 3-5 并行开发） | 新会话无 reset | 2 |
| 7 | Release 3b 指纹拆分 + 生产 prompt rollout | **是（一次，不删数据）** | 3、6 |
| 8 | 观测一个完整发布周期 | — | 3-7 |
| 9 | Release 4 维护 + coding 工具 | 否 | 8 |
| 10 | Release 5 数据触发项 | 视项 | 8/9 |

## 八点五、实施状态（2026-08-06 首批落地）

已实现（全部测试通过，1229/0）：

| 项 | 内容 | 位置 |
|---|---|---|
| A17 第一层 | `ChatMessage.reasoning_content`（Option 区分无键/空串）；tool_calls 轮嵌入该字段（空串也发键）替代 system 注入；`content` 键在 tool_calls 轮保留空串；adapter 按 provider 白名单（deepseek/glm/zhipu/kimi/moonshot）发送、其余剥离保持字节形状不变 | llm/mod.rs, agent/mod.rs `push_assistant_message_with_reasoning`, openai_compatible.rs `prepare_chat_messages_for_provider` |
| B6 半修 | Anthropic `thinking` block 变体 + signature 从流状态贯穿 ChatResult → 请求侧重建（tool_use 前置 thinking block）；已发送 thinking 块仍收到 thinking-400 时**报错而不静默降级** | openai_compatible.rs |
| length 防护 | `finish_reason` 贯穿 ChatResult；`length` 停止时拒绝执行全部 tool call、回灌错误让模型重发 | agent/mod.rs 工具循环 |
| Usage 归一化 | DeepSeek 顶层 hit/miss、OpenAI prompt_tokens_details（含 cache_write）、Responses input_details、Anthropic cache_read/creation（C3 公式）、reasoning_tokens；`cache_reported` 粘性；`normalize_cache_fields` 收口；UsageAccumulator/usage.json 累计新字段；每请求 `prompt cache accounting` 日志行（绝对值） | llm/mod.rs, openai_compatible.rs, state/usage.rs |
| A2+B1 | 联想记忆从 `insert(1)` 改为 turn tail（replay_start 之后 → redo 冻结，决策 6 自然成立） | agent/mod.rs |
| A1 拆分 | `TurnProfile.turn_system_context` 新通道：QQ 发送者 JSON（含 message_id）与 WebUI artifact 清单移到 user 消息后的尾部 system 消息；静态规则留 system prompt | platforms/mod.rs, onebot.rs, web.rs, agent/mod.rs |
| 决策 4 | 好感度快照移出每轮注入（ensure_profile 副作用保留）；查询走已有 `query_qq_relationship` 工具 | real_context/mod.rs |
| A18 截断 | `private_reasoning_memory`（800/400）与 `private_tool_memory`（1600/400）幂等中段截断（+64 slack 保证幂等，只依赖 turn 自身内容） | agent/mod.rs |
| 1.3-b 部分 | `load_tools` 目录与 loaded 集合解耦（字节稳定）；目录条目改用**首行 ≤200 字符摘要**（摘掉嵌套的整份 skills 目录）；`definitions_except` 排序；vision_analyze 平台路径恒注册（空 scope 占位） | registry.rs, vision.rs, agent/mod.rs |
| C1 修复 | 指纹变更**不再删除**历史与 artifacts（改为记日志 + 更新指纹 = 计划内冷启动）；测试更新 | state/mod.rs |
| A14 | journal 附加改 BTreeMap 确定性分块 | conversation_db.rs |
| B8 | 累积器 name 兼容"整名重发"网关（重复忽略/前缀延伸替换/片段追加） | openai_compatible.rs |
| 存量修复 | token 向量测试的两个过期值（上游改 miyu.md/README 未更新） | token_estimate.rs |
| 缓存显示 | REPL footer 与逐轮回执显示 `turn(Ccached)` 绝对值；WebUI 轮次 meta 行显示 `· 缓存 X`；usage 快照接口（/api）携带累计 cache_read/write/reasoning；usage.json 同步累计 | render/mod.rs, cli.rs, web.rs, web/app.js, state/usage.rs |
| **stub 模式（§八点七 方案①，已实现并设为默认）** | `tools.loading_mode = "stub"`（新默认；hybrid/full 保留可选）：懒工具以真名+首行摘要+宽松参数壳常驻，**tools 数组会话内字节恒定**（有确定性单测锁定）；`load_tools` 语义变为契约获取——结果新增 `contracts` 字段（完整 description + JSON Schema，走 tool result 不碰前缀），对 already-available 名字也返回契约；参数直接按契约填在顶层（无嵌套解包）；权限/审批天然按真名判定；hybrid 的懒加载门在 stub 下自动旁路；token 会计同步走 stub 定义 | registry.rs `stub_definitions`/`tool_contracts`, load_tools.rs, config.rs, agent/mod.rs, config_tui.rs |

已知存量问题（非本次引入，已验证干净树复现）：`real_context::muted_bot_suppresses_direct_group_trigger_while_unknown_fails_open` 测试存在时段相关 flaky（凌晨运行 5/5 失败，白天全量套件曾通过）。

未实施（按 v7 门控，非遗漏）：
- **数据门控**（需先观测账单）：Anthropic cache_control 断点（决策 7 排期降级）、2.2 历史块前移、`context_start_seq` 指针、MCP 代理化、设计 a/P8、保留真实序列。
- **eval 门控**：miyu.md 瘦身（Release 3a 要求离线 eval 先行）、工具三件套 snippet/guideline 全量填充。
- **规模门控（后续批次）**：`llm_requests` 表/WirePrefixShape/mock e2e 门禁、model_context sidecar 迁移、render_turn 四合一、snip/prune 水位线、`last_request_at`+冷恢复剪枝、compact 迭代式摘要、cache_sticky 开关与亲和头透传、per-endpoint compat 开关、golden 基线测试、群聊 B2 会计。这些是下一批的第一优先级。

## 八点七、外部佐证与新增方案（2026-08-06，来源：linux.do《关于渐进式披露工具上下文的几种方向讨论》by 时歌）

该文独立推导出与本方案第三/四轮研究一致的**三角约束**：严格原生调用、稳定 Prompt Cache、真正按需加载——缺 Provider 配合时三选二（"保留独立原生调用→真实工具或 Stub 必须常驻；动态修改 tools→破坏早期缓存；缓存稳定+O(1) 初始→只能统一外壳、校验下放客户端"）。与 v4→v7 的裁定（tools 渲染在最前、追加也保不住历史、代理工具丢内层校验）互为印证。

**新增方案 ①（采纳，列为 Release 2c 的推荐演进 "1.3-c stub 模式"）**：全部懒工具以 **stub 常驻**——保留真名 + 一句话描述 + 宽松参数壳 `{arguments: object}`，常驻一个 `load_tool_schemas` 工具按需以 tool result 返回完整 schema（追加在对话尾部、不碰前缀）。文中估算 200 工具 80k→15k（-80%）。对 Miyu 的适配性极佳：
- 60 个懒工具 stub ≈ 60×70tok ≈ 4-5k，**tools 数组从此恒定**（C9 三条"减"路径整体无关化，A4 集合增长消失）；
- **权限按真名判定天然成立**（stub 名=真名，无需设计 a 的 unwrap 分发与提权防护）；call_id 配对与 finish_reason 原生保留；
- Miyu 执行层本就不做 schema 校验（tools 核查 §3 已确认 handler 自行取参），"内层校验下放客户端"对 Miyu 是零损失；
- 改造点：懒工具注册时生成 stub 定义；执行分发时解开 `arguments` 外壳（单点）；`load_tools` 语义改为返回完整 schema 文本；
- 代价：模型按 tool_result 中的 schema 填参；enum/pattern 从语法层降为文本层——与设计 a 相同类型的损失但范围小得多，可对高约束工具（task/qq_mention_users 等）保留完整 schema 常驻豁免。

**新增方案 ②（记录，Release 5 capability 项）**：Moonshot Kimi K3 的**消息级工具声明**——`role:system` 消息携带 `tools` 字段（不得带 content），从该位置起工具对模型可见、与顶层 tools 并存；官方缓存四原则 = "末尾追加不影响缓存；后续保留声明则前缀稳定；中间删改破坏其后；顶层声明不影响命中"（即"追加不要插入，注入了就别删"——与本方案核心不变量 2 逐字一致）。若配置 Kimi 端点，以 per-endpoint capability `message_level_tools` 实现真·渐进披露（pi 的 `deferredToolsMode: "kimi"` 即此）。OpenAI Responses 侧同构 tool_search + defer_loading 已在 P8 记录。

另一佐证：Claude Code 在 5.6 时代删掉 80% 系统提示词未观测到能力受损——支持 Release 3 瘦身方向（仍按 eval 门控执行）。

## 九、待实测核验清单

1. DeepSeek thinking_mode 现行规则（tool_calls 轮回传 reasoning_content 否则 400）——Release 0 fixture 直接覆盖。
2. opencode Zen（默认 provider）对 prompt_cache_key / x-session-id 的支持面（决定 2b 收益上限）。
3. DeepSeek 缓存 TTL 从 usage 时间序列估计（不写死）。
4. 各 MCP server schema 稳定性（决定 Release 5 是否需要 pinned/代理）。
5. OpenAI capability 表落地时按现行官方文档配置（prompt_cache_key/explicit/cache_write_tokens 已官方化，实现时逐项过一遍）。
