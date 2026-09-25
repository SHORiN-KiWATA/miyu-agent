//! `send_qq_message` / `qq_contacts`:让模型往 QQ 直发消息(不经回合回复)。
//!
//! 口径(用户 09-18 拍板)按**模式**分,不按场所分:
//! - 普通模式:`send_qq_message` 可以发到**任意**好友/群,配 `qq_contacts` 查地址簿。
//!   收件人写名字或号码都行——管理员别名、好友的备注/昵称、群名;对不上就从地址簿
//!   (`QqOutreachPort::directory`)里模糊找,多个候选就把候选连号码一起报回去让
//!   模型挑。
//! - 开发模式:只有 `send_qq_message`,**收件人只能是管理员**(`qq.admin_users`,按
//!   `qq.admin_aliases` 的别名列成枚举),没有地址簿工具——写代码时「跑完把结果发我
//!   手机」是真需求,别的不给。
//!
//! 场所只决定两件事:本地会话(REPL / WebUI / shellhook)看 `platforms.terminal_outreach`
//! 开关;平台会话(QQ 里,所有触发者都有,不分管理员/群友)带当前会话的账号、不看
//! 终端开关,发回当前会话仍是 `send_message_to_user`。
//!
//! 配置里开了 QQ 就恒在工具表里,掉线在调用时拦下、明说没连上(09-25)。原来只在 NapCat
//! 的反向 ws 连上时注册:daemon 一重启、QQ 一重连,工具表就变,所有开着的会话下一次请求
//! 整条缓存前缀作废(真机日志里单次就是 23.7 万 token)。
//!
//! 09-18 之前平台会话没有任何带收件人的发送工具,用户要「发到 xxx 交流群」时她
//! 只能拿 run_command 去打 NapCat 的 HTTP 口(BUG-14)。

use super::{ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use miyu_base::config::{AppConfig, PersonaLane};
use miyu_base::host_ports::{qq_outreach_policy, QqDirectory, QqOutreachPort, QqTarget};
use miyu_base::platform_types::{OutboundMessage, OutboundOrigin, OutboundSegment};
use serde_json::{json, Value};
use std::sync::Arc;

pub const TOOL_NAME: &str = "send_qq_message";
pub const CONTACTS_TOOL_NAME: &str = "qq_contacts";

/// 一次列多少条联系人,再多模型也看不过来,让它带关键词再查。
const CONTACTS_LIMIT: usize = 60;

/// NapCat 的反向 WebSocket 是否已连上(至少一个账号在线)。直发能力经
/// `host_ports::QqOutreachPort` 拿,非 daemon 进程里没装端口即视为未连上。
pub fn qq_connected() -> bool {
    miyu_base::host_ports::qq_outreach_port().is_some_and(|port| port.connected())
}

/// 工具装在哪个场所:决定要不要看终端开关、用哪个账号发。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// 本地会话(REPL / WebUI / shellhook)。
    Terminal,
    /// 平台会话;`account` 是当前会话所在的机器人账号(多号时别串号)。
    Platform { account: Option<i64> },
}

/// 收件人范围:按模式定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// 开发模式:只能发管理员,没有地址簿工具。
    Admins,
    /// 普通模式:任意好友/群 + `qq_contacts`。
    Anyone,
}

impl Reach {
    pub fn for_lane(lane: PersonaLane) -> Self {
        match lane {
            PersonaLane::Dev => Self::Admins,
            PersonaLane::Active => Self::Anyone,
        }
    }
}

/// 本地会话用的注册入口(`builtin_plugins`,不知道车道):先按普通模式装;
/// `build_tool_registry` 在 dev 车道上再用 [`restrict_to_admins`] 收窄。
pub fn register(registry: &mut ToolRegistry, config: &AppConfig) {
    register_in(registry, config, Surface::Terminal, Reach::Anyone);
}

/// 开发模式的收窄:装了普通版就换成只发管理员的那版,地址簿工具摘掉。
/// 没装(配置里没开)就什么都不做——不会凭空多出工具。
pub fn restrict_to_admins(registry: &mut ToolRegistry, config: &AppConfig, surface: Surface) {
    if !registry.contains(TOOL_NAME) {
        return;
    }
    registry.unregister(TOOL_NAME);
    registry.unregister(CONTACTS_TOOL_NAME);
    register_in(registry, config, surface, Reach::Admins);
}

