//! 内置插件登记表(09-16,core/normal 接口治理 Phase 4)。
//!
//! 以前新增或移除一个内置插件要在四处登记:`PLUGIN_IDS`、`feature_catalog` 的
//! `TOGGLE_PLUGINS` 与 `plugin_label`、`tools::compose_core` 里的那个 `if`——漏一处
//! 编译照过,人格清单校验放行,工具面上却没有它。现在一行写完 id / 种类 / 中文名 /
//! 提示 / 可勾选 / 机器开关:[`PLUGIN_IDS`]、[`TOGGLE_PLUGINS`]、[`plugin_label`] 全部
//! 从这张表派生。
//!
//! 注册函数住在 `tools::builtin_plugins`(config 是底座,不能反向依赖 tools),两张表
//! 按 id 对齐,`tools` 侧的测试钉住「每个 Builtin 都有注册函数、没有多余的注册函数」。
//! 新增一个内置插件 = 实现文件 + 描述 JSON + `include_str!` 一行 + 这里一行 + 那边一行。

use super::AppConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    /// core 件的开关(`files`):关掉只留 `run_command`,由 compose 显式处理。
    Core,
    /// 内置 Rust 插件:往工具面加东西,按 `tools::builtin_plugins` 的注册表挂。
    Builtin,
    /// 外装件的接入机制(scripts / mcp):本身按各自规范再筛,由 compose 显式处理。
    Provider,
}

/// 机器级开关落在配置的哪个字段上：读一份、写一份。
///
/// 写的那半是 2026-09-20 加的：设置界面的功能表勾上一项时，人格白名单与机器
/// 开关**一起**打开（用户拍板的口径），不然勾了不生效，人要在两个菜单之间
/// 来回找。取消勾选只动人格那层，机器上的配置（密钥、尺寸）留着。
pub struct MachineSwitch {
    pub get: fn(&AppConfig) -> bool,
    pub set: fn(&mut AppConfig, bool),
}

pub struct BuiltinPluginDescriptor {
    /// persona.toml 里的名字,也是 `manifest.plugin_enabled(id)` 的键。
    pub id: &'static str,
    pub kind: PluginKind,
    pub name_zh: &'static str,
    pub hint_zh: &'static str,
    /// 英文界面用这一份。原来只有中文，于是英文 locale 下这一栏留中文、
    /// 而隔壁脚本那一栏按 locale 变英文，同一页混两种语言（用户 09-23 截图）。
    pub name_en: &'static str,
    pub hint_en: &'static str,
    /// 引导 / 成员人格页给不给开关;常开件不摆出来。
    pub toggleable: bool,
    /// 机器级开关:本机装了 / 开了没有。人格只能在装了的里挑;运行态条件
    /// (如 QQ 是否连着)在注册函数里再判。
    pub installed: fn(&AppConfig) -> bool,
    /// 机器级开关能不能**写**。组合条件的（外发要「终端外发开着且 QQ 连着」）
    /// 没有单一开关，给 `None`。
    pub switch: Option<MachineSwitch>,
    /// 有没有「怎么配」的设置页（密钥、尺寸、账号）。功能表上这一行摆个齿轮，
    /// 回车进去。设置页的实现在 `config_tui`，这里只记有没有。
    pub settings: bool,
}

fn always(_: &AppConfig) -> bool {
    true
}
fn exchange_rate_installed(config: &AppConfig) -> bool {
    config.plugins.exchange_rate.enabled
}
fn archlinux_installed(config: &AppConfig) -> bool {
    config.plugins.archlinux.enabled
}
fn memes_installed(config: &AppConfig) -> bool {
    config.plugins.memes.enabled
}
/// 连接状态不在这里:那是运行态,`tools::platform_outreach::qq_connected` 现判。
fn platform_outreach_installed(config: &AppConfig) -> bool {
    config.platforms.terminal_outreach && config.platforms.qq.enabled
}
fn web_images_installed(config: &AppConfig) -> bool {
    config.plugins.web_images.enabled
}
fn image_generation_installed(config: &AppConfig) -> bool {
    config.plugins.image_generation.enabled
}
fn knowledge_base_installed(config: &AppConfig) -> bool {
    config.plugins.knowledge_base.enabled
}
fn mcp_installed(config: &AppConfig) -> bool {
    config.mcp.enabled
}

