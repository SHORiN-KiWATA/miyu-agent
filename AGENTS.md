你是顶级RUST工程师，顶级 AI loop、AI Agent、AI Harness 设计师。

docs/中有过去所有的计划和文档，可以自行按需阅读。

必须重视代码质量，注重模块化、接口化、可复用、可扩展、避免耦合等，永远不要让单个代码文件体积膨胀成为“上帝文件”。

每次输出自检并告知总体进度百分比，告知把握程度百分比。

灵活使用子代理节省上下文的同时提高任务执行速度，但不要滥用并行导致额度的不必要消耗。

不确定的点必须询问用户，而不是自己决定，询问是告知你的推荐项。

编写时应当同时考虑Linux和MacOS上的兼容性，确保能在两类系统上运行（MacOS 仅考虑M系列芯片的）。

先定位问题，找到根因，提出方案，标记推荐选项，让用户做开工前的拍板。

编写完成后应当给出清晰的可照做的验收流程。

用户验收无误后新内容才可以commit，验收成功准备发布的内容在commit时应当记录在next release note中作为下次release note的草稿。

---

# 项目注意事项

深挖去处：`docs/理念.md`（设计哲学正典）、`docs/compact-plan.md`（compact 唯一定论）、`docs/cache-and-prompt-plan.md`（缓存契约）、`docs/wiki/15-扩展指南.md`（新工具/迁移步骤）、`docs/fixed/`（历次排查案卷）、`docs/plan/`（已完成专项的结果记录）。

## 1. 提示词与缓存（字节契约）

1.1 **前缀即契约**：相同会话状态必须产生逐字节相同的请求前缀。序列化路径唯一；非确定内容（时间/随机数/探针值）要么不进前缀、要么进入即冻结；工具描述是常量，禁止拼时间戳/路径。tools 数组一处分叉，排在它前面的 system 缓存也一并作废。
1.2 **append-only 与化石化**：追加不插入；发送过的瞬态内容（runtime/联想记忆/元数据）落库后逐字节回放、永不删除。压缩是唯一例外且单调（水位只前进）。瞬时尾巴放当前用户消息**之后**。
1.3 **tools 数组会话内字节恒定**：默认 full 模式（09-01 定稿：约 60 件工具的量级下 stub 省下的 token 很薄，约束解码型模型又吃不下空壳 stub）；stub 按模型可选（`provider.model_tools_loading_mode`），懒工具以真名+首行摘要（≤60 字符，即描述第一行）+宽松参数壳常驻，完整契约走 load_tools 结果。两种模式都不许让工具描述在会话中途变：会变的内容不拼进描述，走回合尾巴的指令源（`agent/instruction_source.rs`：跟请求里最近一份比、变了才发；技能目录 09-25 起在 `<available-skills>` 块里）；到了轮数上限也照带同一份 tools、只设 `tool_choice: none`（B5）。不要为了让模型看到参数把工具设 always_loaded。
1.4 **指令型注入必须放 system 侧**（每请求新组装、不化石）——内联在消息块里的指令会随化石重放造成跨轮错乱（qq-reply-target 的前身就是这么死的）。所有注入带 XML 标签外壳，不裸奔。
1.5 **文风规范**：模型可见的机械文本（描述/schema/注入/报错回灌）一律英文短句——同语言同语域污染最毒，中文书面语注入是头号 OOC 源；中文只留给人格侧文本。禁分号串联、禁「总述：分述」结构。每条注入先问“删掉会怎样”再问“怎么写短”。人格 hint、goal/compact 模板是实测敏感区，不动。
1.5.1 **同时给模型和给人看的文本必须双槽**（09-21）：模型槽恒英文、进 token 预算；人槽跟界面语言、不进模型面。已落地的三对——脚本 `# Description:`/`# 描述：`（`select_script_description` vs `select_script_ui_description`），脚本 `# Display name:`/`# 显示名称：`，技能 frontmatter `description`（模型）/`display_name`+`summary`（人）。**一个槽两拨人用**就是设置页中英混杂的根（19 个内置脚本曾零中文槽、技能曾无显示名）。技能 `description` 允许中文**触发词**（用户真会说的那几个词），但句子主体必须英文——`test_scripts/check-model-english.sh` 按「首字符 ASCII 字母 + CJK 占比不过半」判，它同时扫 `descriptions/*.json` 与 `src/personas/*/skills/**/SKILL.md`。

1.6 描述/schema/hint 的改动=一次性计划内冷启动，可接受；改后必须仍是常量字节。改消息组装/工具目录后除跑测试外，手测两轮请求的 cache-usage jsonl 确认第二轮 cache_read 不异常下降。
1.7 辅助请求（compact/judge/title/subagent/vision 等）独立 cache/session 状态；唯一例外是 fork 式摘要（刻意复用主对话前缀）。
1.8 token 量尺：`cargo test --lib token_diet_baseline -- --ignored --nocapture`。

