//! 斜杠命令的单一事实来源。
//!
//! 命令名、参数提示、帮助文案、以及「这一行输入是不是命令」的判定都在这里。
//! 原本长在 `cli::repl::commands` 里，是 `pub(in crate::cli)` 的——WebUI 够
//! 不到，只能自己再维护一份清单，两份迟早分叉（加一条命令忘了改另一边）。
//! 提到 crate 级之后 CLI 与 WebUI 同源，`GET /api/commands` 直接从这张表出。
//!
//! 命名避开 `commands`：`platforms::commands` 已经占了那个名字，同名子模块
//! 遮蔽上层兄弟模块在这个仓库里踩过三次（AGENTS.md 3.3 坑 3）。
//!
//! 渲染归 CLI：`print_repl_help` 与 `repl_command_suggestions_line` 要 println
//! 和终端宽度，留在 `cli::repl::commands`。

pub fn split_repl_command(input: &str) -> (&str, &str) {
    let Some((command, args)) = input.split_once(char::is_whitespace) else {
        return (input, "");
    };
    (command, args)
}

/// 从参数串里摘掉一个开关(前后都认),剩下的整段原样留给调用方。
///
/// **不按空白切词**:`/sandbox` 的参数是路径,路径里可以有空格,今天
/// `/sandbox /home/a b/c` 是能用的,切词会把它拆坏。
pub fn take_repl_flag<'a>(args: &'a str, flag: &str) -> (&'a str, bool) {
    let args = args.trim();
    if let Some(rest) = args.strip_prefix(flag) {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return (rest.trim(), true);
        }
    }
    if let Some(rest) = args.strip_suffix(flag) {
        if rest.ends_with(char::is_whitespace) {
            return (rest.trim(), true);
        }
    }
    (args, false)
}
/// Identity of a REPL slash command, dispatched via `parse_repl_input`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplSlashCommand {
    New,
    Session,
    Dev,
    Normal,
    Rename,
    Delete,
    Sandbox,
    Models,
    Persona,
    Usage,
    Config,
    Effort,
    Undo,
    Pop,
    Compact,
    Stt,
    Goal,
    Reset,
    ResetMemory,
    ResetAllMemory,
    Wipe,
    History,
    Help,
    Exit,
}

pub struct ReplCommandSpec {
    pub name: &'static str,
    /// 别名：打这些名字和打 `name` 一样。`/variant` 是 opencode 的叫法，`/effort`
    /// 是 codex / Claude Code 的叫法，两边都认。候选面板、Tab 补全、回车执行都认
    /// 别名；`/help` 与打 `/` 列全表时只出正名，别名在正名那一行后面注一句。
    pub aliases: &'static [&'static str],
    pub command: ReplSlashCommand,
    /// Argument hint rendered in /help, e.g. "[count]"; empty when the
    /// command takes no arguments (enforced at dispatch).
    pub arg_hint: &'static str,
    pub help_en: &'static str,
    pub help_zh: &'static str,
    /// 这条命令在 WebUI 的输入框里出不出现（`GET /api/commands` 按它过滤）。
    ///
    /// 绝大多数命令**不**开：WebUI 早就有对应的 GUI 入口（侧栏切会话、设置面板
    /// 改模型/人格、新建按钮），再给一个命令是两条路做同一件事，还得各自维护
    /// 一套确认弹窗。开的是 WebUI 里**没有别的办法做到**的那几件。
    pub web: bool,
}

impl ReplCommandSpec {
    pub fn help(&self) -> &'static str {
        if miyu_base::i18n::is_zh() {
            self.help_zh
        } else {
            self.help_en
        }
    }

    /// 这个名字（正名或别名）指的是不是它。大小写不敏感。
    pub fn answers_to(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    }

    /// 以 `prefix` 开头的第一个名字：正名优先，正名不中再看别名。打 `/` 列全表
    /// 时每条只出正名；打 `/var` 时正名 `/effort` 不中、别名 `/variant` 中，
    /// 候选就是 `/variant`——Tab 补的是用户正在打的那个词。
    fn suggestion_for(&self, prefix: &str) -> Option<&'static str> {
        std::iter::once(self.name)
            .chain(self.aliases.iter().copied())
            .find(|candidate| starts_with_ignore_ascii_case(candidate, prefix))
    }

    /// 用户打的是哪个名字：打的是别名就还它别名，否则正名。
    fn typed_name(&self, name: &str) -> &'static str {
        self.aliases
            .iter()
            .copied()
            .find(|alias| alias.eq_ignore_ascii_case(name))
            .unwrap_or(self.name)
    }
}