/// 顺序即 [`PLUGIN_IDS`] 的顺序(校验报错时照这个列)。
pub const BUILTIN_PLUGINS: &[BuiltinPluginDescriptor] = &[
    BuiltinPluginDescriptor {
        id: "files",
        kind: PluginKind::Core,
        name_zh: "文件",
        hint_zh: "读写工作区文件",
        name_en: "Files",
        hint_en: "Read and write workspace files",
        toggleable: false,
        installed: always,
        switch: None,
        settings: false,
    },
    BuiltinPluginDescriptor {
        id: "usage_query",
        kind: PluginKind::Builtin,
        name_zh: "用量查询",
        hint_zh: "对话里问用了多少 token",
        name_en: "Usage",
        hint_en: "Ask how many tokens were used",
        toggleable: false,
        installed: always,
        switch: None,
        settings: false,
    },
    BuiltinPluginDescriptor {
        id: "alarm",
        kind: PluginKind::Builtin,
        name_zh: "闹钟",
        hint_zh: "定时提醒",
        name_en: "Alarm",
        hint_en: "Timed reminders",
        toggleable: true,
        installed: always,
        switch: None,
        settings: false,
    },
    BuiltinPluginDescriptor {
        id: "exchange_rate",
        kind: PluginKind::Builtin,
        name_zh: "汇率",
        hint_zh: "货币换算",
        name_en: "Exchange rate",
        hint_en: "Currency conversion",
        toggleable: true,
        installed: exchange_rate_installed,
        switch: Some(MachineSwitch {
            get: |config| config.plugins.exchange_rate.enabled,
            set: |config, on| config.plugins.exchange_rate.enabled = on,
        }),
        settings: false,
    },
    BuiltinPluginDescriptor {
        id: "archlinux",
        kind: PluginKind::Builtin,
        name_zh: "Arch Linux",
        hint_zh: "AUR 查询与审查安装、Arch 新闻",
        name_en: "Arch Linux",
        hint_en: "AUR search and audited install, Arch news",
        toggleable: true,
        installed: archlinux_installed,
        switch: Some(MachineSwitch {
            get: |config| config.plugins.archlinux.enabled,
            set: |config, on| config.plugins.archlinux.enabled = on,
        }),
        settings: true,
    },
    BuiltinPluginDescriptor {
        id: "print_image",
        kind: PluginKind::Builtin,
        name_zh: "打印图片",
        hint_zh: "把图片打到终端里,可调尺寸",
        name_en: "Print image",
        hint_en: "Print images into the terminal, size adjustable",
        toggleable: false,
        installed: always,
        switch: None,
        settings: true,
    },
    BuiltinPluginDescriptor {
        id: "memes",
        kind: PluginKind::Builtin,
        name_zh: "表情包",
        hint_zh: "用表情包回复",
        name_en: "Memes",
        hint_en: "Reply with a meme",
        toggleable: true,
        installed: memes_installed,
        switch: Some(MachineSwitch {
            get: |config| config.plugins.memes.enabled,
            set: |config, on| config.plugins.memes.enabled = on,
        }),
        settings: true,
    },
    BuiltinPluginDescriptor {
        id: "platform_outreach",
        kind: PluginKind::Builtin,
        name_zh: "外发",
        hint_zh: "从对话里给通讯平台发消息",
        name_en: "Outreach",
        hint_en: "Send to a messaging platform from the conversation",
        toggleable: false,
        installed: platform_outreach_installed,
        switch: None,
        settings: false,
    },
    BuiltinPluginDescriptor {
        id: "web_images",
        kind: PluginKind::Builtin,
        name_zh: "搜图",
        hint_zh: "网络找图",
        name_en: "Image search",
        hint_en: "Find images on the web",
        toggleable: false,
        installed: web_images_installed,
        switch: Some(MachineSwitch {
            get: |config| config.plugins.web_images.enabled,
            set: |config, on| config.plugins.web_images.enabled = on,
        }),
        settings: true,
    },
    BuiltinPluginDescriptor {
        id: "image_generation",
        kind: PluginKind::Builtin,
        name_zh: "生图",
        hint_zh: "AI 画图",
        name_en: "Image generation",
        hint_en: "Draw with AI",
        toggleable: true,
        installed: image_generation_installed,
        switch: Some(MachineSwitch {
            get: |config| config.plugins.image_generation.enabled,
            set: |config, on| config.plugins.image_generation.enabled = on,
        }),
        settings: true,
    },
    BuiltinPluginDescriptor {
        id: "knowledge_base",
        kind: PluginKind::Builtin,
        name_zh: "知识库",
        hint_zh: "自己的资料库,对话里能查",
        name_en: "Knowledge base",
        hint_en: "Your own library, searchable in conversation",
        toggleable: false,
        installed: knowledge_base_installed,
        switch: Some(MachineSwitch {
            get: |config| config.plugins.knowledge_base.enabled,
            set: |config, on| config.plugins.knowledge_base.enabled = on,
        }),
        settings: true,
    },
    BuiltinPluginDescriptor {
        id: "ledger",
        kind: PluginKind::Builtin,
        name_zh: "记账",
        hint_zh: "记账本",
        name_en: "Ledger",
        hint_en: "Expense ledger",
        toggleable: true,
        installed: always,
        switch: None,
        settings: false,
    },
    BuiltinPluginDescriptor {
        id: "scripts",
        kind: PluginKind::Provider,
        name_zh: "脚本工具",
        hint_zh: "逐个勾选",
        name_en: "Script tools",
        hint_en: "Pick them one by one",
        toggleable: false,
        installed: always,
        switch: None,
        settings: false,
    },
    // MCP 与脚本同级:插件闸之上还能按服务器 id 逐个勾(`plugins.mcp`)。
    BuiltinPluginDescriptor {
        id: "mcp",
        kind: PluginKind::Provider,
        name_zh: "MCP",
        hint_zh: "外接 MCP 服务器的工具",
        name_en: "MCP",
        hint_en: "Tools from external MCP servers",
        toggleable: false,
        installed: mcp_installed,
        switch: Some(MachineSwitch {
            get: |config| config.mcp.enabled,
            set: |config, on| config.mcp.enabled = on,
        }),
        settings: false,
    },
];

