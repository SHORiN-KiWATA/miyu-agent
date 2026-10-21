//! 浮动尾部人格提醒(persona-reminder)。
//!
//! 背景:平台会话的请求里角色声音只占约一成,历史里机器块与长回复的
//! 自模仿会把人设淹掉("聊几轮就忘")。A/B 实测(2026-08-14,真实群聊
//! 会话重建请求)里,把一小段"说话方式备忘"追加在实时请求绝对末尾是
//! 唯一对已污染会话也有效的手段:入戏率 5/12 → 8/12,而完整人格贴尾、
//! 身份摘要、多副本全部实测更差。
//!
//! 备忘由模型从人格文件自动蒸馏(篇幅规范从设定本身推断,短人格蒸出
//! "话很少"、话痨人格蒸出"爱长篇";简短型的讲解条款由代码按字节追加,
//! 不经蒸馏)。产物按(蒸馏指令+人格内容)hash 缓存在
//! `state_dir/persona-hints/<scope>.<hash>.md`,两者都不改就只蒸一次。
//! 所有会话形态(终端/WebUI/平台)一致生效,提醒文本不含平台场景描述。
//!
//! 注入方式(08-16 起):不再浮动,每隔 `prompt.persona_reminder_interval` 轮
//! 以化石身份进历史(`agent/history.rs`,间隔按历史里最近一份提醒化石数),
//! 纯追加、不掰前缀。要不要这个子系统由 `config::subsystems`(人格清单 ×
//! `prompt.persona_reminder`)裁决,dev 无人格恒不提醒。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::llm::{ChatMessage, OpenAiCompatibleClient};
use miyu_base::config::AppConfig;
use miyu_base::paths::MiyuPaths;

/// 蒸馏指令。要求两行输出:首行篇幅类型(短|长),次行风格备忘正文。
/// 正文全部用陈述句,且必须含一条从设定文件推断的整体篇幅陈述
/// (不许一律默认写很短——话痨型人格要如实蒸出爱长篇),不提场景平台。
const DISTILL_PROMPT: &str = "Below is an AI persona definition file. Output exactly two lines:\
the first line is a single character — write 「短」 if the persona leans toward brief replies, 「长」 if it leans toward long, detailed ones,\
judging from the file's sentence-length limits, line-break rules, usage scenarios and tone; with no such evidence, judge from the persona's temperament.\
The second line distills the persona's speaking style and hard rules into one third-person statement of at most 130 characters,\
to be placed at the end of chat requests as a reminder to stay in character. Requirements for the second line: write it in the same language as the persona file;\
use only declarative sentences (what this persona is like, what it never does), no commands;\
it must include one statement about overall reply length — how long replies usually are; if the persona favors detailed explanations, faithfully say it likes to write at length,\
do not default to very short; do not add exception clauses of your own such as 'expands when explanation is needed';\
no lists and no line breaks within the line; prioritize the hard rules in the file that are easiest to violate\
(sentence length, emoji, punctuation, line breaks, parenthetical asides, bold text, tone, formatting);\
do not mention the definition file itself, do not start with the persona's name, and do not describe the scene or platform of the conversation.\
Output nothing beyond these two lines.";

/// 简短型人格的定长讲解条款,由代码追加、不经蒸馏——A/B 实测这一条是
/// 技术答疑压缩的全部来源(0/6→4/6,中位 243→112 字),而交给蒸馏
/// 意译则丢失(意译版 0/6,与无条款基线无差)。措辞贴近实测变体,但
/// 省略主语:人格代词(他/她/它)因人而异,不能写死。
const TERSE_EXPLAIN_CLAUSE: &str =
    "就算是讲解答疑，也只说最关键的两三步，整条不超过一百字，一次说不完就等对方追问。";

/// 蒸馏产物正文的字符上限。v3 指令要求 ≤130 字,越界通常意味着模型没
/// 守约;截断比整段丢弃便宜,尾部块的价值在前几条硬规则上。
const BODY_MAX_CHARS: usize = 240;
const NAME_MAX_CHARS: usize = 24;
const DISTILL_MAX_TOKENS: u32 = 4000;
const DISTILL_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistilledHint {
    pub body: String,
}

