//! 脚本头部元数据:紧跟 shebang 的注释块。
//!
//! 头部是脚本自带的真相源(09-05):id/显示名/描述/超时/分组/argv/参数 schema
//! 都能写在文件开头的注释里,脚本单文件即可分发;`index.json` 退为覆盖层,
//! 只放「手写覆盖」和 disabled 名单。
//!
//! 解析规则:第一行 shebang;之后逐行看,注释行(`#` 或 `//` 起头)取注释体,
//! `键: 值` 命中已知键就收下,**不认识的注释行跳过而不是终止**——此前遇到
//! `# -*- coding: utf-8 -*-` 就断,内置五个 python 脚本的头部因此一直是死的,
//! 全靠 index.json 兜着。第一个非空、非注释行(代码、docstring)结束头部。
//! 只读文件前 32KB:头部一定在开头,不必为一个 120KB 的脚本读全文。

use super::*;

pub(crate) const HEADER_READ_LIMIT: usize = 32 * 1024;

/// 参数除 stdin JSON 外是否还展开成 argv。`flags` 把 `{"query":"x","limit":5}`
/// 展成 `--query=x --limit=5`,脚本用 argparse/getopt 就够,不必手写 stdin 层。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ArgvMode {
    #[default]
    #[serde(rename = "none")]
    Off,
    #[serde(rename = "flags")]
    Flags,
}