pub fn register_in(
    registry: &mut ToolRegistry,
    config: &AppConfig,
    surface: Surface,
    reach: Reach,
) {
    match reach {
        Reach::Admins => register_admins_only(registry, config, surface),
        Reach::Anyone => register_anyone(registry, config, surface),
    }
}

/// 只发管理员:别名直接列成枚举,没有地址簿。
fn register_admins_only(registry: &mut ToolRegistry, config: &AppConfig, surface: Surface) {
    let list = qq_outreach_policy(config).recipients;
    let labels: Vec<String> = list.iter().map(|(_, label)| label.clone()).collect();
    let primary = labels.first().cloned().unwrap_or_default();
    let mut to_schema = json!({
        "type": "string",
        "description": format!("Recipient (an administrator). Omit to reach the primary administrator ({primary})."),
    });
    if !labels.is_empty() {
        to_schema["enum"] = Value::Array(labels.into_iter().map(Value::String).collect());
    }
    let description = match surface {
        Surface::Terminal => {
            "Send a message to the user's QQ, as text or as a spoken voice message."
        }
        Surface::Platform { .. } => {
            "Send a message to an administrator's QQ (not this conversation; reply here with send_message_to_user instead), as text or as a spoken voice message."
        }
    };
    registry.register(
        ToolSpec::new(
            TOOL_NAME,
            description,
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Message text (spoken text when voice is true)." },
                    "voice": { "type": "boolean", "description": "Send as a voice message instead of text." },
                    "to": to_schema
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            move |arguments| async move { send(arguments, surface, Reach::Admins).await },
        )
        .writes(),
    );
}

/// 任意好友/群:名字或号码,配一个地址簿查询工具。
fn register_anyone(registry: &mut ToolRegistry, config: &AppConfig, surface: Surface) {
    let primary = qq_outreach_policy(config)
        .recipients
        .first()
        .map(|(_, label)| label.clone())
        .unwrap_or_default();
    let to_description = if primary.is_empty() {
        "Recipient: a friend's remark or nickname, a group name, or a QQ/group number. Look names up with qq_contacts if unsure.".to_string()
    } else {
        format!("Recipient: a friend's remark or nickname, a group name, or a QQ/group number. Omit to reach the primary administrator ({primary}). Look names up with qq_contacts if unsure.")
    };
    let description = match surface {
        Surface::Terminal => {
            "Send a message to a QQ friend or group, as text or as a spoken voice message."
        }
        Surface::Platform { .. } => {
            "Send a message to ANOTHER QQ friend or group (not this conversation; reply here with send_message_to_user instead), as text or as a spoken voice message."
        }
    };
    registry.register(
        ToolSpec::new(
            TOOL_NAME,
            description,
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Message text (spoken text when voice is true)." },
                    "to": { "type": "string", "description": to_description },
                    "kind": { "type": "string", "enum": ["friend", "group"], "description": "Whether `to` is a friend or a group. Only needed when a name or number matches both." },
                    "voice": { "type": "boolean", "description": "Send as a voice message instead of text." }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
            move |arguments| async move { send(arguments, surface, Reach::Anyone).await },
        )
        .writes(),
    );
    registry.register(ToolSpec::new(
        CONTACTS_TOOL_NAME,
        "Look up QQ friends and groups the bot can message: returns names with their numbers. Empty query lists everything (capped).",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Part of a remark, nickname, group name, or a number. Omit to list all." }
            },
            "additionalProperties": false
        }),
        move |arguments| async move { contacts(arguments, surface).await },
    ));
}

fn port() -> Result<Arc<dyn QqOutreachPort>> {
    miyu_base::host_ports::qq_outreach_port()
        .context("send_qq_message only works inside the daemon")
}

/// 连着的端口。工具恒在工具表里,掉线在这儿拦(模块头)。
fn connected_port(unavailable: &str) -> Result<Arc<dyn QqOutreachPort>> {
    let port = port()?;
    if !port.connected() {
        bail!("QQ is not connected right now (NapCat is offline); {unavailable}");
    }
    Ok(port)
}

fn account_of(surface: Surface) -> Option<i64> {
    match surface {
        Surface::Terminal => None,
        Surface::Platform { account } => account,
    }
}