fn starts_with_ignore_ascii_case(candidate: &str, prefix: &str) -> bool {
    candidate.len() >= prefix.len()
        && candidate.is_char_boundary(prefix.len())
        && candidate[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// WebUI 输入框认得的命令。CLI 认得全部，两边同源于 `REPL_COMMAND_TABLE`。
pub fn web_commands() -> Vec<&'static ReplCommandSpec> {
    REPL_COMMAND_TABLE.iter().filter(|spec| spec.web).collect()
}

/// Single source of truth for REPL slash commands: drives Tab completion,
/// prefix resolution, /help output, and dispatch in the REPL loop.
pub const REPL_COMMAND_TABLE: &[ReplCommandSpec] = &[
    ReplCommandSpec {
        name: "/new",
        aliases: &[],
        command: ReplSlashCommand::New,
        arg_hint: "[name]",
        help_en: "create a new session and switch to it",
        help_zh: "创建新会话并切换过去",
        web: false,
    },
    ReplCommandSpec {
        name: "/session",
        aliases: &[],
        command: ReplSlashCommand::Session,
        arg_hint: "[name|index]",
        help_en: "list sessions, or switch to one (Ctrl+D deletes in the picker)",
        help_zh: "列出会话，或切换到指定会话（菜单内 Ctrl+D 删除）",
        web: false,
    },
    ReplCommandSpec {
        name: "/dev",
        aliases: &[],
        command: ReplSlashCommand::Dev,
        arg_hint: "[new]",
        help_en: "go to the dev lane's latest session; `new` starts a fresh one",
        help_zh: "去开发模式那条车道最近用的会话；加 new 开一条新的",
        web: false,
    },
    ReplCommandSpec {
        name: "/normal",
        aliases: &[],
        command: ReplSlashCommand::Normal,
        arg_hint: "[new]",
        help_en: "go to the normal lane's latest session; `new` starts a fresh one",
        help_zh: "去普通模式那条车道最近用的会话；加 new 开一条新的",
        web: false,
    },
    ReplCommandSpec {
        name: "/rename",
        aliases: &[],
        command: ReplSlashCommand::Rename,
        arg_hint: "<name>",
        help_en: "rename the current session",
        help_zh: "重命名当前会话",
        web: false,
    },
    ReplCommandSpec {
        name: "/delete",
        aliases: &[],
        command: ReplSlashCommand::Delete,
        arg_hint: "[name|index]",
        help_en: "delete a session (current by default)",
        help_zh: "删除会话（默认当前会话）",
        web: false,
    },
    ReplCommandSpec {
        name: "/sandbox",
        aliases: &[],
        command: ReplSlashCommand::Sandbox,
        arg_hint: "[path [--allow-read]|clear]",
        help_en: "confine this session to a directory (Landlock); `--allow-read` locks writes only, no arg shows, `clear` unbinds",
        help_zh: "把本会话关进某个目录(Landlock 沙盒);--allow-read 只锁写、读放开,不带参数查看,clear 解绑",
        // WebUI 也开(09-13 用户拍板):浏览器里没有别的入口能做这件事。成员会话
        // 本来就关在自己家里,daemon 侧一律拒绝。
        web: true,
    },
    ReplCommandSpec {
        name: "/models",
        aliases: &[],
        command: ReplSlashCommand::Models,
        arg_hint: "[index|provider/model|default]",
        help_en: "switch this session's model",
        help_zh: "切换当前会话使用的模型",
        web: false,
    },
    ReplCommandSpec {
        name: "/persona",
        aliases: &[],
        command: ReplSlashCommand::Persona,
        arg_hint: "[name]",
        help_en: "switch the active persona",
        help_zh: "切换当前人格",
        web: false,
    },
    ReplCommandSpec {
        name: "/usage",
        aliases: &[],
        command: ReplSlashCommand::Usage,
        arg_hint: "",
        help_en: "show token usage details",
        help_zh: "显示 Token 用量详情",
        web: false,
    },
    ReplCommandSpec {
        name: "/config",
        aliases: &[],
        command: ReplSlashCommand::Config,
        arg_hint: "",
        help_en: "open configuration UI",
        help_zh: "打开配置界面",
        web: false,
    },
    ReplCommandSpec {
        name: "/effort",
        aliases: &["/variant"],
        command: ReplSlashCommand::Effort,
        arg_hint: "[name]",
        help_en: "view or switch thinking level",
        help_zh: "查看或切换思考档位",
        web: false,
    },
    ReplCommandSpec {
        name: "/undo",
        aliases: &[],
        command: ReplSlashCommand::Undo,
        arg_hint: "",
        help_en: "undo the last turn or context compaction",
        help_zh: "撤销上一轮或上下文压缩",
        web: false,
    },
    ReplCommandSpec {
        name: "/pop",
        aliases: &[],
        command: ReplSlashCommand::Pop,
        arg_hint: "[count]",
        help_en: "pop selected turns or the oldest count from active context",
        help_zh: "从当前上下文弹出所选轮次或最旧的指定轮数",
        // WebUI 只有按数量的形态（交互式挑选依赖终端）。
        web: true,
    },
    ReplCommandSpec {
        name: "/compact",
        aliases: &[],
        command: ReplSlashCommand::Compact,
        arg_hint: "",
        help_en: "compact current conversation context now",
        help_zh: "立即压缩当前会话上下文",
        web: true,
    },
    ReplCommandSpec {
        name: "/stt",
        aliases: &[],
        command: ReplSlashCommand::Stt,
        arg_hint: "",
        help_en:
            "dictate with the microphone: speech goes into the input box; Esc stops (needs voice enabled)",
        help_zh: "用麦克风听写：说的话填进输入框(需开启语音功能);Esc 停止,回车发送",
        web: false,
    },
    ReplCommandSpec {
        name: "/goal",
        aliases: &[],
        command: ReplSlashCommand::Goal,
        arg_hint: "[目标|edit <新目标>|pause|resume|clear]",
        help_en: "give the session a long task and let it keep working on it by itself",
        help_zh: "交代一件长活，之后它会自己一轮轮做下去；不带参数看进度",
        web: true,
    },
    ReplCommandSpec {
        name: "/reset",
        // `/clear` 就是 `/reset`（用户 09-24：功能相同）。它原来是清屏，清屏还有
        // Ctrl+L；别处的提示（压缩打转时「或使用 /clear 重新开始」）说的一直是这个意思。
        aliases: &["/clear"],
        command: ReplSlashCommand::Reset,
        arg_hint: "",
        help_en: "start this conversation over",
        help_zh: "重新开始当前会话",
        web: true,
    },
    ReplCommandSpec {
        name: "/reset-memory",
        aliases: &[],
        command: ReplSlashCommand::ResetMemory,
        arg_hint: "",
        help_en: "erase the long-term memory this conversation produced",
        help_zh: "清空本次会话记下的长期记忆",
        web: true,
    },
    ReplCommandSpec {
        name: "/reset-all-memory",
        aliases: &[],
        command: ReplSlashCommand::ResetAllMemory,
        arg_hint: "",
        help_en: "erase this mode's entire long-term memory",
        help_zh: "清空当前模式的全部长期记忆",
        web: true,
    },
    ReplCommandSpec {
        name: "/wipe",
        aliases: &[],
        command: ReplSlashCommand::Wipe,
        arg_hint: "",
        help_en: "erase memory, every conversation and group contexts (skills and scripts stay)",
        help_zh: "抹掉记忆、所有会话内容、群聊上下文（技能和脚本保留）",
        web: false,
    },
    ReplCommandSpec {
        name: "/history",
        aliases: &[],
        command: ReplSlashCommand::History,
        arg_hint: "",
        help_en: "show recent conversation history",
        help_zh: "显示最近的会话历史",
        web: false,
    },
    ReplCommandSpec {
        name: "/help",
        aliases: &[],
        command: ReplSlashCommand::Help,
        arg_hint: "",
        help_en: "show this help",
        help_zh: "显示此帮助",
        web: false,
    },
    ReplCommandSpec {
        name: "/exit",
        aliases: &[],
        command: ReplSlashCommand::Exit,
        arg_hint: "",
        help_en: "leave REPL",
        help_zh: "退出 REPL",
        web: false,
    },
];

pub fn repl_command_spec(command: ReplSlashCommand) -> &'static ReplCommandSpec {
    REPL_COMMAND_TABLE
        .iter()
        .find(|spec| spec.command == command)
        .expect("every ReplSlashCommand has a table entry")
}
/// Parsed REPL input: plain chat, a resolved slash command with its argument
/// string, or an unknown/ambiguous slash command.
/// 一行输入的归类。**没有「未知命令」这一类**——`/` 开头但不命中命令表的输入
/// 就是聊天（见 `parse_repl_input` 的文档）。
pub enum ReplInput<'a> {
    Chat,
    Slash(ReplSlashCommand, &'a str),
}

