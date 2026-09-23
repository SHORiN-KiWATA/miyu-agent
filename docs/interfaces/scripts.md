# scripts(as-built)

完整作者契约在 [`docs/scripts/README.md`](../scripts/README.md),模型侧同一份在内置技能
`src/personas/default/skills/script-creator/SKILL.md`。本页只登记契约要素与版本规则,不复制正文。

## 要素

| 项 | 现状 |
|---|---|
| 资源形态 | 一个可执行文件 = 一个工具;契约写在紧跟 shebang 的注释头部(`# 键: 值`,`//` 也认) |
| 头部键 | `Id` `Description`(英文,首句 ≤60 字符)`显示名称`(中文,必填)`Display name` `Parameters` `Timeout`(默认 120,上限 300)`Group` `Argv`(`none`/`flags`)`Trust`(`owner`/`external`)`Permission`(`writes` 默认 / `read-only` / `presentation`)`Example` `Hint` `Requires` `Capabilities`(宿主查询能力,见 host-capabilities.md);别名见 `crates/miyu-engine/src/tools/scripts/header.rs::header_key` |
| 未知键 | 跳过——新增头部键对旧版本天然向前兼容 |
| 目录层 | 内置(`/usr/share/miyu/personas/default/scripts/`,只默认人格可见;老位置 `…/scripts/personas/default/` 仍扫)→ 全局(`~/.miyu/data/scripts/`)→ 人格(`…/personas/<人格>/`);同 id 后者覆盖;每层可有 `index.json` 覆盖层与 `disabled` 名单 |
| 传参 | stdin 一个 JSON 对象;≤64KB 时同份放 `MIYU_ARGS_JSON`;`Argv: flags` 时展开 `--key=value` |
| 输出 | stdout 给模型;退出码 0 成功,失败非零并输出 `{"ok":false,"error","fix"}`;单流 8MiB 硬截断、20000 字符软截断 |
| 附件 | `MIYU-IMAGE: <路径> \| <说明>` 整行摘掉交投递层 |
| 环境 | `MIYU_SCRIPT_CACHE_DIR` 指向缓存目录;声明了 `Capabilities` 且在 daemon 里跑时另有 `MIYU_HOST_TOKEN` / `MIYU_HOST_CAPABILITIES` / `MIYU_HOST_BIN`(`miyu host <method>` 查宿主) |
| 权限 | `Permission` 决定 read-only 闸;`Trust` 决定不可信场所(QQ 群、远端成员)可见性——外部场所只注册 `Trust: external` 的脚本(`scripts::register_external`),且不给 `manage_script` |
| 人格闸 | `PersonaManifest.plugins.scripts` 开关 + 白名单在扫描层裁决(`retain_persona_visible`) |
| 刷新 | 每个工具回合按目录指纹重扫(`prepare_script_refresh`),只在变了才替换工具;不用重启 |
| 注册工具 | `manage_script` register / unregister / list;注册前校验 shebang、描述、`type: object` |

## 版本

头部没有 version 字段。兼容规则:只加键不删键;改默认值(如 `Permission` 缺省)算破坏性,
须在 [compatibility.md](compatibility.md) 登记并给迁移说明。

## 验收

`cargo test --lib tools::scripts`;黑盒见 `testkit/`(脚本相关子目录)。