const fn plugin_ids() -> [&'static str; BUILTIN_PLUGINS.len()] {
    let mut ids = [""; BUILTIN_PLUGINS.len()];
    let mut index = 0;
    while index < BUILTIN_PLUGINS.len() {
        ids[index] = BUILTIN_PLUGINS[index].id;
        index += 1;
    }
    ids
}

/// 插件 id:与 `tools::compose_registry` 里的注册单元一一对应,persona.toml 里
/// 名字的真相源,拼错的名字在 `PersonaManifest::validate` 里能被指出来。
pub const PLUGIN_IDS: &[&str] = &plugin_ids();

const fn toggle_count() -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < BUILTIN_PLUGINS.len() {
        if BUILTIN_PLUGINS[index].toggleable {
            count += 1;
        }
        index += 1;
    }
    count
}

const fn toggle_plugin_ids() -> [&'static str; toggle_count()] {
    let mut ids = [""; toggle_count()];
    let mut filled = 0;
    let mut index = 0;
    while index < BUILTIN_PLUGINS.len() {
        if BUILTIN_PLUGINS[index].toggleable {
            ids[filled] = BUILTIN_PLUGINS[index].id;
            filled += 1;
        }
        index += 1;
    }
    ids
}

/// 引导里给开关的内置插件。其余 [`PLUGIN_IDS`] 一律常开、不摆出来。
pub const TOGGLE_PLUGINS: &[&str] = &toggle_plugin_ids();