pub fn repl_commands() -> Vec<&'static str> {
    REPL_COMMAND_TABLE.iter().map(|spec| spec.name).collect()
}

/// 输入框的候选命令名。
///
/// 还在打命令名（没出现空白）：按前缀筛，正名与别名都算，每条命令最多出一个名字。
/// 命令名后面已经有空白（在打参数，或刚敲了个空格）：只认打全的那一条——候选
/// 不再随着「打全」而消失。以前 `/sessio` 有候选、补上 `n` 反而没了，像是打错了
/// 一样（用户实测）；现在打全了它还在，在打参数时也还在。
pub fn repl_command_suggestions(input: &str) -> Vec<&'static str> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    if input.contains(char::is_whitespace) {
        let (name, _) = split_repl_command(input);
        return repl_command_spec_for_name(name)
            .map(|spec| vec![spec.typed_name(name)])
            .unwrap_or_default();
    }
    REPL_COMMAND_TABLE
        .iter()
        .filter_map(|spec| spec.suggestion_for(input))
        .collect()
}

/// Tab 补全：唯一候选才展开。已经在打参数的行不动——候选那时只是「这条命令
/// 打全了」的确认，展开会把参数抹掉。
pub fn complete_repl_command(input: &str) -> Option<&'static str> {
    if input.contains(char::is_whitespace) {
        return None;
    }
    let suggestions = repl_command_suggestions(input);
    if suggestions.len() == 1 {
        suggestions.first().copied()
    } else {
        None
    }
}