## 2. 工具系统

2.1 **描述/schema 真相源是 `src/tools/descriptions/*.json`**（经 `crates/miyu-engine/src/tools/tool_descriptions.rs` 的 include_str! 宏，JSON 等资源留在仓库 `src/` 不随 rs 进 crate；新增必须补宏行，忘了=JSON 静默失效）。Rust 里的描述只是占位，注册时被 JSON 整体覆盖（load_skill 09-25 起也不例外）。权限只由 `.writes()`/`.presentation()` 决定，JSON 的 permission 字段是死字段。
2.1.1 **工具契约是公共预算**（09-21 瘦身）：单件 ≤550 tok、仓库自带整面 ≤10500 tok，`bundled_tool_face_stays_within_its_token_budget` 当场拦。写法三条：①工具描述 ≤3 句（是什么/什么时候用/一条最容易踩的坑），教程与输出格式长文不写；②**「调用之后才用得上」的知识写进工具自己的输出，不写进 schema**（字段语义、退化标志怎么读、分页边界——模型拿到结果时才需要）；③参数说明 ≤1 句，名字加 enum 已说清的（`format: md|json`）干脆不写，默认值统一 `"Default 20."`。合并域省不了多少：单工具信封只有 27 tok，参数一个都不会少。重量级低频的脚本住进 `skills/<技能名>/scripts/`：不进 tools 数组（照常注册、可经工具桥调用），由技能正文带路。判据是「住在哪」，不是脚本头部的一行声明。

2.1.2 **内置技能住资源树、读盘加载**（09-23）：`<前缀>/personas/<人格>/skills/<技能名>/SKILL.md`，与脚本共用 `<前缀>/personas/` 布局（`miyu_core::skills::skill_roots`，源码树里即 `src/personas/`）。摆错位置、目录名与 frontmatter `name` 对不上，这份技能就静默加载不到（travel-planner 曾这么躺了一天）。跨人格可用的只有 `PLATFORM_WIDE_SKILLS`（skill-creator / script-creator），其余内置技能属出厂人格。`bundled_skills_follow_the_persona_resource_layout` 当场拦。

2.2 **工具名是最强的能力广告：域内聚合、域间分名**。编辑/读取类能力并入 edit（文件系统）/kb/artifact（补丁语义）与 read（`kb:`/`artifact:` 前缀），别为新存储开新读写工具。把能力藏进描述里的前缀/参数，模型想不起来（kb: 前缀实测翻车史）。
2.3 **输出格式改造必须双兼容**：旧回合 tool_flow 逐字节回放，旧 JSON 解析器永远保留。“结构即功能”的不改：成败判定只认输出 JSON 的 success/ok 布尔（非 JSON=默认成功），错误路径保留 ok:false JSON。
2.4 畸形参数在 registry 统一收口（按 schema 还原字符串化的数组/对象/数字），声明为 string 的参数一个字节不碰；报错说自己真正知道的（“期望整数，收到字符串 "1"”）。
2.5 subagent 子代理是全新上下文、不继承主对话——定论，prompt 必须自包含。`dev=true` 的子代理走开发模式三件套（dev 人格作用域 + core_only 工具面 + 中转线 dev 工具作用域）。平台限额（生图张数等）由代码承担，不写进 prompt 求自觉。

## 3. 数据库与状态

3.1 迁移只在 MIGRATIONS 末尾追加、纯增量（不回填不删列）。**只加表、加索引的改动走 `state/migrations/named.rs` 的命名迁移**（09-24 起）：它不动 user_version，老版本程序照样能开库，分支之间也不撞号；只有改老列、重建表才走版本号迁移。改 Turn 字段必须同步所有固定列序 SELECT 与 map_turn_row（全库最脆弱处）；界面用的按页查询（`conversation_db/pages.rs`）也要同步检查。
3.2 追加型数据用自增子表，别塞 turns 的 JSON 数组列（读改写全量=O(N²) 写放大）。
3.3 **DB 备份用 VACUUM INTO，禁止 fs::copy 活库**（打开再 close 会丢本进程的 POSIX 常驻锁——08-21 conversation.db 损坏根因）；db/-wal/-shm 三件同进退；手工查活库一律 `mode=ro&immutable=1` 或拷副本；quick_check 不过就别跑 vacuum。

3.4 **会话级状态进会话库，别再在 `state/` 下按会话 id 开文件**（09-24 会话项目第 1 段）：
- 待办、提示词指纹、思考档位钉存 `session_values`，终端上键历史存 `repl_history`，都挂 sessions 表级联删除；
- 以前按会话 id 命名的文件，删会话时没人清（实测 todos 11/11、repl-history 127/129 是孤儿）；
- 删会话走 `StateStore::delete_session`，库外的 spill、compact、老文件它会一起清。
3.5 **开库**：
- 客户端（终端、一次性命令、守护进程里临时开库的工具）用 `StateStore::new`：进程内共用一个连接，库是当前版本就不跑体检。只有守护进程开主库、开成员库时用 `open_maintained` / `open_member`，体检和回收空闲页只在那里做。
- 用量账本是机器级的独立库 `state/usage.db`（`usage::ledger`），不进会话库。

