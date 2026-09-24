# 子系统挂接表(as-built,2026-09-16)

四个 normal 子系统由 `PersonaManifest.subsystems` 声明意愿,`config::subsystems`
(`SUBSYSTEMS` 表 + `EnabledSubsystems::resolve`)把「人格意愿 × 机器配置」折成一份快照;
所有挂接点只看快照。Agent 构造时取一次(`reload_config` 重取),组工具面时取一次,
平台插件按回合取。中途改 persona.toml 只影响之后新建的 Agent(daemon 每回合新建)。

> 09-24:技能从子系统并入**插件**(与 scripts / MCP 同级的 `PluginKind::Provider`)。
> 人格意愿看 `plugins.enabled` 里的 `"skills"`,机器位仍是 `skills.enabled`,两者相乘是
> `PersonaManifest::skills_enabled`;逐件白名单仍是 `plugins.skills`。注册仍在
> `tools/compose_providers.rs` 的末尾(子代理快照之后、脚本与 MCP 之后),位置没动,
> `request_shape_probe` 五张脸的提示词/消息/工具逐字节不变。老 persona.toml 的
> `subsystems.skills` 读入时折进插件名单(`PersonaManifestWire::into_manifest`):关着 → 名单里
> 去掉技能;开着且名单是明细 → 补上技能;写出去不再带这个字段。

## 启用判定

| 子系统 | 人格意愿 | 机器配置 | 备注 |
|---|---|---|---|
| memory | `subsystems.memory` | `memory_config().enabled` | dev 走 `dev_scoped()` 把机器位也关掉 |
| persona_reminder | `subsystems.persona_reminder` | `prompt.persona_reminder` | dev 无人格恒不提醒;间隔 `prompt.persona_reminder_interval` 是「怎么提醒」 |
| voice | `subsystems.voice` | `voice.enabled \|\| voice.tts.is_active()` | 两件工具各自再看自己那一位 |
| emotion | `subsystems.emotion` | real_context 插件设置 `affection_enable` / `emotion_enable` | 机器位不在 `AppConfig` 类型化字段上,插件把两者相乘(`persona_overlay`) |

`PersonaManifest::core_only()`(dev)折出来的快照恒为空(`is_empty()`)。

## 挂接点

阶段名对应 `SubsystemPhase`。同一阶段内的顺序即回合内的先后。

| 子系统 | 阶段 | 位置 | 读/写 | 失败策略 |
|---|---|---|---|---|
| memory | ToolRegistration | `tools/compose_core.rs` `memory::register` | 写 registry | 无 |
| memory | ToolRegistration(平台) | `tools/mod.rs::rescope_platform_memory_tools` | 换成带主体作用域的三件 | 无 |
| memory | SystemPrompt | `agent/setup.rs::assemble_system_prompt` → `with_memory_preamble` | 返回新 String | 无 |
| memory | BeforeModel | `agent/turn_loop/stream.rs` 联想记忆块(`MemoryStore` 再看 `association_enabled`) | `messages.push` | 失败记 warn、不注入 |
| memory | AfterTurn | `stream.rs` / `redo.rs` 日记(`MemoryStore` 再看 `auto_diary_enabled`);`history.rs` 逐出库归档 | 写记忆库 | 记 warn |
| persona_reminder | BeforeModel | `turn_loop/parallel.rs::resolve_persona_reminder` → `persona_hint::resolve`;`history.rs` 按间隔注入 `<persona-reminder>` | `messages.push`,随尾巴化石化 | 蒸馏失败降级为无提醒 |
| voice | ToolRegistration | `tools/compose_core.rs` `voice_chat::register`(`voice.enabled`)、`voice_speak::register`(`tts.is_active()`) | 写 registry | 无 |
| voice | SystemPrompt | `agent/prompt.rs::with_host_environment` 属主分支末尾的 `<voice-protocol>`(快照关着整段不发,09-16) | 返回新 String | 无 |
| voice | 语音回合 | `web/voice_bridge.rs` 把命令包成 `<voice_input>`;`web/voice_tts.rs` 回合后朗读 `<speak>` | 改本地输入 | 播报失败记 warn |
| emotion | PlatformHooks | `platforms/plugins/real_context`:`register_tools`(`query_qq_relationship`)、`inject_context`(判官阈值修正、回合尾 `<internal-state>` 一行)、`after_send`(好感度/情绪更新) | 写 `turn_system_context`;写状态库 | 快照失败记 warn 当中性;更新失败记 warn |

## 已知不一致(未改,记在这里)

- (已解决 09-16)`VOICE_PROTOCOL` 现在看 `subsystems.voice`(人格清单 × 机器 `voice.enabled || tts` 激活):
  关着的机器/人格不再带那 120 字;开关翻转是一次计划内冷启动。量尺 `request_shape_probe` 的
  `normal-owner-voice` 脸与 09-16 之前的 owner 字节逐字相同,`normal-owner`(语音关)只少那一段。
- 技能(09-24 起是插件):`apply_skill_refresh` 以 `config.skills.enabled` + 注册表里有没有
  `load_skill` 为判据,没有直接看插件位;`load_skill` 只在 `skills_enabled` 为真时注册,结果等价。
- (已解决 09-16)`persona_reminder` / `emotion` 两个开关进了 `feature_catalog`:引导页与成员人格页能勾,
  机器侧没开(`prompt.persona_reminder` 关、QQ 没开)就不摆。

## 验收

```
cargo test --lib -- config::subsystems agent::tests::subsystems tools::compose_tests real_context::tests::subsystem_gate
```