/// 按**完整**名字（正名或别名）找命令。
pub fn repl_command_spec_for_name(name: &str) -> Option<&'static ReplCommandSpec> {
    REPL_COMMAND_TABLE.iter().find(|spec| spec.answers_to(name))
}

/// `name` 是不是 `command` 的名字（正名或别名）。直连道的 if 链用它，别名知识
/// 只留在命令表一处。
pub fn names_repl_command(name: &str, command: ReplSlashCommand) -> bool {
    repl_command_spec_for_name(name).is_some_and(|spec| spec.command == command)
}

/// 命令表里有没有这个**完整**名字（正名或别名）。执行前的唯一判定入口——直连道
/// 的 if 链和泄漏守门都问它，不再走前缀展开（理由见 `parse_repl_input`）。
pub fn is_repl_command(name: &str) -> bool {
    repl_command_spec_for_name(name).is_some()
}

/// 一条斜杠命令在**回合跑着的时候**能不能执行，以及怎么执行。
///
/// 用户 09-20 拍板：原来回合中所有命令一律静默吞掉（连 `/new` `/goal` 都做不
/// 到），实际上三类东西的约束完全不同，分开对待。判据是「**要不要往屏幕上写
/// 东西**」——全屏 TUI 的正文是按块记账的，回合中往缓冲里插字会把正在开的块
/// 写坏（09-19 那个「滚动思考点开出现两份」就是块半开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuringTurn {
    /// 就地执行，不打断跟随。只发 IPC、最多出一条 ≤2 行的浮层提示——浮层是逐
    /// 帧画上去的，根本不碰正文缓冲。
    Inline,
    /// 先把这一轮**分离到后台**（daemon 照跑），执行完再按事件号挂回来接着
    /// 看。要占屏的（超过 2 行的长文、要重载 daemon 的面板）和会换走会话的
    /// 都走这条。
    Detach,
    /// 面板**寄宿在回合循环里**跑：键盘交给面板，事件流在 socket 里排队，
    /// 面板收掉接着画，全程同一个渲染器（09-20）。
    ///
    /// 原来 `/models` `/session` 走 `Detach`：每分离一次就把当前渲染段收尾
    /// 定稿吐一行小结、挂回来又是全新的渲染器重新计时——用户开三次面板，
    /// 同一段思考就成了三行「Worked for … 1 thought」。这不是收尾方式选错，
    /// 是段落被切开这件事本身躲不掉，只有不分离才没有接缝。
    ///
    /// 只有全屏 TUI 有面板；行内 REPL 拿到它就当 `Detach` 处理。
    Panel,
    /// 回合中做不了。`reason_*` 是给用户看的一句话——原来是**静默**吞掉，
    /// 屏幕上一点反应都没有，比拒绝本身更难受。
    Blocked {
        reason_en: &'static str,
        reason_zh: &'static str,
    },
}

impl DuringTurn {
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            DuringTurn::Blocked {
                reason_en,
                reason_zh,
            } => Some(if miyu_base::i18n::is_zh() {
                reason_zh
            } else {
                reason_en
            }),
            _ => None,
        }
    }
}

