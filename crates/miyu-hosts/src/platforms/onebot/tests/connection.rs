//! 反向 WS 连接、鉴权与 API 往返。

use super::shared::*;
use crate::platforms::onebot::*;

#[tokio::test]
async fn listener_rebind_is_transactional_and_reuses_the_web_port() {
    let temp = tempfile::tempdir().unwrap();
    let web_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let web_port = web_listener.local_addr().unwrap().port();
    let state = test_web_state(temp.path(), web_port);
    let listener = state.platforms.qq_listener.clone();

    let shared = config_with(|config| {
        config.enabled = true;
        config.reverse_ws_port = web_port;
    });
    listener
        .prepare(&state, None, &shared)
        .await
        .unwrap()
        .commit();
    {
        let inner = listener.inner.lock().unwrap();
        assert_eq!(inner.active_port, Some(web_port));
        assert!(inner.task.is_none());
    }

    let available = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let dedicated_port = available.local_addr().unwrap().port();
    drop(available);
    let dedicated = config_with(|config| {
        config.enabled = true;
        config.reverse_ws_port = dedicated_port;
    });
    listener
        .prepare(&state, Some(&shared), &dedicated)
        .await
        .unwrap()
        .commit();
    {
        let inner = listener.inner.lock().unwrap();
        assert_eq!(inner.active_port, Some(dedicated_port));
        assert!(inner.task.is_some());
    }

    let occupied = tokio::net::TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let conflict = config_with(|config| {
        config.enabled = true;
        config.reverse_ws_port = occupied_port;
    });
    assert!(listener
        .prepare(&state, Some(&dedicated), &conflict)
        .await
        .is_err());
    {
        let inner = listener.inner.lock().unwrap();
        assert_eq!(inner.active_port, Some(dedicated_port));
        assert!(inner.task.is_some());
    }

    let disabled = OneBotConfig::default();
    listener
        .prepare(&state, Some(&dedicated), &disabled)
        .await
        .unwrap()
        .commit();
    let inner = listener.inner.lock().unwrap();
    assert_eq!(inner.active_port, None);
    assert!(inner.task.is_none());
}

#[tokio::test]
async fn default_qq_port_follows_the_web_fallback_port() {
    let temp = tempfile::tempdir().unwrap();
    let web_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let web_port = web_listener.local_addr().unwrap().port();
    assert_ne!(web_port, miyu_core::ipc::DEFAULT_WEB_PORT);
    let state = test_web_state(temp.path(), web_port);
    let listener = state.platforms.qq_listener.clone();
    let config = config_with(|config| config.enabled = true);

    assert_eq!(effective_reverse_ws_port(&state, &config), Some(web_port));
    listener
        .prepare(&state, None, &config)
        .await
        .unwrap()
        .commit();

    let inner = listener.inner.lock().unwrap();
    assert_eq!(inner.active_port, Some(web_port));
    assert!(inner.task.is_none());
}

#[test]
fn token_check_accepts_bearer_and_rejects_wrong() {
    let mut headers = HeaderMap::new();
    assert!(token_matches(&headers, ""));
    assert!(!token_matches(&headers, "secret"));
    headers.insert(AUTHORIZATION, "Bearer secret".parse().unwrap());
    assert!(token_matches(&headers, "secret"));
    assert!(!token_matches(&headers, "other"));
    headers.insert(AUTHORIZATION, "Token secret".parse().unwrap());
    assert!(token_matches(&headers, "secret"));
    headers.insert(AUTHORIZATION, "secret".parse().unwrap());
    assert!(token_matches(&headers, "secret"));
}

#[test]
fn empty_token_authorizes_only_local_network_connections() {
    let headers = HeaderMap::new();
    for peer in [
        "127.0.0.1:1234",
        "[::1]:1234",
        "192.168.1.5:1234",
        "10.0.0.2:1234",
        "172.17.0.2:1234",
        "100.101.102.103:1234",
        "[fd00::5]:1234",
        "[fe80::1]:1234",
        "[::ffff:192.168.1.5]:1234",
    ] {
        assert!(
            connection_authorized(&headers, "", peer.parse().unwrap()),
            "{peer} should be accepted"
        );
    }
    for peer in [
        "8.8.8.8:1234",
        "100.128.0.1:1234",
        "[2001:db8::1]:1234",
        "[::ffff:8.8.8.8]:1234",
    ] {
        assert!(
            !connection_authorized(&headers, "", peer.parse().unwrap()),
            "{peer} should be rejected"
        );
    }
}

#[test]
fn late_identity_binding_cannot_replace_a_newer_connection() {
    let (older, _older_frames) = test_connection(None);
    let (newer, _newer_frames) = test_connection(None);
    let mut registry = ConnectionRegistry::default();
    let older_generation = registry.register(0, older.clone());
    let newer_generation = registry.register(0, newer.clone());

    assert!(registry.bind(10000, newer_generation, newer));
    assert!(!registry.bind(10000, older_generation, older));
    assert!(registry.is_current(10000, newer_generation));
    assert!(!registry.is_current(10000, older_generation));
}