## 4. 平台 / QQ

4.1 trusted（principal/admin，宿主产生）与 untrusted（昵称/正文，用户可控）字段分离承载；不可信文本进提示词必须过 safe_prompt_field（防伪造记录行）。
4.2 插件 hook：system_context 必须字节稳定，动态内容走 turn_system_context；memory_content 禁改。
4.3 投递幂等闸：图片按内容 digest，文字按归一化 bigram 近似度（回合内）——端点故障日模型会把“发送”重演成同义变体，两个闸都不能拆。
4.4 出站图解码 256MB 上限同时是可发送图片的尺寸包络，不能为省内存下调。定时消息先记账再发送，错过时点跳过不补发。

## 5. 测试与排查

5.1 **先证明不修时现象会出现，再修**；新回归用例退回修复前必须报红，否则守不住任何东西。
5.2 量尺类测试标 #[ignore]；断言结果不断言耗时；性能对比看倍率不看绝对值。
5.3 测试不受开发环境影响：终端探测（TERM/kitty）在 cfg!(test) 下走固定路径；PTY 测试等子进程真就位再断言。
5.4 黑盒实测必须 MIYU_HOME 沙箱（普通 CLI 未知子命令会把参数当对话发给生产 daemon）。“改动没生效”先查幽灵 daemon 与测试 home 的配置残值。普通单次 CLI 阅后即焚会杀后台任务，测唤醒用 shellhook 形态。
5.5 仓库自 08-26 起 fmt-clean（`939a2feb` 全量格式化，字节基线验证提示词未变），改完直接 `cargo fmt` 即可，别再手工挑文件——遗留的「rustfmt 会顺着 mod 声明递归刷子模块」陷阱随之失效。涉及 agent/llm/registry/提示词的改动，`test_scripts/refactor-check.sh` 五道门禁 + `test_scripts/arch_dep_check.py` 层序门禁是验收硬要求；改提示词组装/工具面还要跑 `request_shape_probe`（§8.5）证明字节零变化。
5.6 报错信息是嫌疑人不是证词：先读规范/原始数据（curl 探针、协议原文、日志），最后才轮到推理。

## 6. 性能与重构

6.1 没有实测数字不合并；“实测后判不做”清单见 `docs/plan/low-footprint.md` 与 `docs/fixed/2026-08-18-性能优化.md`（mimalloc、AppConfig→Arc 快照、资源外置、panic=abort 等），别重提。
6.2 文件规模：目标 800 行 / 上限 1500 / 红线 2000。codegen-units=1 已定（release 编译 ~5.5 分钟属预期）；它与「单 crate」无关——09-16 起是 workspace（§8.2），体积靠 codegen-units=1 + LTO，不靠 crate 数。
6.3 搬文件五坑（include_str 相对路径漂移/模块名遮蔽/super 语义改变/脚本必须拒绝覆盖已存在文件/回退前先看暂存区）：`docs/fixed/2026-08-18-代码拆分.md` §五。

## 7. 构建与发布

7.1 `src/prompts/*`、web/ 静态资源、assets 词表全部编译进二进制——改完必须重新构建，daemon 按 MIYU_BUILD_ID 判断重启。
7.2 发版链照 v0.4.5 流程：release commit → tag → 资产必须含 fonts/（`tar -tf` 验 + 包内二进制自报版本）→ AUR 包装包 → 本地 pacman 轮换。仓库 packaging/ 三份 PKGBUILD 是真相源，别让它与 AUR 克隆脱节。

## 8. 分层、边界与重构（2026-09-16 定论）