impl ArgvMode {
    pub(crate) fn is_off(&self) -> bool {
        *self == ArgvMode::Off
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "flags" => Some(ArgvMode::Flags),
            "none" | "off" => Some(ArgvMode::Off),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            ArgvMode::Off => "none",
            ArgvMode::Flags => "flags",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScriptDescriptions {
    pub(crate) zh: Option<String>,
    pub(crate) en: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScriptDisplayNames {
    pub(crate) zh: Option<String>,
    pub(crate) en: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScriptMetadata {
    pub(crate) descriptions: ScriptDescriptions,
    pub(crate) display_names: ScriptDisplayNames,
    pub(crate) id: Option<String>,
    pub(crate) timeout_seconds: Option<u64>,
    pub(crate) groups: Vec<String>,
    pub(crate) argv: Option<ArgvMode>,
    pub(crate) parameters: Option<Value>,
    /// `Trust: external` = 也给不可信场所(QQ 群等);缺省只给属主。
    pub(crate) trust: Option<ToolTrust>,
    /// `Permission: read-only|presentation|writes`;缺省 writes(脚本会跑命令)。
    pub(crate) permission: Option<ToolPermission>,
    /// `Example: {"query":"x"}`:stub 模式下附在桩上的一行调用示例。
    pub(crate) stub_example: Option<String>,
    /// `Hint: <tool>: <sentence>`:被指工具在场时把句子追加到本脚本描述末尾。
    pub(crate) hints: Vec<(String, String)>,
    /// `Requires: tool_a, tool_b`:本回合先调用过其中之一才放行。
    pub(crate) requires: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderKey {
    Id,
    DescriptionEn,
    DescriptionZh,
    DisplayNameEn,
    DisplayNameZh,
    Timeout,
    Groups,
    Argv,
    Parameters,
    Trust,
    Permission,
    Example,
    Hint,
    Requires,
}

/// 读脚本开头(最多 32KB),UTF-8 边界截断按 lossy 处理——头部在前,截在
/// 末尾的半个字符不影响解析。
pub(crate) fn read_header(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut buffer = Vec::new();
    file.take(HEADER_READ_LIMIT as u64)
        .read_to_end(&mut buffer)
        .ok()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

pub(crate) fn metadata_from_script(path: &Path) -> ScriptMetadata {
    read_header(path)
        .map(|raw| extract_metadata(&raw))
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn extract_description(raw: &str) -> Option<String> {
    select_script_description(&extract_metadata(raw).descriptions)
}

#[cfg(test)]
pub(crate) fn description_from_script(path: &Path) -> Option<String> {
    select_script_description(&metadata_from_script(path).descriptions)
}

pub(crate) fn select_script_description(descriptions: &ScriptDescriptions) -> Option<String> {
    // 模型面恒英文:英文描述优先;用户脚本只写了中文时原样保留(用户内容)。
    descriptions
        .en
        .as_ref()
        .or(descriptions.zh.as_ref())
        .cloned()
}

pub(crate) fn select_script_display_name(display_names: &ScriptDisplayNames) -> Option<String> {
    select_display_name_for(locale(), display_names)
}

/// 英文界面拿不到英文名时**不回退中文**:内建工具的可读名是双语硬编码表
/// (`builtin_readable_tool_name`),英文 UI 上它们全是人话;脚本这边一回退,
/// 同一排里就会蹦出「小红书搜索」。缺英文名时调用方改用
/// [`humanize_script_id`] 从 id 兜一个。中文界面仍吃英文名兜底——中文 UI 上
/// 一个英文名远好过一个裸 id。
pub(crate) fn select_display_name_for(
    locale: Locale,
    display_names: &ScriptDisplayNames,
) -> Option<String> {
    match locale {
        Locale::Zh => display_names.zh.as_ref().or(display_names.en.as_ref()),
        Locale::En => display_names.en.as_ref(),
    }
    .cloned()
}

/// 没写英文名的脚本在英文界面上的兜底名:`xhs_search` → `Xhs search`。
/// 用户自己写的脚本几乎不会写英文名(本机三个自建脚本全是只写中文),兜底
/// 得保证英文 UI 至少拿到一个 ASCII 名字;内置脚本仍该手写 `Display name:`,
/// 因为 `procusage`、`showenv` 这类 id 折出来的英文不达意。
pub(crate) fn humanize_script_id(id: &str) -> String {
    let mut humanized = String::with_capacity(id.len());
    let mut capitalize = true;
    for character in id.chars() {
        if character == '_' || character == '-' {
            if !humanized.is_empty() {
                humanized.push(' ');
            }
            continue;
        }
        if capitalize {
            humanized.extend(character.to_uppercase());
            capitalize = false;
        } else {
            humanized.push(character);
        }
    }
    if humanized.is_empty() {
        id.to_string()
    } else {
        humanized
    }
}

/// 注释体:`#`/`//` 起头的行去掉前缀与两侧空白;其它行返回 None。
fn comment_body(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('#') {
        return Some(rest.trim_start_matches('#').trim());
    }
    if let Some(rest) = trimmed.strip_prefix("//") {
        return Some(rest.trim_start_matches('/').trim());
    }
    None
}

pub(crate) fn extract_metadata(raw: &str) -> ScriptMetadata {
    let mut metadata = ScriptMetadata::default();
    let mut lines = raw.lines().peekable();
    if lines.peek().is_some_and(|line| line.starts_with("#!")) {
        lines.next();
    }
    // `Parameters:` 后的多行 JSON 块:逐行累加,凑成合法 JSON 即收下。
    let mut pending_json: Option<String> = None;
    for line in lines {
        let Some(body) = comment_body(line) else {
            if line.trim().is_empty() {
                continue;
            }
            break;
        };
        if let Some(block) = pending_json.as_mut() {
            block.push('\n');
            block.push_str(body);
            if let Ok(value) = serde_json::from_str::<Value>(block) {
                metadata.parameters = Some(value);
                pending_json = None;
            }
            continue;
        }
        if body.is_empty() {
            continue;
        }
        let Some((key, value)) = split_header_line(body) else {
            continue;
        };
        match key {
            HeaderKey::Parameters => {
                if value.is_empty() {
                    pending_json = Some(String::new());
                } else if let Ok(parsed) = serde_json::from_str::<Value>(value) {
                    metadata.parameters = Some(parsed);
                } else {
                    pending_json = Some(value.to_string());
                }
            }
            _ if value.is_empty() => {}
            HeaderKey::Id => metadata.id = Some(value.to_string()),
            HeaderKey::DescriptionEn => metadata.descriptions.en = Some(value.to_string()),
            HeaderKey::DescriptionZh => metadata.descriptions.zh = Some(value.to_string()),
            HeaderKey::DisplayNameEn => metadata.display_names.en = Some(value.to_string()),
            HeaderKey::DisplayNameZh => metadata.display_names.zh = Some(value.to_string()),
            HeaderKey::Timeout => metadata.timeout_seconds = parse_timeout(value),
            HeaderKey::Groups => metadata.groups = split_groups(value),
            HeaderKey::Argv => metadata.argv = ArgvMode::parse(value),
            HeaderKey::Trust => metadata.trust = ToolTrust::parse(value),
            HeaderKey::Permission => metadata.permission = ToolPermission::parse(value),
            HeaderKey::Example => metadata.stub_example = Some(value.to_string()),
            HeaderKey::Hint => {
                if let Some(hint) = split_hint(value) {
                    metadata.hints.push(hint);
                }
            }
            HeaderKey::Requires => metadata.requires = split_groups(value),
        }
    }
    if let Some(parameters) = metadata.parameters.as_mut() {
        sanitize_script_parameters(metadata.id.as_deref(), parameters);
    }
    metadata
}

pub(crate) fn sanitize_script_parameters(id: Option<&str>, schema: &mut Value) {
    let script_id = id.unwrap_or("<unknown>");
    sanitize_schema_recursively(script_id, schema);
}

fn sanitize_schema_recursively(script_id: &str, value: &mut Value) {
    match value {
        Value::Object(map) => {
            let expected_type = map.get("type").and_then(Value::as_str).map(|s| s.to_string());
            if let Some(enum_val) = map.get_mut("enum") {
                if let Value::Array(items) = enum_val {
                    let orig_len = items.len();
                    items.retain(|item| {
                        let ok = match expected_type.as_deref() {
                            Some("string") => item.as_str().is_some_and(|s| !s.is_empty()),
                            Some("integer") => item.as_i64().is_some() || item.as_u64().is_some(),
                            Some("number") => item.as_f64().is_some(),
                            Some("boolean") => item.is_boolean(),
                            _ => match item {
                                Value::String(s) => !s.is_empty(),
                                Value::Null => false,
                                _ => true,
                            },
                        };
                        if !ok {
                            tracing::warn!(
                                script = script_id,
                                bad_value = ?item,
                                expected_type = ?expected_type,
                                "script schema contains invalid or empty enum entry; dropped"
                            );
                        }
                        ok
                    });
                    if items.len() < orig_len {
                        tracing::warn!(
                            script = script_id,
                            "sanitized enum in script parameter schema"
                        );
                    }
                } else {
                    tracing::warn!(
                        script = script_id,
                        "script schema 'enum' is not an array; dropping"
                    );
                    map.remove("enum");
                }
                if map
                    .get("enum")
                    .and_then(Value::as_array)
                    .is_some_and(|arr| arr.is_empty())
                {
                    map.remove("enum");
                }
            }
            if let (Some(default_val), Some(Value::Array(enum_items))) =
                (map.get("default"), map.get("enum"))
            {
                if !enum_items.contains(default_val) {
                    tracing::warn!(
                        script = script_id,
                        default = ?default_val,
                        "script schema 'default' is not in 'enum'"
                    );
                }
            }
            for child in map.values_mut() {
                sanitize_schema_recursively(script_id, child);
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_schema_recursively(script_id, item);
            }
        }
        _ => {}
    }
}

/// 在最先出现的半角或全角冒号处切开——此前先找半角再找全角,
/// `描述：走 stdin: 喂 JSON` 这类值里带半角冒号的中文行会被切错键。
fn split_header_line(line: &str) -> Option<(HeaderKey, &str)> {
    let half = line.find(':');
    let full = line.find('：');
    let (index, width) = match (half, full) {
        (Some(h), Some(f)) if f < h => (f, '：'.len_utf8()),
        (Some(h), _) => (h, 1),
        (None, Some(f)) => (f, '：'.len_utf8()),
        (None, None) => return None,
    };
    let key = header_key(line[..index].trim())?;
    Some((key, line[index + width..].trim()))
}

fn header_key(raw: &str) -> Option<HeaderKey> {
    let normalized = raw.to_ascii_lowercase().replace([' ', '-'], "_");
    Some(match normalized.as_str() {
        "id" | "tool_id" => HeaderKey::Id,
        "description" => HeaderKey::DescriptionEn,
        "描述" | "功能介绍" => HeaderKey::DescriptionZh,
        "display_name" => HeaderKey::DisplayNameEn,
        "显示名称" | "工具名称" => HeaderKey::DisplayNameZh,
        "timeout" | "timeout_seconds" | "超时" => HeaderKey::Timeout,
        "group" | "groups" | "分组" => HeaderKey::Groups,
        "argv" => HeaderKey::Argv,
        "parameters" | "params" | "schema" | "参数" => HeaderKey::Parameters,
        "trust" | "信任" | "可见范围" => HeaderKey::Trust,
        "permission" | "权限" => HeaderKey::Permission,
        "example" | "stub_example" | "示例" => HeaderKey::Example,
        "hint" | "cross_hint" | "指路" | "指路句" => HeaderKey::Hint,
        "requires" | "requires_prior" | "需先调用" | "前置工具" => HeaderKey::Requires,
        _ => return None,
    })
}

fn parse_timeout(value: &str) -> Option<u64> {
    let digits = value
        .trim()
        .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '秒')
        .trim();
    digits.parse::<u64>().ok().filter(|secs| *secs > 0)
}

/// `Hint: read: Prefix a path with kb: …` → (被指工具, 句子)。句子自带前导空格,
/// 它是接在描述末尾的。
fn split_hint(value: &str) -> Option<(String, String)> {
    let (tool, sentence) = value.split_once([':', '：'])?;
    let tool = tool.trim();
    let sentence = sentence.trim();
    if tool.is_empty() || sentence.is_empty() {
        return None;
    }
    Some((tool.to_string(), format!(" {sentence}")))
}

fn split_groups(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == ',' || c == '，' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_string)
        .collect()
}

/// 文件名 stem → 工具名:非字母数字一律折成 `_`,连续折叠,首字符非字母时
/// 加 `script_` 前缀;一个字母数字都没有(纯中文文件名)返回 None。
/// 此前自动检测直接拿 stem 当 id,`battery-care` 这种带连字符的名字进了工具
/// 面,而 manage_script 又只放行 `^[a-zA-Z][a-zA-Z0-9_]*$`,两套规则打架。
pub(crate) fn normalize_script_id(stem: &str) -> Option<String> {
    let mut id = String::new();
    let mut pending_underscore = false;
    for character in stem.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_underscore && !id.is_empty() {
                id.push('_');
            }
            id.push(character);
            pending_underscore = false;
        } else {
            pending_underscore = true;
        }
    }
    if id.is_empty() {
        return None;
    }
    if !id.chars().next()?.is_ascii_alphabetic() {
        id.insert_str(0, "script_");
    }
    Some(id)
}
