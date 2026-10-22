//! 群里的复读:几个人接连发同一句话,只有第一条走正常判断。
//!
//! 10-22 23:44 取证:她重启回来时五个人各发了一句「miyu似了」,四条各判一次、
//! 各回一句、各自引用加 @。每条都是名字开头的直接触发,判官每次都看得出「大家在
//! 刷这句」,却不知道自己刚对同一串判过回。复读由程序认出来,不再交给判官。
//!
//! 认法:正文去掉空白、标点、符号并转小写后相同,而且这一串里有别人说过,串里
//! 相邻两条(首尾相接)都不超过 `REPEAT_GAP_SECONDS`。她自己的消息不算串里的一条。
//! 认出来的只记下、不判断;被复读的那条(串首)如果她回了,发出去时不带引用和 @
//! ——那句话已经不是冲某一个人说的了。

use crate::platforms::plugins::real_context::*;

/// 串里相邻两条的最大间隔。旧群数据:363 串复读,相邻间隔 p50 7s / p95 48s /
/// p99 123s,取 120s 盖住绝大多数又不至于把隔了很久的同一句也并进来。
pub(in crate::platforms::plugins::real_context) const REPEAT_GAP_SECONDS: i64 = 120;

/// 往回最多看这么多条。一串复读不会比这长,长了也只差在认不出更早的串首。
const REPEAT_LOOKBACK_MESSAGES: usize = 40;

/// 被复读的串首登记多久。足够盖住「判官放行 → 生成 → 发送」这一段。
pub(in crate::platforms::plugins::real_context) const REPEATED_HEAD_TTL: Duration =
    Duration::from_secs(600);

/// 比较用的正文:只留字母和数字(含中日韩文字),字母转小写。
pub(in crate::platforms::plugins::real_context) fn repeat_key(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 当前消息是一串复读里的后续一条时,返回串首的消息 id。
///
/// `history` 是当前消息之前的记录,顺序不限。`now` 是当前消息的发送时间(秒)。
pub(in crate::platforms::plugins::real_context) fn repeat_head(
    key: &str,
    sender_id: &str,
    now: i64,
    history: &[HistoryMessage],
) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    let mut older = history
        .iter()
        .filter(|message| !message.is_bot && message.recalled_at.is_none())
        .collect::<Vec<_>>();
    older.sort_by(|a, b| {
        b.sent_at
            .cmp(&a.sent_at)
            .then(b.ingress_order.cmp(&a.ingress_order))
    });
    let mut last = now;
    let mut head = None;
    let mut someone_else = false;
    for message in older {
        // 按时间倒着走:比串里最早那条还早出一个窗口,后面只会更早。
        if last - message.sent_at > REPEAT_GAP_SECONDS {
            break;
        }
        if repeat_key(&message.content.text) == key {
            last = message.sent_at;
            head = Some(message.message_id.clone());
            someone_else |= message.sender_id != sender_id;
        }
    }
    head.filter(|_| someone_else)
}

impl RealContextPlugin {
    /// 当前消息是复读就记下串首、打一行日志并返回 true;调用方据此放弃这一轮。
    /// 终端管理员不受限;纯表情、纯占位符没有可比的正文,不算。
    pub(in crate::platforms::plugins::real_context) async fn skip_repeat(
        &self,
        context: &PlatformTurnContext,
        event: &PlatformInboundEvent,
        settings: &RealContextPluginSettings,
        trigger: TriggerKind,
    ) -> bool {
        if context.is_admin || super::inject::placeholder_only(&event.text) {
            return false;
        }
        let key = repeat_key(&event.text);
        if key.is_empty() {
            return false;
        }
        let Ok(group) = group_key(context) else {
            return false;
        };
        let history = self
            .store(context)
            .recent(
                RecentQuery::for_context(
                    group,
                    context.config.active_persona_scope(),
                    REPEAT_LOOKBACK_MESSAGES,
                )
                .before_ingress_order(event.ingress_order),
            )
            .await;
        let mut history = match history {
            Ok(page) => page.messages,
            Err(error) => {
                tracing::warn!(target: "miyu::qq", error = %error, "{}", miyu_base::i18n::text("repeat check could not read the group history; judging normally", "复读检查读不到群聊记录,照常判断"));
                return false;
            }
        };
        history.retain(|message| message.message_id != event.message_id);
        let now = if event.timestamp > 0 {
            event.timestamp
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs() as i64)
        };
        let Some(head) = repeat_head(&key, &event.sender_id, now, &history) else {
            return false;
        };
        {
            let now = Instant::now();
            let mut runtime = self.runtime.lock().unwrap();
            let session = runtime.session_mut(&runtime_session_key(context), now);
            session
                .repeated_heads
                .retain(|_, at| now.duration_since(*at) <= REPEATED_HEAD_TTL);
            session.repeated_heads.insert(head.clone(), now);
        }
        self.log_skip(
            context,
            settings,
            trigger,
            &format!("复读：同一句话前面已有人说过（串首消息 {head}），只判第一条"),
        );
        true
    }

    /// 这一轮回复的是一条后来被人复读的消息吗。是就不带引用和 @。
    pub(in crate::platforms::plugins::real_context) fn replying_to_repeated(
        &self,
        context: &PlatformTurnContext,
    ) -> bool {
        let mut ids = Vec::new();
        if let Some(target) = context.reserved_response_target_message_id() {
            ids.push(target);
        }
        if let Some(event) = context.inbound_event() {
            ids.push(event.message_id.clone());
        }
        if ids.iter().all(String::is_empty) {
            return false;
        }
        let now = Instant::now();
        let runtime = self.runtime.lock().unwrap();
        runtime
            .sessions
            .get(&runtime_session_key(context))
            .is_some_and(|session| {
                ids.iter().any(|id| {
                    session
                        .repeated_heads
                        .get(id)
                        .is_some_and(|at| now.duration_since(*at) <= REPEATED_HEAD_TTL)
                })
            })
    }
}