8.1 **层序的真相源是 `test_scripts/arch_dep_check.py` 的 `TIERS`**（基础 → 配置 → 存储与协议 → 子系统 → 传输 → 工具与引擎 → 场所与展示 → 入口）：同层互引随意，低层引高层就是违规，新顶层模块必须先登记；记忆 / 技能 / 人格提醒三个子系统互不 use。白名单 `arch-dep-waivers.json` 只减不增，每条 reason 就是待办。「门禁绿」只证明没新增违规，不证明无环——层序表才是无环的依据。
8.2 **crate 划分**：根包 `miyu`（入口层 + 两个 bin）留原地，`crates/{miyu-base,miyu-core,miyu-engine,miyu-hosts}` 各收一到三层；打包脚本与 `target/release/miyu` 不变。放置规则：只有 `.rs` 归 crate，`src/prompts/*.md`、`src/memes`、`src/personas/**`（出厂脚本与技能，09-23 起 `src/scripts/` 并入）、`assets/`、`web/` 留原处（`include_str!` 用相对路径，`paths::resources` 的开发态根读 `MIYU_WORKSPACE_ROOT`）；`build.rs` 两份各司其职：根包 `build.rs` 只算构建 id（唯一对整棵源码树 `rerun-if-changed` 的脚本，`miyu::run()` 用 `miyu_base::install_build_id` 装进去，下层运行时读 `miyu_base::build_id()`，**不许在下层编译期嵌构建 id**，否则改上层一行都从 base 起全量重编）；`crates/miyu-base/build.rs` 只烘焙资源（提示词 / o200k / `JIEBA_INDEX`）且只对资源文件 rerun。`concat!(env!)` 跨不了 crate。可见性只放宽编译器点名的（E0603/E0624/E0364/E0451），不批量 `pub(crate)` → `pub`；trait 方法不能带 `pub`。跨 crate 的测试夹具与测试态行为：`#[cfg(test)]` 只在**本 crate 自己的**测试里为真，上游 crate 的测试看到的是生产行为。所以下层 crate 里所有测试态开关（`miyu_executable` 的防 fork 炸弹闸、终端探测固定路径、进程内 embedding/渲染、块标记线程局部、重试延迟）一律写 `cfg(any(test, feature = "testkit"))`，夹具住 `test_support.rs`（模块私有，父模块 `pub use test_support::*`），上游在 `[dev-dependencies]` 里打开 `testkit`；测试找仓库资源用 `miyu_base::WORKSPACE_ROOT`，不用 `CARGO_MANIFEST_DIR`（那是 `crates/<crate>`）。
8.3 **下层要上层的能力一律走端口**：下层定义窄 trait，拥有能力的层在 daemon 启动时装入（`host_ports::{VoicePort, QqOutreachPort}`、`platform_types::PlatformToolContext`、`agent::PlatformTurn`）；非 daemon 进程取到 `None` 用原错误文案兜底。禁止反向 `use`；要信息走查询端口（`miyu host` / `HostQuery`），要通知走事件。
8.4 **测试夹具住各模块 `test_support.rs`**（`#[cfg(test)] mod test_support;` + `use test_support::*;` 再导出），生产文件里不写 `#[cfg(test)] fn`。「只有测试在用」≠ 死代码：裁决法是整体删掉跑测试构建，按编译错误的封闭函数看——薄包装把测试调用点改成直接调生产函数，独立死逻辑才连专用测试一起删，摆状态用的夹具留下。死代码清理两阶段：先全部标 `#[cfg(test)]` 保编译，再在测试构建下查；删之前列清单等拍板（`docs/plan/2026-09-16-dead-code-inventory.md` 是样板）。
8.5 **重构验收三件套**：`request_shape_probe`（`MIYU_REQUEST_SHAPE_OUT=/tmp/x.json cargo test --lib request_shape_probe -- --ignored`，五张脸逐字节 diff，该变的只变那一段）+ `refactor-check.sh` 五道 + 层序门禁；碰 daemon / 扩展的再跑端到端探针（`testkit/host-query/run.py` 9 项、`testkit/oobe/mcp_probe.py` 7 项）。量尺：本机没有 `/usr/bin/time`，峰值内存读 cgroup `memory.peak`；跑整套测试与编译一律 `systemd-run --user --scope -p MemoryMax=20G -p MemorySwapMax=0`，同一时刻只跑一个 cargo。已知并行抖动（onebot notices 已加串行锁 / render::math 字体预热 / claude_code haiku）复跑一次，第二次仍红才算问题。
8.6 **机械段可以委派便宜模型**：施工单必须写死硬约束（绝对路径、cgroup、一次一个 cargo、不 commit/stash/`cargo fix`）、逐步命令、验收三件套、「锚点对不上就停下报告」；判断题（归属、trait 形状、删还是搬）自己做；派出去后别碰会被编译的文件。检查点用 `git diff HEAD`（`git mv` 会暂存改名）。
8.7 **下结论前的三条自查**：grep 别管 `head`（截断过后半段导致「生产从不读」误报）、跨行字段访问按字段名搜；worktree 里不 `cd`（会把后续命令的工作目录重置到主检出）；抓帧先确认目标在视口里。其余坑见 `docs/wiki/18-重构经验.md`。
8.8 **一棵 worktree 只能一个会话动手**：要接手别人的树先 `ListAgents`/`SendMessage` 说清边界，`git add` 别用 `-A`（会把对方在途半成品扫进提交），跑验收用 `--all-targets`（只 `cargo build` 看不见测试里的路径错误）；场所与引擎之间只传 `PersonaLane`，`normal|dev` 是线上的词，代码里不许再造第二套模式枚举。