/// 不跟人格走的机器级能力。
///
/// 它们在工具面上属于 core（`compose_core` 直接按 config 判），persona.toml
/// 管不着，所以**不进** [`PLUGIN_IDS`]——写进 persona.toml 也不会生效。但设置
/// 界面仍得给它们开关和设置页，于是单列一张表：2026-09-20 之前这两项只在
/// `config_tui` 的一张手写表里，和这张登记表口径对不上（那张表里有 web /
/// vision / memory，没有闹钟 / 汇率 / 记账）。
pub struct MachineFeature {
    pub id: &'static str,
    pub name_zh: &'static str,
    pub hint_zh: &'static str,
    /// 英文界面用这一份（理由同 `BuiltinPluginDescriptor`）。
    pub name_en: &'static str,
    pub hint_en: &'static str,
    pub switch: MachineSwitch,
    pub settings: bool,
}

pub const MACHINE_FEATURES: &[MachineFeature] = &[
    MachineFeature {
        id: "web",
        name_zh: "网络搜索",
        hint_zh: "搜索 API 与脚本兜底",
        name_en: "Web search",
        hint_en: "Search APIs with a script fallback",
        switch: MachineSwitch {
            get: |config| config.plugins.web.enabled,
            set: |config, on| config.plugins.web.enabled = on,
        },
        settings: true,
    },
    MachineFeature {
        id: "vision",
        name_zh: "识图",
        hint_zh: "图片理解与终端预览",
        name_en: "Vision",
        hint_en: "Image understanding and terminal preview",
        switch: MachineSwitch {
            get: |config| config.plugins.vision.enabled,
            set: |config, on| config.plugins.vision.enabled = on,
        },
        settings: true,
    },
];

pub fn machine_feature(id: &str) -> Option<&'static MachineFeature> {
    MACHINE_FEATURES.iter().find(|item| item.id == id)
}

pub fn descriptor(id: &str) -> Option<&'static BuiltinPluginDescriptor> {
    BUILTIN_PLUGINS.iter().find(|plugin| plugin.id == id)
}

/// 插件 id → (显示名, 一句话说明)。WebUI 与终端引导共用;不认识的 id 给空串。
pub fn plugin_label(id: &str) -> (&'static str, &'static str) {
    descriptor(id).map_or(("", ""), |plugin| (plugin.name_zh, plugin.hint_zh))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 派生出来的名单必须与 09-16 之前手写的三张表逐字相同——这是把手写表
    /// 换成派生表的等价证明。
    #[test]
    fn derived_lists_match_the_former_hand_written_tables() {
        assert_eq!(
            PLUGIN_IDS,
            [
                "files",
                "usage_query",
                "alarm",
                "exchange_rate",
                "archlinux",
                "print_image",
                "memes",
                "platform_outreach",
                "web_images",
                "image_generation",
                "knowledge_base",
                "ledger",
                "scripts",
                "mcp",
            ]
        );
        assert_eq!(
            TOGGLE_PLUGINS,
            [
                "alarm",
                "exchange_rate",
                "archlinux",
                "memes",
                "image_generation",
                "ledger",
            ]
        );
        assert_eq!(plugin_label("ledger"), ("记账", "记账本"));
        assert_eq!(plugin_label("mcp"), ("MCP", "外接 MCP 服务器的工具"));
        assert_eq!(plugin_label("nope"), ("", ""));
    }

    #[test]
    fn ids_are_unique_and_every_row_has_a_label() {
        let mut seen = std::collections::BTreeSet::new();
        for plugin in BUILTIN_PLUGINS {
            assert!(seen.insert(plugin.id), "{} 登记了两次", plugin.id);
            assert!(!plugin.name_zh.is_empty(), "{} 没有中文名", plugin.id);
            assert!(!plugin.hint_zh.is_empty(), "{} 没有一句话说明", plugin.id);
        }
    }

    /// 机器开关按配置现算:关掉插件配置,`installed` 立刻为假;常开件恒真。
    #[test]
    fn installed_follows_the_machine_config() {
        let mut config = AppConfig::default();
        config.plugins.exchange_rate.enabled = false;
        config.mcp.enabled = false;
        assert!(!(descriptor("exchange_rate").unwrap().installed)(&config));
        assert!(!(descriptor("mcp").unwrap().installed)(&config));
        assert!((descriptor("alarm").unwrap().installed)(&config));
        assert!((descriptor("files").unwrap().installed)(&config));
    }
}