/// 回合跑着时这条命令怎么办。`args` 参与判断：`/effort max` 只是改个配置，
/// `/effort` 不带参数要弹面板，两者不是一回事。
pub fn during_turn(command: ReplSlashCommand, args: &str) -> DuringTurn {
    use ReplSlashCommand::*;
    let bare = args.trim().is_empty();
    match command {
        // ── 就地执行 ──
        // `/goal` 09-19 就破例放行了：它**完全不往屏幕上写**（成功静默，
        // 后果由 daemon 的续轮体现），所以跟随一点都不用断。
        //
        // 别的命令即使也不占屏（`/rename` `/sandbox` 就一条 IPC + 浮层），
        // 实现都挂在 `RemoteRepl` 上，回合循环够不到——在这里重写一遍就是
        // 第二份事实来源，迟早分叉。宁可让它们和面板类走同一条「分离 →
        // 执行 → 挂回来」，代价是正文上留一道接缝。
        Goal => DuringTurn::Inline,

        // ── 面板寄宿在回合里 ──
        // 这两个不带参数就是弹面板，而且落地不用重载 daemon：`/models` 只要
        // 路径 + 会话 id 就能就地落盘；`/session` 挑完把结果带回 `RemoteRepl`
        // 去切（换走之后这一轮本来就不再跟）。`/effort` `/persona` 的面板选完
        // 要 `ReloadConfig`、还可能连带换会话，仍走下面的分离路。
        Models | Session if bare => DuringTurn::Panel,

        // ── 分离 → 执行 → 挂回来 ──
        // 换会话的：换走之后这一轮就不该再跟了，它在 daemon 里继续跑。
        New | Dev | Normal | Session => DuringTurn::Detach,
        // 要占屏的面板；`/effort <档位>` 带参数不弹面板，但仍走同一条路。
        Effort | Models | Persona | Config => DuringTurn::Detach,
        // 长文（超过 2 行就落回正文缓冲，回合中写缓冲会写坏正在开的块）。
        Help | Usage | History => DuringTurn::Detach,
        // 一条 IPC + 浮层，不占屏，但实现在 `RemoteRepl` 上（见上）。
        Rename | Sandbox | Stt => DuringTurn::Detach,
        // 退出：Ctrl+D 本来就能在回合中退（回合留在 daemon 里继续跑），
        // `/exit` 只是被这道闸吞掉了。
        Exit => DuringTurn::Detach,

        // ── 回合中做不了 ──
        // 前四个 daemon 自己就拒：`reserve_admin_for_session` 看到这条会话
        // 有活动回合直接 409。客户端提前说清楚，别等它报一句 admin busy。
        Undo | Pop | Compact | Reset => DuringTurn::Blocked {
            reason_en: "this rewrites the conversation the running reply is still appending to; wait for it to finish",
            reason_zh: "这要改写正在被续写的对话，等这一轮说完再来",
        },
        Delete | Wipe => DuringTurn::Blocked {
            reason_en: "this would kill the running reply; interrupt it first (Esc) if that is what you want",
            reason_zh: "这会把正在跑的这一轮掐掉；真要这么做就先按 Esc 打断",
        },
        ResetMemory | ResetAllMemory => DuringTurn::Blocked {
            reason_en: "she may be writing memory this very turn; wait for it to finish",
            reason_zh: "她这一轮可能正在写记忆，等说完再来",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回合中 `/models` `/session` 不带参数是面板，寄宿在回合里；带了参数不弹
    /// 面板（`/models default` 直接打印、`/session <名字>` 直接切），仍走分离
    /// （09-20）。`/effort` `/persona` 的面板选完要重载 daemon，也仍走分离。
    #[test]
    fn bare_models_and_session_are_hosted_panels_during_a_turn() {
        use ReplSlashCommand::*;
        assert_eq!(during_turn(Models, ""), DuringTurn::Panel);
        assert_eq!(during_turn(Session, "   "), DuringTurn::Panel);
        assert_eq!(during_turn(Models, "default"), DuringTurn::Detach);
        assert_eq!(during_turn(Session, "昨天那条"), DuringTurn::Detach);
        assert_eq!(during_turn(Effort, ""), DuringTurn::Detach);
        assert_eq!(during_turn(Persona, ""), DuringTurn::Detach);
        assert_eq!(during_turn(Goal, "看看"), DuringTurn::Inline);
        assert!(during_turn(Compact, "").reason().is_some());
        assert!(during_turn(Models, "").reason().is_none());
    }
}
