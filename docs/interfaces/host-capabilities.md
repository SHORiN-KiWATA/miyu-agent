# 宿主能力端口(as-built)

下层要用上层才有的能力时,不反向 `use`,而是调窄 trait;实现由拥有能力的层装入。
原则:只暴露调用方真正用到的动作,不传 `AppConfig` 整本、不传 `DaemonState`、不传密钥、
不造"万能 HostContext"。

## 端口清单

| 端口 | 定义 | 实现 / 装入点 | 调用方 |
|---|---|---|---|
| `VoicePort` | `host_ports/ports.rs` | `web::voice_bridge::VoiceBridgePort`,`web::server` 启动时 `install_voice_port` | `tools::voice_speak` / `voice_chat` / `platform_outreach`,`platforms::tool`(send_voice_message),`platforms::onebot::voice_inbound` |
| `QqOutreachPort` | `host_ports/ports.rs` | `platforms::onebot::proactive::OutreachPort`,`web::server` 启动时 `install_outreach_port`;`send_to` 任意好友/群、`directory` 地址簿(缓存 5 分钟) | `tools::platform_outreach`(send_qq_message / qq_contacts,注册条件 `qq_connected`;普通模式任意好友/群,开发模式只发管理员且无地址簿——终端由 `build_tool_registry` 按车道收窄,平台由 `platforms::tool::register_outreach` 按车道装给所有触发者) |
| live-turn 宿主工具位 | `host_ports/live_turn.rs` | `platforms::live_turns::LiveTurnGuard` 随平台回合登记/注销 | `llm::…::cli_relay::host_tools_face`(中转线桥的工具面) |
| 终端正文视口 | `terminal::{set_content_viewport, content_viewport}` | 全屏 `Screen` 进入/重绘时设,drop 时清 | `render::markdown`(公式)、`render::content_cols`、`terminal::kitty`、`tools::memes` / `vision::print` |
| 供应商目录 | `provider_catalog` | 领域模块自身 | WebUI / TUI / OOBE |
| `PlatformToolContext` | `platform_types.rs` | `platforms::tool_context` 为 `PlatformTurnContext` 实现 | 平台作用域工具(vision、image_generation 参考图) |

非 daemon 进程(REPL 直连、`miyu run`、测试)里端口为 `None`;调用方沿用原错误文案
("speak 只能在 daemon 里用…"、"send_qq_message only works inside the daemon")。

## 端口的生命周期

- 装入:daemon 启动一次,覆盖语义;测试可装假实现(`ports.rs` 单测示范)。
- 无取消/超时语义:端口方法各自沿用被包装函数的预算(语音前端 20s 等待、转写 60s)。
- 观察字段:与被包装函数相同,不新增日志。

## 新增一个端口

1. 先确认真的是"下层要上层的能力",而不是应该下沉的类型;
2. 在 `host_ports/ports.rs` 加 trait(方法 ≤ 6 个、参数是 DTO/基本类型)、`install_*` / `*_port()`;
3. 拥有能力的层实现并在 `web::server` 装入;
4. 调用方替换 `use crate::web` / `crate::platforms`;
5. `python3 test_scripts/arch_dep_check.py` 必须仍为"无"。

## 进程外扩展的只读查询(as-built,09-16)

脚本这类进程外扩展不拿 WebUI 管理员 token,也不自报身份:宿主在拉起它时签一张
**一次性令牌**(`host_ports::host_grants`,只在 daemon 里签发,守卫掉落即作废),
写进环境变量 `MIYU_HOST_TOKEN` / `MIYU_HOST_CAPABILITIES` / `MIYU_HOST_BIN`;
扩展跑 `$MIYU_HOST_BIN host <method> [json]`,CLI 经 IPC `HostQuery` 把令牌与方法交给
daemon,daemon 按令牌找授权集(`host_ports::host_query::answer_host_query`)。

| 方法 | 需要的能力 | 返回 |
|---|---|---|
| `host.info` | `host.info` | 版本、`config::SUPPORTED_CONTRACTS`、本次授权集、当前人格 |
| `providers.list` | `providers.read` | 供应商摘要列表(id、显示名、协议、enabled、内置 CLI 位、default_model、models、active)+ 当前激活选择 |
| `providers.get` | `providers.read` | 一家的摘要 + `context_windows`、`modalities` |
| `subsystems.enabled` | `subsystems.read` | 当前人格的五个子系统开关 |

- 能力词表:`host_ports::HOST_CAPABILITIES`(与 PM 的 `requires-capabilities`、脚本头部 `Capabilities:` 同一张表)。
- 授权来源:脚本头部 `Capabilities:` ∩ 词表,且只给 `Trust: owner` 的脚本(`Trust: external` 的脚本也会在不可信场所跑,不给);
  MCP 服务器按 `mcp.servers[].capabilities` ∩ 词表,令牌随服务器进程生灭(`tools::mcp::connection::McpConnection` 持守卫;09-25 起 daemon 里的进程按会话常驻,令牌活到进程被回收)。
- 脱敏:摘要里没有 api_key、base_url、超时、额外请求体;返回的是 DTO,不是配置对象,也不会返回任何能反向拿到运行时的句柄。
- 错误码:`permission_denied`(令牌不认识/作废/能力不在集里)、`unknown_method`、`invalid_argument`、`not_found`、`unavailable`(daemon 不在)。
- 作用域:以 daemon 当前全局配置为准(与脚本所在回合同一份);成员私有人格的按人视图未做。
- 非 daemon 进程(REPL 直连、`miyu run`)不签令牌,脚本看不到变量,按「宿主不可用」处理。

加方法:先在本表与 `docs/scripts/README.md` 登记方法、能力、返回字段,再改 `host_query.rs` 与它的测试。

验收:`cargo test --lib -- host_ports::host_grants host_ports::host_query web::tests::ipc_bridge::host_query`
(单测 + IPC 往返);真链路 `cargo build && python3 testkit/host-query/run.py`(隔离 daemon + 桩模型,
不碰线上 8300)。

## planned

- 成员私有人格的按人作用域(查询只回本人授权的选择);
- 写类能力(配置写、发送)——复用权限专项的闸,不做只读模式旁路。