#[tokio::test]
async fn api_calls_wait_for_the_matching_echo() {
    let (handle, mut frames) = test_connection(None);
    let caller = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.call_api("get_login_info", json!({})).await })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(frame["action"], "get_login_info");
    let echo = frame["echo"].as_str().unwrap().to_string();

    // An unrelated response must not resolve this request.
    route_api_response(
        &handle,
        json!({ "status": "ok", "retcode": 0, "data": null, "echo": "other" }),
    );
    assert!(!caller.is_finished());
    route_api_response(
        &handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "nickname": "Miyu" },
            "echo": echo,
        }),
    );
    let data = caller.await.unwrap().unwrap();
    assert_eq!(data["nickname"], "Miyu");
    assert!(handle.pending.lock().unwrap().is_empty());
}

#[test]
fn api_error_detail_drops_raw_protocol_bytes() {
    // Verbatim shape of a failed kick: NapCat splices the target's
    // protobuf-encoded UID into the wording.
    let raw = "kick member failed: \u{8}\u{0}\u{12}\u{18}u_GnsZB8HSJVKfjWNjMqYqbA";
    let cleaned = sanitize_api_detail(raw);
    assert_eq!(cleaned, "kick member failed: u_GnsZB8HSJVKfjWNjMqYqbA");
    assert!(!cleaned.chars().any(char::is_control));

    assert_eq!(sanitize_api_detail("  spaced  "), "spaced");
    let long = "x".repeat(500);
    let clipped = sanitize_api_detail(&long);
    assert!(clipped.ends_with('…'));
    assert_eq!(clipped.chars().count(), 201);
}

#[tokio::test]
async fn api_errors_preserve_napcat_status_retcode_and_wording() {
    let (handle, mut frames) = test_connection(None);
    let caller = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .call_api("delete_msg", json!({ "message_id": 1 }))
                .await
        })
    };
    let frame: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    route_api_response(
        &handle,
        json!({
            "status": "failed",
            "retcode": "1200",
            "wording": "消息已超过撤回时限",
            "echo": frame["echo"],
        }),
    );
    let error = caller.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("status=failed"));
    assert!(error.contains("retcode=1200"));
    assert!(error.contains("消息已超过撤回时限"));
}

/// Regression: a picture is megabytes of base64 JSON that NapCat has to
/// receive, decode and upload to QQ. Any budget short of the backstop made
/// Miyu treat a delivered image as failed and post the text fallback on top
/// of it. Size scaling was not enough — the old `div_ceil` step handed a
/// 0.99 MiB payload the same 30s as a 64 KiB one.
#[test]
fn attachment_sends_wait_for_napcat_instead_of_a_size_budget() {
    let text_only = vec![text_segment("hello")];
    assert_eq!(send_timeout_for(&text_only), API_CALL_TIMEOUT);

    let small_image = vec![image_segment(&vec![0u8; 64 * 1024], false)];
    assert_eq!(send_timeout_for(&small_image), MAX_SEND_TIMEOUT);

    // The old boundary case: just under a megabyte used to share the
    // smallest budget with a thumbnail.
    let boundary_image = vec![image_segment(&vec![0u8; 700 * 1024], false)];
    assert_eq!(send_timeout_for(&boundary_image), MAX_SEND_TIMEOUT);

    let huge_image = vec![image_segment(&vec![0u8; 19 * 1024 * 1024], false)];
    assert_eq!(send_timeout_for(&huge_image), MAX_SEND_TIMEOUT);

    // Mixed frames follow the attachment, not the text.
    let mixed = vec![text_segment("看图"), image_segment(&vec![0u8; 4096], false)];
    assert_eq!(send_timeout_for(&mixed), MAX_SEND_TIMEOUT);
}

#[tokio::test]
async fn delete_message_sends_one_numeric_request_and_does_not_retry_failure() {
    let (handle, mut frames) = test_connection(None);
    let adapter = test_adapter(handle.clone(), Target::Group { group_id: 7 });
    let caller = tokio::spawn(async move { adapter.delete_message("442989412").await });

    let request: Value = serde_json::from_str(&frames.recv().await.unwrap()).unwrap();
    assert_eq!(request["action"], "delete_msg");
    assert_eq!(request["params"]["message_id"], 442989412);
    route_api_response(
        &handle,
        json!({
            "status": "failed",
            "retcode": 1200,
            "wording": "decode failed",
            "echo": request["echo"],
        }),
    );
    let error = caller.await.unwrap().unwrap_err().to_string();
    assert!(error.contains("retcode=1200"));
    assert!(error.contains("decode failed"));
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn api_call_fails_immediately_when_the_writer_is_closed() {
    let (handle, frames) = test_connection(None);
    drop(frames);
    let started = tokio::time::Instant::now();

    assert!(handle.call_api("get_status", json!({})).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(handle.pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn adapter_uses_the_new_connection_after_reconnect() {
    let (old_handle, mut old_frames) = test_connection(None);
    let adapter = Arc::new(test_adapter(old_handle, Target::Private { user_id: 42 }));
    let (new_handle, mut new_frames) = test_connection(None);
    adapter
        .registry
        .lock()
        .unwrap()
        .register(adapter.self_id, new_handle.clone());

    let send = {
        let adapter = adapter.clone();
        tokio::spawn(async move {
            adapter
                .send_message_segments(vec![text_segment("hello")])
                .await
        })
    };
    let frame: Value = serde_json::from_str(&new_frames.recv().await.unwrap()).unwrap();
    assert!(old_frames.try_recv().is_err());
    route_api_response(
        &new_handle,
        json!({
            "status": "ok",
            "retcode": 0,
            "data": { "message_id": 1 },
            "echo": frame["echo"],
        }),
    );
    assert!(send.await.unwrap().is_ok());
}
