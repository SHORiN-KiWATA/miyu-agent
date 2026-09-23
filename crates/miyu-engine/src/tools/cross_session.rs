//! `send_to_other_running_session`（09-23）：给同一个人别的开着的会话里的 AI 发话。
//!
//! 只给本地会话（终端 / 网页的普通与开发模式）：`compose_registry` 在子代理快照之后
//! 才挂它，会话化的子代理由 [`super::SUBAGENT_SESSION_EXCLUDED`] 摘掉，平台回合由
//! 场所层的 `apply_platform_turn_scope` 摘掉（QQ 里一律没有）。名单与投递走
//! [`CrossSessionPort`]；消息外壳、显示与重投在场所层（`web::cross_session`）。
//!
//! 描述与参数的真相源是 `src/tools/descriptions/send_to_other_running_session.json`，
//! 这里的只是占位。

use super::{ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use miyu_base::host_ports::{cross_session_port, CrossSessionPort, PeerDelivery};
use serde_json::{json, Value};
use std::sync::Arc;

pub const TOOL_NAME: &str = "send_to_other_running_session";

/// 一条消息的上限。对方要把它整段读进上下文，再长就该写成文件、发路径。
const MAX_MESSAGE_CHARS: usize = 20_000;

pub(super) fn register(registry: &mut ToolRegistry) {
    registry.register(
        ToolSpec::new(
            TOOL_NAME,
            "Talk to the AI in another open session of the same user.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "send"] },
                    "session_id": { "type": "string" },
                    "message": { "type": "string" }
                },
                "required": ["action"]
            }),
            |args| async move { run(args).await },
        )
        .writes(),
    );
}

async fn run(args: Value) -> Result<String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let from =
        miyu_base::workspace::try_session().context("this tool needs a session to send from")?;
    // 注册位置已经挡住了子代理，这里再挡一道：工具桥这类回合外调用也走得到这儿。
    if miyu_base::workspace::in_subagent() {
        bail!(
            "a subagent cannot message other sessions, report back to the agent that started you"
        );
    }
    let port = cross_session_port()
        .context("other sessions are only reachable while the Miyu daemon is running")?;
    match action.as_str() {
        "list" => list(port.as_ref(), &from),
        "send" => send(port, &from, &args).await,
        "" => bail!("action is required: list or send"),
        other => bail!("unknown action \"{other}\", expected list or send"),
    }
}

fn list(port: &dyn CrossSessionPort, from: &str) -> Result<String> {
    let others = port.peers(from)?;
    // 这条会话单独写明：模型不知道自己的会话 id（主机环境块里没有，放进去会让每条
    // 新会话都丢掉系统提示词那段缓存），名单里只剩一条时它会把那条当成自己——09-24
    // 真机，网页会话看到终端开着的那条，却回答「就这一个会话，就是你」。
    let this_session = json!({
        "session_id": from,
        "short_id": miyu_core::state::short_session_id(from),
        "name": port.own_name(from).unwrap_or_default(),
    });
    // 「拿到结果才用得上」的知识写在输出里，不写进 schema（AGENTS §2.1.1）。
    let note = if others.is_empty() {
        "No other session is open or running right now. this_session is you."
    } else {
        "other_sessions are the user's other open or running sessions, not this one. Send with action=send and a session_id or short_id from them. data is the SQLite file holding each session's turns (table turns, by session_id), open it read-only."
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "this_session": this_session,
        "other_sessions": others,
        "note": note,
    }))?)
}

async fn send(port: Arc<dyn CrossSessionPort>, from: &str, args: &Value) -> Result<String> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let to = text("session_id");
    let message = text("message");
    if to.is_empty() {
        bail!("session_id is required for send, take one from action=list");
    }
    if message.is_empty() {
        bail!("message is required for send");
    }
    let chars = message.chars().count();
    if chars > MAX_MESSAGE_CHARS {
        bail!(
            "message has {chars} characters, the limit is {MAX_MESSAGE_CHARS}. Write it to a file and send the path"
        );
    }
    let (peer, delivered) = port.send(from, &to, &message).await?;
    let delivered = match delivered {
        PeerDelivery::Queued => "queued into its running turn",
        PeerDelivery::Started => "started a new turn there",
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "session_id": peer.session_id,
        "name": peer.name,
        "delivered": delivered,
        "note": "A reply reaches you only if that AI messages you back.",
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::BoxFuture;
    use miyu_base::host_ports::{install_cross_session_port, PeerSession};
    use std::sync::Mutex;

    /// 记下收到的投递，名单里只有 `b`。
    struct FakePort(Mutex<Vec<(String, String, String)>>);

    fn peer(id: &str) -> PeerSession {
        PeerSession {
            session_id: id.to_string(),
            short_id: id.to_string(),
            name: format!("会话 {id}"),
            mode: "normal".to_string(),
            cwd: "/tmp".to_string(),
            running: false,
            open: true,
            data: "/tmp/conversation.db".to_string(),
        }
    }

    impl CrossSessionPort for FakePort {
        fn peers(&self, _from: &str) -> Result<Vec<PeerSession>> {
            Ok(vec![peer("b")])
        }

        fn own_name(&self, session_id: &str) -> Option<String> {
            Some(format!("会话 {session_id}"))
        }

        fn send(
            &self,
            from: &str,
            to: &str,
            message: &str,
        ) -> BoxFuture<'static, Result<(PeerSession, PeerDelivery)>> {
            self.0
                .lock()
                .unwrap()
                .push((from.to_string(), to.to_string(), message.to_string()));
            let result = if to == "b" {
                Ok((peer("b"), PeerDelivery::Started))
            } else {
                Err(anyhow::anyhow!("session {to} is not open"))
            };
            Box::pin(async move { result })
        }
    }

    async fn call(session: &str, args: Value) -> Result<String> {
        miyu_base::workspace::with_session(Arc::from(session), run(args)).await
    }

    #[tokio::test]
    async fn lists_and_sends_through_the_port_as_the_calling_session() {
        let port = Arc::new(FakePort(Mutex::new(Vec::new())));
        install_cross_session_port(port.clone());

        let listed = call("a", json!({"action": "list"})).await.unwrap();
        let listed: Value = serde_json::from_str(&listed).unwrap();
        // 这条会话单独写明,别的会话进另一个字段,模型不会把名单里那条当成自己。
        assert_eq!(listed["this_session"]["session_id"], "a");
        assert_eq!(listed["this_session"]["name"], "会话 a");
        assert_eq!(listed["other_sessions"][0]["session_id"], "b");
        assert_eq!(listed["other_sessions"][0]["data"], "/tmp/conversation.db");
        assert!(listed.get("sessions").is_none());

        let sent = call(
            "a",
            json!({"action": "send", "session_id": " b ", "message": " 构建好了 "}),
        )
        .await
        .unwrap();
        let sent: Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(sent["delivered"], "started a new turn there");
        assert_eq!(
            port.0.lock().unwrap().as_slice(),
            [("a".to_string(), "b".to_string(), "构建好了".to_string())]
        );

        let missing = call("a", json!({"action": "send", "session_id": "b"}))
            .await
            .unwrap_err();
        assert!(
            missing.to_string().contains("message is required"),
            "{missing}"
        );
        let unknown = call(
            "a",
            json!({"action": "send", "session_id": "z", "message": "x"}),
        )
        .await
        .unwrap_err();
        assert!(unknown.to_string().contains("not open"), "{unknown}");
    }
}