async fn send(arguments: Value, surface: Surface, reach: Reach) -> Result<String> {
    let text = arguments
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if text.is_empty() {
        bail!("text is required");
    }
    let voice = arguments
        .get("voice")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let port = connected_port("nothing was sent")?;
    // 策略按当前配置现算(不是注册那一刻的):配置重载后立刻生效。
    let policy = port.policy();
    if surface == Surface::Terminal && !policy.allowed {
        bail!("sending to messaging platforms from the terminal is disabled in settings");
    }
    let to = arguments
        .get("to")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let kind = arguments
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty());
    let account = account_of(surface);
    let target = if to.is_empty() {
        let Some((primary, _)) = policy.recipients.first() else {
            bail!("no recipient given and no administrator QQ id is configured (接入通讯平台 → 管理员 QQ 号)");
        };
        QqTarget::Friend(*primary)
    } else {
        match reach {
            // 开发模式:只认管理员的别名/号码,对不上就报允许的名单。
            Reach::Admins => resolve_admin(to, &policy.recipients)?,
            // 普通模式:先只用管理员别名试(不打 API);对不上再拉地址簿。
            Reach::Anyone => {
                match resolve_recipient(to, kind, &policy.recipients, &QqDirectory::default()) {
                    Ok(target) => target,
                    Err(_) => {
                        let directory = port.directory(account).await?;
                        resolve_recipient(to, kind, &policy.recipients, &directory)?
                    }
                }
            }
        }
    };
    let kind = if voice {
        let path = miyu_base::host_ports::voice_port()
            .context("send_qq_message only works inside the daemon")?
            .synthesize_file(text.clone())
            .await?;
        let outcome = port
            .send_to(
                account,
                target,
                OutboundMessage::segments(
                    OutboundOrigin::Tool,
                    vec![OutboundSegment::AudioPath {
                        path: path.clone(),
                        transcript: text.clone(),
                    }],
                ),
            )
            .await;
        let _ = std::fs::remove_file(&path);
        outcome?;
        "voice"
    } else {
        // 来源沿用旧 `send_direct_text` 的 `Plugin`(定时消息同一条路),不改语义。
        port.send_to(
            account,
            target,
            OutboundMessage::text(OutboundOrigin::Plugin, &text),
        )
        .await?;
        "text"
    };
    Ok(
        json!({ "ok": true, "kind": kind, "to": target.id(), "to_kind": target.kind() })
            .to_string(),
    )
}

async fn contacts(arguments: Value, surface: Surface) -> Result<String> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let port = connected_port("the contact list is unavailable")?;
    let policy = port.policy();
    let directory = port.directory(account_of(surface)).await?;
    Ok(render_contacts(&query, &policy.recipients, &directory).to_string())
}

/// 只发管理员那版的收件人:管理员的别名或号码。
fn resolve_admin(to: &str, admins: &[(i64, String)]) -> Result<QqTarget> {
    admins
        .iter()
        .find(|(id, label)| label == to || id.to_string() == to)
        .map(|(id, _)| QqTarget::Friend(*id))
        .with_context(|| {
            let allowed: Vec<&str> = admins.iter().map(|(_, label)| label.as_str()).collect();
            format!("unknown recipient {to}; in dev mode only administrators can be reached: {allowed:?}")
        })
}

/// 地址簿查询结果:按关键词(备注/昵称/群名/号码子串,不分大小写)过滤,空
/// 关键词列全部;各自封顶,超出的报总数让模型带关键词再查。
fn render_contacts(query: &str, admins: &[(i64, String)], directory: &QqDirectory) -> Value {
    let needle = query.to_lowercase();
    let hit = |fields: &[&str], id: i64| {
        needle.is_empty()
            || fields
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
            || id.to_string().contains(&needle)
    };
    let admin_alias = |id: i64| {
        admins
            .iter()
            .find(|(admin, _)| *admin == id)
            .map(|(_, label)| label.clone())
            .filter(|label| label != &id.to_string())
    };
    let friends: Vec<Value> = directory
        .friends
        .iter()
        .filter(|friend| hit(&[&friend.nickname, &friend.remark], friend.user_id))
        .map(|friend| {
            let mut entry = json!({ "user_id": friend.user_id, "nickname": friend.nickname });
            if !friend.remark.is_empty() {
                entry["remark"] = Value::String(friend.remark.clone());
            }
            if let Some(alias) = admin_alias(friend.user_id) {
                entry["alias"] = Value::String(alias);
            }
            entry
        })
        .collect();
    let groups: Vec<Value> = directory
        .groups
        .iter()
        .filter(|group| hit(&[&group.name], group.group_id))
        .map(|group| {
            json!({ "group_id": group.group_id, "name": group.name, "member_count": group.member_count })
        })
        .collect();
    let friends_total = friends.len();
    let groups_total = groups.len();
    let mut out = json!({
        "friends": friends.into_iter().take(CONTACTS_LIMIT).collect::<Vec<_>>(),
        "groups": groups.into_iter().take(CONTACTS_LIMIT).collect::<Vec<_>>(),
    });
    if friends_total > CONTACTS_LIMIT || groups_total > CONTACTS_LIMIT {
        out["truncated"] = json!({ "friends_total": friends_total, "groups_total": groups_total, "hint": "narrow the query" });
    }
    if friends_total == 0 && groups_total == 0 && !query.is_empty() {
        out["hint"] =
            Value::String("nothing matched; try a shorter keyword or the number".to_string());
    }
    out
}

