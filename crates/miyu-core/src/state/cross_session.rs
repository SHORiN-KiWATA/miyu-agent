//! 跨会话消息的外壳（09-23）：另一个会话里的 AI 用 `send_to_other_running_session`
//! 投进来的那条「用户消息」。
//!
//! ```text
//! <cross-session-message from="会话名" session="会话 id">
//! Sent by the AI in another session, not by the user.
//! 正文
//! </cross-session-message>
//! ```
//!
//! 外壳既是模型收到的原文，也是给人看的那一份。拼在场所层（发件会话名是不可信字段，
//! 要过平台层的 `safe_prompt_field`），拆在这里：daemon 重投、终端显示与回放都要认它。

/// 开头标签带属性，所以只取到属性之前。
pub const CROSS_SESSION_MESSAGE_TAG: &str = "<cross-session-message";
pub const CROSS_SESSION_CLOSE_TAG: &str = "</cross-session-message>";
/// 给模型的机械文本，按 AGENTS §1.5 写英文短句，只陈述是谁发的（用户 09-23 定「只要这一句」）。
pub const CROSS_SESSION_SENDER_NOTE: &str = "Sent by the AI in another session, not by the user.";

/// 拆出来的一条跨会话消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossSessionMessage {
    pub from_name: String,
    pub from_session: String,
    pub body: String,
}

/// 拆外壳；不是跨会话消息（或外壳坏了）返回 None。
pub fn parse_cross_session_message(content: &str) -> Option<CrossSessionMessage> {
    let rest = content
        .trim_start()
        .strip_prefix(CROSS_SESSION_MESSAGE_TAG)?;
    let (from_name, rest) = quoted_attr(rest, "from")?;
    let (from_session, rest) = quoted_attr(rest, "session")?;
    let rest = rest.trim_start().strip_prefix('>')?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let rest = rest
        .strip_prefix(CROSS_SESSION_SENDER_NOTE)
        .map(|rest| rest.strip_prefix('\n').unwrap_or(rest))
        .unwrap_or(rest);
    let body = rest.trim_end();
    let body = body.strip_suffix(CROSS_SESSION_CLOSE_TAG).unwrap_or(body);
    Some(CrossSessionMessage {
        from_name,
        from_session,
        body: body.trim_end_matches('\n').to_string(),
    })
}

/// 会话 id 的短写：末尾那段随机串（`sess_1790180210926_acd86d38` → `acd86d38`）。
/// 完整 id 二十几个字符，界面上放不下（用户 09-24）；没有下划线的 id（`default`）
/// 原样返回。工具也认它（在名单里唯一匹配才算）。
pub fn short_session_id(session_id: &str) -> &str {
    session_id
        .rsplit_once('_')
        .map(|(_, tail)| tail)
        .filter(|tail| !tail.is_empty())
        .unwrap_or(session_id)
}

/// 收到那条消息时的抬头（终端与网页一个说法）：名字加短 id，重名的会话也分得开。
pub fn cross_session_headline(from_name: &str, from_session: &str) -> String {
    miyu_base::i18n::text("Message from {name} ({id})", "从 {name}（{id}）收到消息")
        .replace("{name}", from_name)
        .replace("{id}", short_session_id(from_session))
}

/// ` key="…"`：值按 JSON 字符串的转义规则读（`safe_prompt_field` 就是这么写的），
/// 所以名字里的 `\"` 不会被当成结束引号。
fn quoted_attr<'a>(input: &'a str, key: &str) -> Option<(String, &'a str)> {
    let rest = input.trim_start().strip_prefix(key)?.strip_prefix("=\"")?;
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        match ch {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => {
                let raw = &rest[..index];
                let value = serde_json::from_str::<String>(&format!("\"{raw}\"")).ok()?;
                return Some((value, &rest[index + 1..]));
            }
            _ => escaped = false,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_envelope_and_rejects_everything_else() {
        let content = format!(
            "{CROSS_SESSION_MESSAGE_TAG} from=\"写\\\"代码\\\"\" session=\"s-1\">\n{CROSS_SESSION_SENDER_NOTE}\n第一行\n第二行\n{CROSS_SESSION_CLOSE_TAG}"
        );
        let message = parse_cross_session_message(&content).expect("envelope");
        assert_eq!(message.from_name, "写\"代码\"");
        assert_eq!(message.from_session, "s-1");
        assert_eq!(message.body, "第一行\n第二行");
        assert_eq!(
            parse_cross_session_message("<background-job-report>x"),
            None
        );
        assert_eq!(parse_cross_session_message("普通的一句话"), None);
        assert_eq!(
            parse_cross_session_message("<cross-session-message from=\"缺引号>"),
            None
        );
    }

    #[test]
    fn short_ids_are_the_random_tail() {
        assert_eq!(short_session_id("sess_1790180210926_acd86d38"), "acd86d38");
        assert_eq!(short_session_id("default"), "default");
        assert_eq!(short_session_id("odd_"), "odd_");
    }
}