/// 手写提醒文件:`<prompts_dir>/hints/<scope>.md`。放子目录是有意的——
/// 人格列表与校验只扫 prompts_dir 下的文件,hints/ 不会被当成人格。
/// 默认 Miyu 人格的 scope 是 "default",对应 hints/default.md。
pub fn manual_hint_path(config: &AppConfig, paths: &MiyuPaths, scope: &str) -> PathBuf {
    config
        .prompts_dir_path(paths)
        .join("hints")
        .join(format!("{scope}.md"))
}

/// 默认 Miyu 人格是否生效:未选自定义人格、也没有任何自定义 system
/// prompt 时,base_system_prompt 落到内置 miyu.md——只有这时内置的
/// 提示/预设对话默认值才有意义。
fn uses_default_persona(config: &AppConfig, paths: &MiyuPaths) -> bool {
    config
        .active_persona_prompt(paths)
        .map(|prompt| prompt.trim().is_empty())
        .unwrap_or(false)
}

/// 读取手写提醒。文件缺失且默认 Miyu 人格生效时回退到内置默认提示;
/// 文件存在但为空 = 显式停用手写(回到自动蒸馏)。
pub fn manual_hint(config: &AppConfig, paths: &MiyuPaths, scope: &str) -> Option<String> {
    match std::fs::read_to_string(manual_hint_path(config, paths, scope)) {
        Ok(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Err(_) if scope == "default" && uses_default_persona(config, paths) => {
            let text = miyu_base::prompts::default_miyu_hint();
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        Err(_) => None,
    }
}

/// 预设对话(begin_dialogs):`<prompts_dir>/dialogs/<scope>.md`,以行首
/// `user:` 与 `assistant:` 标记的消息对。每次请求注入 system 之后、真实
/// 历史之前,永不落库——模型把它们当普通聊天记录,学的是轮次里的语气
/// 而不是指令(AstrBot begin_dialogs 同机制;A/B 实测示例对是语气打磨
/// 层,闲聊中位 63→48 字)。常量前缀:只在编辑时断一次缓存。
pub fn dialogs_path(config: &AppConfig, paths: &MiyuPaths, scope: &str) -> PathBuf {
    config
        .prompts_dir_path(paths)
        .join("dialogs")
        .join(format!("{scope}.md"))
}

/// 预设对话原文(注入与 TUI 表单预填共用)。文件缺失且默认 Miyu 人格
/// 生效时回退到内置默认;文件存在但为空 = 显式停用。
pub fn dialogs_raw(config: &AppConfig, paths: &MiyuPaths, scope: &str) -> String {
    match std::fs::read_to_string(dialogs_path(config, paths, scope)) {
        Ok(text) => text.trim().to_string(),
        Err(_) if scope == "default" && uses_default_persona(config, paths) => {
            miyu_base::prompts::default_miyu_dialogs()
                .trim()
                .to_string()
        }
        Err(_) => String::new(),
    }
}

/// 解析后的预设对话对(注入用)。
pub fn load_dialogs(config: &AppConfig, paths: &MiyuPaths, scope: &str) -> Vec<(String, String)> {
    parse_dialogs(&dialogs_raw(config, paths, scope))
}

/// `user:` 行开新的用户消息,`assistant:` 行开对应回复(标记大小写不
/// 敏感,冒号全半角均可),后续无标记行并入当前段(保留换行);没有配到
/// 回复的尾部 user 丢弃,防止注入无应答的 user 消息。
pub fn parse_dialogs(raw: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut question: Option<String> = None;
    let mut answer: Option<String> = None;
    let mut flush = |question: &mut Option<String>, answer: &mut Option<String>| {
        if let (Some(q), Some(a)) = (question.take(), answer.take()) {
            let (q, a) = (q.trim().to_string(), a.trim().to_string());
            if !q.is_empty() && !a.is_empty() {
                pairs.push((q, a));
            }
        }
        *question = None;
        *answer = None;
    };
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = strip_role_marker(trimmed, "user") {
            flush(&mut question, &mut answer);
            question = Some(rest.to_string());
        } else if let Some(rest) = strip_role_marker(trimmed, "assistant") {
            if question.is_some() && answer.is_none() {
                answer = Some(rest.to_string());
            }
        } else if let Some(current) = answer.as_mut().or(question.as_mut()) {
            current.push('\n');
            current.push_str(line);
        }
    }
    flush(&mut question, &mut answer);
    pairs
}

/// `parse_dialogs` 的逆:对列表写回 `user:`/`assistant:` 行格式,对与
/// 对之间空一行(纯可读性,解析忽略)。多行内容靠续行进位,往返无损。
pub fn format_dialogs(pairs: &[(String, String)]) -> String {
    let mut out = String::new();
    for (index, (question, answer)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str("user: ");
        out.push_str(question);
        out.push('\n');
        out.push_str("assistant: ");
        out.push_str(answer);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// 行首角色标记 `<role>:`(ASCII 大小写不敏感,冒号全半角均可)。
fn strip_role_marker<'a>(line: &'a str, role: &str) -> Option<&'a str> {
    let head = line.get(..role.len())?;
    if !head.eq_ignore_ascii_case(role) {
        return None;
    }
    let rest = &line[role.len()..];
    rest.strip_prefix(':')
        .or_else(|| rest.strip_prefix('：'))
        .map(str::trim_start)
}

/// Miyu 表单预填:default scope 的文件现值,缺失时直接给内置默认——
/// 不看当前激活的是哪个人格,因为表单编辑的就是 Miyu 本体的附属文件。
pub fn miyu_aux_prefill(config: &AppConfig, paths: &MiyuPaths) -> (String, String) {
    let hint = match std::fs::read_to_string(manual_hint_path(config, paths, "default")) {
        Ok(text) => text.trim().to_string(),
        Err(_) => miyu_base::prompts::default_miyu_hint().trim().to_string(),
    };
    let dialogs = match std::fs::read_to_string(dialogs_path(config, paths, "default")) {
        Ok(text) => text.trim().to_string(),
        Err(_) => miyu_base::prompts::default_miyu_dialogs()
            .trim()
            .to_string(),
    };
    (hint, dialogs)
}

fn auto_cache_dir(paths: &MiyuPaths) -> PathBuf {
    paths.state_dir.join("persona-hints")
}

fn cache_file_name(scope: &str, persona: &str) -> String {
    // 蒸馏指令一并入 hash:指令升级(如篇幅条款收紧)自动触发重蒸,
    // 不必让用户手动清缓存。
    let mut hasher = blake3::Hasher::new();
    hasher.update(DISTILL_PROMPT.as_bytes());
    hasher.update(persona.as_bytes());
    format!("{scope}.{}.md", &hasher.finalize().to_hex()[..16])
}

fn parse_cache_file(content: &str) -> Option<DistilledHint> {
    let body = content.trim().to_string();
    (!body.is_empty()).then_some(DistilledHint { body })
}

/// 蒸馏原始输出 → 产物。两行:篇幅类型(短|长) / 正文。简短型人格由
/// 代码把 [`TERSE_EXPLAIN_CLAUSE`] 追加到正文末尾——制胜条款字节固定,
/// 不给蒸馏意译丢失的机会。容错:模型偶尔仍在最前多输出一行名字
/// (短且非标记,且次行是标记)则丢弃;类型行缺失时不追加条款(保守)。
fn parse_distill_output(raw: &str) -> Option<DistilledHint> {
    let mut lines = raw
        .lines()
        .map(|line| line.trim().trim_matches(['"', '“', '”', '`']).trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let is_marker =
        |line: &str| line.chars().count() <= 4 && (line.contains('短') || line.contains('长'));
    if lines.len() > 1
        && !is_marker(&lines[0])
        && lines[0].chars().count() <= NAME_MAX_CHARS
        && is_marker(&lines[1])
    {
        lines.remove(0);
    }
    let terse = match lines.first().map(String::as_str) {
        Some(marker) if is_marker(marker) => {
            let terse = marker.contains('短');
            lines.remove(0);
            terse
        }
        _ => false,
    };
    let mut body = lines
        .join("")
        .chars()
        .take(BODY_MAX_CHARS)
        .collect::<String>();
    if body.is_empty() {
        return None;
    }
    if terse {
        body.push_str(TERSE_EXPLAIN_CLAUSE);
    }
    Some(DistilledHint { body })
}

/// 解析当前人格的提醒全文:手写文件优先(原样使用),否则走自动蒸馏——
/// 命中缓存直接读,未命中蒸馏一次并落盘,同 scope 的旧 hash 缓存
/// 顺手清掉。
pub async fn resolve(
    config: &AppConfig,
    paths: &MiyuPaths,
    client: &OpenAiCompatibleClient,
) -> Result<Option<String>> {
    let scope = config.active_persona_scope();
    if let Some(text) = manual_hint(config, paths, &scope) {
        return Ok(Some(text));
    }
    let persona = config.base_system_prompt(paths)?;
    if persona.trim().is_empty() {
        return Ok(None);
    }
    let directory = auto_cache_dir(paths);
    let file = directory.join(cache_file_name(&scope, &persona));
    if let Ok(content) = std::fs::read_to_string(&file) {
        return Ok(parse_cache_file(&content).map(|hint| hint.body));
    }
    let hint = distill(client, &persona).await?;
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    remove_stale_cache(&directory, &scope, &file);
    std::fs::write(&file, format!("{}\n", hint.body))
        .with_context(|| format!("failed to write {}", file.display()))?;
    Ok(Some(hint.body))
}

fn remove_stale_cache(directory: &Path, scope: &str, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let prefix = format!("{scope}.");
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".md"));
        if stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn distill(client: &OpenAiCompatibleClient, persona: &str) -> Result<DistilledHint> {
    // 推理型模型会先把 token 预算吃在思维链上,预算给足并对空产物重试
    // (实测 deepseek-v4-flash 蒸馏时思维链膨胀会返回空正文)。
    let client = client.with_max_tokens(DISTILL_MAX_TOKENS);
    let prompt = format!("{DISTILL_PROMPT}\n\n{persona}");
    let mut last_len = 0;
    for _ in 0..DISTILL_ATTEMPTS {
        let result = client
            .chat_buffered(vec![ChatMessage::plain("user", prompt.clone())], Vec::new())
            .await?;
        if let Some(hint) = parse_distill_output(&result.content) {
            return Ok(hint);
        }
        last_len = result.content.len();
    }
    anyhow::bail!("persona hint distillation returned no usable text (last output {last_len}B)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_pairs_round_trip_through_line_format() {
        // TUI 列表编辑器(验收 #19)靠这一对函数存取:多行内容走续行,
        // 序列化→解析必须无损,否则每次进出编辑器都会磨损内容。
        let pairs = vec![
            ("你好".to_string(), "嗯,在。".to_string()),
            ("多行\n第二行".to_string(), "回答\n也两行".to_string()),
        ];
        assert_eq!(parse_dialogs(&format_dialogs(&pairs)), pairs);
        assert_eq!(format_dialogs(&[]), "");
    }

    #[test]
    fn distill_output_terse_appends_fixed_clause() {
        let hint = parse_distill_output("短\n回复通常很短,从不使用Emoji。").unwrap();
        assert_eq!(
            hint.body,
            format!("回复通常很短,从不使用Emoji。{TERSE_EXPLAIN_CLAUSE}")
        );
    }

    #[test]
    fn distill_output_verbose_keeps_body_as_is() {
        let hint = parse_distill_output("长\n回复通常较长且详尽。").unwrap();
        assert_eq!(hint.body, "回复通常较长且详尽。");
    }

    #[test]
    fn distill_output_drops_stray_leading_name_line() {
        // 模型偶尔仍按旧习惯多输出一行名字:丢弃,不混进正文。
        let hint = parse_distill_output("未有\n短\n回复很短。").unwrap();
        assert_eq!(hint.body, format!("回复很短。{TERSE_EXPLAIN_CLAUSE}"));
    }

    #[test]
    fn distill_output_tolerates_marker_variants_and_quotes() {
        let hint = parse_distill_output("「短」\n回复很短。\n\n从不换行。").unwrap();
        assert!(hint.body.starts_with("回复很短。从不换行。"));
        assert!(hint.body.ends_with(TERSE_EXPLAIN_CLAUSE));
    }

    #[test]
    fn distill_output_missing_marker_skips_clause() {
        let hint = parse_distill_output("回复通常很短,语气毒舌。").unwrap();
        assert_eq!(hint.body, "回复通常很短,语气毒舌。");
    }

    #[test]
    fn distill_output_empty_is_none() {
        assert!(parse_distill_output("").is_none());
        assert!(parse_distill_output("  \n\n").is_none());
    }

    #[test]
    fn body_is_capped_before_clause() {
        let body = "字".repeat(BODY_MAX_CHARS + 50);
        let hint = parse_distill_output(&format!("长\n{body}")).unwrap();
        assert_eq!(hint.body.chars().count(), BODY_MAX_CHARS);
    }

    #[test]
    fn cache_file_roundtrip() {
        let hint = DistilledHint {
            body: "回复很短。".to_string(),
        };
        assert_eq!(parse_cache_file("回复很短。\n").unwrap(), hint);
        assert!(parse_cache_file("  \n").is_none());
    }

    fn test_paths(root: &std::path::Path) -> MiyuPaths {
        MiyuPaths {
            root_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config/config.jsonc"),
            skills_dir: root.join("config/skills"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            pictures_dir: root.join("pictures"),
            fish_hook_file: root.join("fish/miyu.fish"),
            bash_hook_file: root.join("shell/bash-hook.sh"),
            zsh_hook_file: root.join("shell/zsh-hook.zsh"),
            scripts_dir: root.join("config/scripts"),
            system_scripts_dir: PathBuf::new(),
        }
    }

    #[test]
    fn manual_hint_precedence_file_over_embedded_over_none() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        // 默认人格 + 无文件 → 内置默认提示。
        assert_eq!(
            manual_hint(&config, &paths, "default").as_deref(),
            Some(miyu_base::prompts::default_miyu_hint().trim())
        );
        // 自定义 scope 不回退内置。
        assert!(manual_hint(&config, &paths, "custom").is_none());
        let path = manual_hint_path(&config, &paths, "default");
        assert!(path.ends_with("hints/default.md"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 文件存在 → 文件赢。
        std::fs::write(&path, "  手写提醒。\n").unwrap();
        assert_eq!(
            manual_hint(&config, &paths, "default").as_deref(),
            Some("手写提醒。")
        );
        // 文件存在但空 = 显式停用手写(回到自动蒸馏),不回退内置。
        std::fs::write(&path, "   \n").unwrap();
        assert!(manual_hint(&config, &paths, "default").is_none());
    }

    /// 内置对话示范可完整解析:条数不写死(内容会被编辑),只保证
    /// 首对精确、全部成对非空。
    #[test]
    fn embedded_miyu_dialogs_parse_into_pairs() {
        let pairs = parse_dialogs(&miyu_base::prompts::default_miyu_dialogs());
        assert!(pairs.len() >= 12);
        assert_eq!(pairs[0].0, "问个事，现在兼容层玩终末地咋样？");
        assert_eq!(pairs[0].1, "挺好的呀，帧数表现完全原生");
        assert!(pairs
            .iter()
            .all(|(user, assistant)| !user.is_empty() && !assistant.is_empty()));
    }

    #[test]
    fn default_persona_dialogs_fall_back_to_embedded() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path());
        let config = AppConfig::default();
        assert!(load_dialogs(&config, &paths, "default").len() >= 12);
        assert!(load_dialogs(&config, &paths, "custom").is_empty());
        // 空文件 = 显式停用。
        let path = dialogs_path(&config, &paths, "default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "\n").unwrap();
        assert!(load_dialogs(&config, &paths, "default").is_empty());
    }

    #[test]
    fn dialogs_parse_pairs_and_drop_dangling_question() {
        let raw = "user: 你好\nassistant: 哼，又来一个。\n\nUser: 帮我写脚本\n还要能跑\nassistant: 写可以，报错别来找我哭。\nuser: 没有回答的问题";
        let pairs = parse_dialogs(raw);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("你好".to_string(), "哼，又来一个。".to_string()));
        assert_eq!(pairs[1].0, "帮我写脚本\n还要能跑");
        assert_eq!(pairs[1].1, "写可以，报错别来找我哭。");
        assert!(parse_dialogs("").is_empty());
        assert!(parse_dialogs("随便写点没有标记的行").is_empty());
    }

    #[test]
    fn dialogs_parse_fullwidth_colon_markers() {
        let pairs = parse_dialogs("user：问题\nassistant：回答");
        assert_eq!(pairs, vec![("问题".to_string(), "回答".to_string())]);
    }

    #[test]
    fn dialogs_parse_ignores_unmarked_preamble() {
        let pairs = parse_dialogs("## 标题\n\n说明行。\n\nuser: 你好\nassistant: 嗯");
        assert_eq!(pairs, vec![("你好".to_string(), "嗯".to_string())]);
    }

    #[test]
    fn cache_name_tracks_persona_content() {
        let a = cache_file_name("default", "人格A");
        let b = cache_file_name("default", "人格B");
        assert_ne!(a, b);
        assert!(a.starts_with("default.") && a.ends_with(".md"));
    }
}