/// 把 `to` 翻成收件方(普通模式)。顺序:号码直给 → 管理员别名 → 精确名字 → 子串;
/// 每一步都可能撞出多个候选,那就连号码一起报回去让模型挑,不替它猜。
fn resolve_recipient(
    to: &str,
    kind: Option<&str>,
    admins: &[(i64, String)],
    directory: &QqDirectory,
) -> Result<QqTarget> {
    let kind = match kind {
        Some("friend") | Some("private") | Some("user") => Some("friend"),
        Some("group") => Some("group"),
        Some(other) => bail!("unknown kind {other}; use \"friend\" or \"group\""),
        None => None,
    };
    if let Ok(id) = to.parse::<i64>() {
        let is_group = directory.groups.iter().any(|group| group.group_id == id);
        let is_friend = admins.iter().any(|(admin, _)| *admin == id)
            || directory.friends.iter().any(|friend| friend.user_id == id);
        return match (kind, is_friend, is_group) {
            (Some("group"), _, _) => Ok(QqTarget::Group(id)),
            (Some(_), _, _) => Ok(QqTarget::Friend(id)),
            (None, true, false) => Ok(QqTarget::Friend(id)),
            (None, false, true) => Ok(QqTarget::Group(id)),
            (None, true, true) => bail!("{id} is both a friend and a group; pass kind"),
            // 号码在地址簿里对不上:不替它猜是人是群。给了 kind 就照发(临时
            // 会话、刚加的好友都可能不在列表里),没给就让模型说清楚。
            (None, false, false) => bail!(
                "{id} is neither a friend nor a group the bot is in; pass kind (\"friend\" or \"group\") to send anyway"
            ),
        };
    }
    let needle = to.to_lowercase();
    let mut exact: Vec<(QqTarget, String)> = Vec::new();
    let mut partial: Vec<(QqTarget, String)> = Vec::new();
    let mut consider = |target: QqTarget, name: &str| {
        if name.is_empty() {
            return;
        }
        let lower = name.to_lowercase();
        if lower == needle {
            exact.push((target, name.to_string()));
        } else if lower.contains(&needle) {
            partial.push((target, name.to_string()));
        }
    };
    if kind != Some("group") {
        for (id, label) in admins {
            consider(QqTarget::Friend(*id), label);
        }
        for friend in &directory.friends {
            consider(QqTarget::Friend(friend.user_id), &friend.remark);
            consider(QqTarget::Friend(friend.user_id), &friend.nickname);
        }
    }
    if kind != Some("friend") {
        for group in &directory.groups {
            consider(QqTarget::Group(group.group_id), &group.name);
        }
    }
    for candidates in [exact, partial] {
        let mut unique: Vec<(QqTarget, String)> = Vec::new();
        for (target, name) in candidates {
            if !unique.iter().any(|(seen, _)| *seen == target) {
                unique.push((target, name));
            }
        }
        match unique.len() {
            0 => continue,
            1 => return Ok(unique[0].0),
            _ => {
                let listed: Vec<String> = unique
                    .iter()
                    .map(|(target, name)| format!("{name} ({} {})", target.kind(), target.id()))
                    .collect();
                bail!(
                    "{to} matches several recipients: {}; pass the number",
                    listed.join(", ")
                );
            }
        }
    }
    if directory.friends.is_empty() && directory.groups.is_empty() {
        bail!("unknown recipient {to}");
    }
    bail!("unknown recipient {to}; look it up with qq_contacts or pass the number")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use miyu_base::host_ports::{QqContact, QqGroup, QqOutreachPolicy};
    use miyu_base::platform_types::OutboundBody;
    use std::sync::Mutex;

    fn directory() -> QqDirectory {
        QqDirectory {
            friends: vec![
                QqContact {
                    user_id: 10001,
                    nickname: "小明".into(),
                    remark: "同事小明".into(),
                },
                QqContact {
                    user_id: 10003,
                    nickname: "阿花".into(),
                    remark: String::new(),
                },
                QqContact {
                    user_id: 10004,
                    nickname: "花花".into(),
                    remark: String::new(),
                },
            ],
            groups: vec![
                QqGroup {
                    group_id: 20001,
                    name: "Miyu 交流群".into(),
                    member_count: 42,
                },
                QqGroup {
                    group_id: 20002,
                    name: "家庭群".into(),
                    member_count: 5,
                },
                QqGroup {
                    group_id: 10003,
                    name: "撞号的群".into(),
                    member_count: 2,
                },
            ],
        }
    }

    fn admins() -> Vec<(i64, String)> {
        vec![(10001, "10001".to_string()), (10002, "老板".to_string())]
    }

    /// 普通模式的收件人解析:别名/备注/昵称/群名精确优先,再子串;撞多个报候选;
    /// 号码按地址簿判人群,两边都撞或都没有就要 kind。开发模式只认管理员。
    #[test]
    fn recipients_resolve_by_alias_name_number_and_ask_when_ambiguous() {
        let dir = directory();
        let admins = admins();
        let resolve = |to: &str, kind: Option<&str>| resolve_recipient(to, kind, &admins, &dir);
        assert_eq!(resolve("老板", None).unwrap(), QqTarget::Friend(10002));
        assert_eq!(resolve("同事小明", None).unwrap(), QqTarget::Friend(10001));
        assert_eq!(
            resolve("miyu 交流群", None).unwrap(),
            QqTarget::Group(20001)
        );
        assert_eq!(resolve("交流群", None).unwrap(), QqTarget::Group(20001));
        assert_eq!(resolve("家庭", None).unwrap(), QqTarget::Group(20002));
        // 「花」撞到阿花与花花:报候选,不猜。
        let error = resolve("花", None).unwrap_err().to_string();
        assert!(
            error.contains("several") && error.contains("10003") && error.contains("10004"),
            "{error}"
        );
        // 精确命中优先于子串:「阿花」只会是阿花。
        assert_eq!(resolve("阿花", None).unwrap(), QqTarget::Friend(10003));
        // kind 限定范围。
        assert_eq!(
            resolve("撞号的群", Some("group")).unwrap(),
            QqTarget::Group(10003)
        );
        assert!(resolve("撞号的群", Some("friend")).is_err());
        // 号码:群号直给、好友号直给、撞号要 kind、不认识的号也要 kind。
        assert_eq!(resolve("20002", None).unwrap(), QqTarget::Group(20002));
        assert_eq!(resolve("10001", None).unwrap(), QqTarget::Friend(10001));
        assert!(resolve("10003", None)
            .unwrap_err()
            .to_string()
            .contains("both"));
        assert_eq!(
            resolve("10003", Some("group")).unwrap(),
            QqTarget::Group(10003)
        );
        assert!(resolve("30000", None)
            .unwrap_err()
            .to_string()
            .contains("pass kind"));
        assert_eq!(
            resolve("30000", Some("friend")).unwrap(),
            QqTarget::Friend(30000)
        );
        assert!(resolve("不存在的人", None)
            .unwrap_err()
            .to_string()
            .contains("qq_contacts"));
        assert!(resolve("x", Some("bogus")).is_err());
        // 空地址簿只认管理员别名。
        assert_eq!(
            resolve_recipient("老板", None, &admins, &QqDirectory::default()).unwrap(),
            QqTarget::Friend(10002)
        );
        assert!(resolve_recipient("交流群", None, &admins, &QqDirectory::default()).is_err());
        // 开发模式只认管理员。
        assert_eq!(
            resolve_admin("老板", &admins).unwrap(),
            QqTarget::Friend(10002)
        );
        assert_eq!(
            resolve_admin("10001", &admins).unwrap(),
            QqTarget::Friend(10001)
        );
        let error = resolve_admin("交流群", &admins).unwrap_err().to_string();
        assert!(error.contains("only administrators"), "{error}");
    }

    /// 地址簿查询:关键词过滤备注/昵称/群名/号码,管理员别名带上,空关键词列全。
    #[test]
    fn contacts_filter_by_keyword_and_carry_admin_aliases() {
        let out = render_contacts("花", &admins(), &directory());
        let friends = out["friends"].as_array().unwrap();
        assert_eq!(friends.len(), 2);
        assert!(out["groups"].as_array().unwrap().is_empty());
        let out = render_contacts("", &admins(), &directory());
        assert_eq!(out["friends"].as_array().unwrap().len(), 3);
        assert_eq!(out["groups"].as_array().unwrap().len(), 3);
        assert_eq!(out["friends"][0]["remark"], "同事小明");
        let out = render_contacts("2000", &admins(), &directory());
        assert_eq!(out["groups"].as_array().unwrap().len(), 2);
        let out = render_contacts("没有这个", &admins(), &directory());
        assert!(out["hint"].as_str().unwrap().contains("nothing matched"));
    }

    fn config_with_admins() -> AppConfig {
        let mut config = AppConfig::default();
        config.platforms.qq.admin_users = vec![10001, 10002];
        config
            .platforms
            .qq
            .admin_aliases
            .insert("10002".to_string(), "老板".to_string());
        config
    }

    /// 两种模式的工具面:开发模式 `to` 是管理员别名枚举、没有地址簿工具;普通模式
    /// `to` 自由填、带 `kind`,多一个 `qq_contacts`。`restrict_to_admins` 把普通版收成
    /// 开发版,没装的时候不会凭空多出来。
    #[test]
    fn reach_decides_the_face() {
        let config = config_with_admins();
        let mut dev = ToolRegistry::new();
        register_in(&mut dev, &config, Surface::Terminal, Reach::Admins);
        assert!(dev.contains(TOOL_NAME));
        assert!(!dev.contains(CONTACTS_TOOL_NAME));
        let schema = dev.get(TOOL_NAME).unwrap().parameters.clone();
        assert_eq!(schema["properties"]["to"]["enum"], json!(["10001", "老板"]));
        assert!(schema["properties"].get("kind").is_none());

        let mut normal = ToolRegistry::new();
        register_in(
            &mut normal,
            &config,
            Surface::Platform { account: Some(900) },
            Reach::Anyone,
        );
        assert!(normal.contains(TOOL_NAME));
        assert!(normal.contains(CONTACTS_TOOL_NAME));
        let schema = normal.get(TOOL_NAME).unwrap().parameters.clone();
        assert!(schema["properties"]["to"].get("enum").is_none());
        assert_eq!(
            schema["properties"]["kind"]["enum"],
            json!(["friend", "group"])
        );

        restrict_to_admins(&mut normal, &config, Surface::Terminal);
        assert!(normal.contains(TOOL_NAME));
        assert!(!normal.contains(CONTACTS_TOOL_NAME));
        let schema = normal.get(TOOL_NAME).unwrap().parameters.clone();
        assert!(schema["properties"]["to"].get("enum").is_some());

        let mut empty = ToolRegistry::new();
        restrict_to_admins(&mut empty, &config, Surface::Terminal);
        assert!(!empty.contains(TOOL_NAME), "没连 ws 时不会凭空多出工具");
        assert_eq!(Reach::for_lane(PersonaLane::Dev), Reach::Admins);
        assert_eq!(Reach::for_lane(PersonaLane::Active), Reach::Anyone);
    }

    struct RecordingPort {
        policy: QqOutreachPolicy,
        directory: QqDirectory,
        sent: Arc<Mutex<Vec<(Option<i64>, QqTarget, OutboundMessage)>>>,
        connected: bool,
    }

    impl QqOutreachPort for RecordingPort {
        fn connected(&self) -> bool {
            self.connected
        }

        fn policy(&self) -> QqOutreachPolicy {
            self.policy.clone()
        }

        fn send_to(
            &self,
            account: Option<i64>,
            target: QqTarget,
            message: OutboundMessage,
        ) -> BoxFuture<'static, Result<()>> {
            let sent = self.sent.clone();
            Box::pin(async move {
                sent.lock().unwrap().push((account, target, message));
                Ok(())
            })
        }

        fn directory(&self, _account: Option<i64>) -> BoxFuture<'static, Result<QqDirectory>> {
            let directory = self.directory.clone();
            Box::pin(async move { Ok(directory) })
        }
    }

    fn install(allowed: bool, sent: &Arc<Mutex<Vec<(Option<i64>, QqTarget, OutboundMessage)>>>) {
        install_port(allowed, true, sent);
    }

    fn install_port(
        allowed: bool,
        connected: bool,
        sent: &Arc<Mutex<Vec<(Option<i64>, QqTarget, OutboundMessage)>>>,
    ) {
        miyu_base::host_ports::install_qq_outreach_port(Arc::new(RecordingPort {
            policy: QqOutreachPolicy {
                allowed,
                recipients: admins(),
            },
            directory: directory(),
            sent: sent.clone(),
            connected,
        }));
    }

    /// 工具只认 `host_ports::QqOutreachPort`:没装端口(非 daemon 进程)报原来的错;
    /// 终端场所看 `terminal_outreach` 开关,平台场所不看;开发模式只能发管理员
    /// (地址簿里有的群也不行),普通模式群名解析到群号、平台场所带着当前账号发;
    /// 不传 `to` 发主管理员。
    #[tokio::test]
    async fn send_routes_through_the_outreach_port() {
        let platform = Surface::Platform { account: Some(900) };
        let error = send(json!({ "text": "hi" }), Surface::Terminal, Reach::Anyone)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "send_qq_message only works inside the daemon"
        );

        let sent = Arc::new(Mutex::new(Vec::new()));
        install(false, &sent);
        let error = send(json!({ "text": "hi" }), Surface::Terminal, Reach::Anyone)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("disabled in settings"),
            "{error}"
        );
        // 平台场所不受终端开关管;普通模式任意群都能发。
        send(
            json!({ "text": "群里见", "to": "交流群" }),
            platform,
            Reach::Anyone,
        )
        .await
        .unwrap();
        // 开发模式:地址簿里有的群也不给发,只认管理员。
        let error = send(
            json!({ "text": "x", "to": "交流群" }),
            platform,
            Reach::Admins,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("only administrators"), "{error}");

        install(true, &sent);
        let output = send(
            json!({ "text": " 开饭了 ", "to": "老板" }),
            Surface::Terminal,
            Reach::Admins,
        )
        .await
        .unwrap();
        assert!(output.contains("\"to\":10002"), "{output}");
        // 终端 + 普通模式:也能发到群。
        send(
            json!({ "text": "终端发群", "to": "家庭群" }),
            Surface::Terminal,
            Reach::Anyone,
        )
        .await
        .unwrap();
        let error = send(
            json!({ "text": "x", "to": "路人" }),
            Surface::Terminal,
            Reach::Anyone,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("unknown recipient"), "{error}");
        send(json!({ "text": "默认" }), Surface::Terminal, Reach::Admins)
            .await
            .unwrap();
        assert!(
            send(json!({ "text": "  " }), Surface::Terminal, Reach::Anyone)
                .await
                .is_err()
        );

        // 掉线时（09-25 起工具恒在工具表里，好让缓存前缀不随连接变）：说清楚没连上，一条都不发。
        install_port(true, false, &sent);
        for (surface, reach) in [
            (Surface::Terminal, Reach::Admins),
            (platform, Reach::Anyone),
        ] {
            let error = send(json!({ "text": "x", "to": "老板" }), surface, reach)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("QQ is not connected"), "{error}");
        }
        let error = contacts(json!({}), platform).await.unwrap_err();
        assert!(error.to_string().contains("QQ is not connected"), "{error}");

        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 4);
        assert_eq!(sent[0].0, Some(900));
        assert_eq!(sent[0].1, QqTarget::Group(20001));
        assert_eq!(sent[1].1, QqTarget::Friend(10002));
        assert!(matches!(sent[1].2.origin, OutboundOrigin::Plugin));
        assert!(matches!(
            &sent[1].2.body,
            OutboundBody::Segments(segments)
                if matches!(segments.as_slice(), [OutboundSegment::Text(text)] if text == "开饭了")
        ));
        assert_eq!(sent[2].0, None);
        assert_eq!(sent[2].1, QqTarget::Group(20002));
        assert_eq!(sent[3].1, QqTarget::Friend(10001));
    }
}
