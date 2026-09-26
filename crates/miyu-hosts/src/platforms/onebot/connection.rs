//! 反向 WebSocket 连接的接入、鉴权与 API 往返。
//!
//! OneBot 是**反向连接**：客户端主动连过来，Miyu 是服务端。所以要有注册表管理
//! 「当前有哪些连接活着」，还要有 `ConnectionHandle` 让业务侧不必知道自己在跟
//! 哪条连接说话。
//!
//! API 调用是异步请求/响应对，靠 echo 字段配对（`route_api_response`）。连接
//! 断了要唤醒所有还在等的调用方，否则它们会一直挂到超时。
//!
//! `sanitize_api_detail` 存在的原因：OneBot 端的报错里可能带 token。

use crate::platforms::onebot::*;

pub(in crate::platforms::onebot) const API_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds parsed/in-flight events per NapCat connection. Same-conversation
/// LLM turns are serialized later; this cap only prevents an unbounded task
/// buildup under hostile traffic.
pub(in crate::platforms::onebot) const MAX_IN_FLIGHT_MESSAGES: usize = 32;

/// Live NapCat connections keyed by bot QQ id. NapCat reconnects on its
/// own schedule, which can leave a half-open predecessor; each new
/// connection bumps the generation and the old read loop notices it has
/// been replaced and exits, so replies are never duplicated.
#[derive(Default)]
pub(crate) struct ConnectionRegistry {
    next_generation: u64,
    connections: HashMap<i64, RegisteredConnection>,
}

pub(in crate::platforms::onebot) struct RegisteredConnection {
    pub(in crate::platforms::onebot) generation: u64,
    pub(in crate::platforms::onebot) handle: ConnectionHandle,
}

impl ConnectionRegistry {
    pub fn register(&mut self, self_id: i64, handle: ConnectionHandle) -> u64 {
        self.next_generation += 1;
        let generation = self.next_generation;
        if self_id != 0 {
            self.connections
                .insert(self_id, RegisteredConnection { generation, handle });
        }
        generation
    }

    pub(in crate::platforms::onebot) fn bind(
        &mut self,
        self_id: i64,
        generation: u64,
        handle: ConnectionHandle,
    ) -> bool {
        if self_id == 0
            || self
                .connections
                .get(&self_id)
                .is_some_and(|connection| connection.generation > generation)
        {
            return false;
        }
        self.connections
            .insert(self_id, RegisteredConnection { generation, handle });
        true
    }

    pub(in crate::platforms::onebot) fn is_current(&self, self_id: i64, generation: u64) -> bool {
        self.connections
            .get(&self_id)
            .is_some_and(|connection| connection.generation == generation)
    }

    pub(in crate::platforms::onebot) fn remove(&mut self, self_id: i64, generation: u64) -> bool {
        if self.is_current(self_id, generation) {
            self.connections.remove(&self_id);
            true
        } else {
            false
        }
    }

    pub(in crate::platforms::onebot) fn handle(&self, self_id: i64) -> Option<ConnectionHandle> {
        self.connections
            .get(&self_id)
            .map(|connection| connection.handle.clone())
    }

    pub(crate) fn connected_accounts(&self) -> Vec<i64> {
        let mut accounts = self.connections.keys().copied().collect::<Vec<_>>();
        accounts.sort_unstable();
        accounts
    }

    pub(crate) fn disconnect_all(&mut self) {
        for connection in self.connections.values() {
            let _ = connection.handle.shutdown.send(true);
        }
        self.connections.clear();
    }
}

/// Cheap-to-clone sender half of one connection: outbound frames plus
/// the echo table for request/response API calls.
#[derive(Clone)]
pub struct ConnectionHandle {
    pub(in crate::platforms::onebot) out_tx: mpsc::UnboundedSender<String>,
    pub(in crate::platforms::onebot) pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    pub(in crate::platforms::onebot) bot_name: Arc<Mutex<Option<String>>>,
    pub(in crate::platforms::onebot) asset_base_url: Option<String>,
    pub(in crate::platforms::onebot) assets: crate::platforms::assets::AssetLeaseStore,
    pub(in crate::platforms::onebot) shutdown: watch::Sender<bool>,
}

impl ConnectionHandle {
    pub(in crate::platforms::onebot) fn send_frame(&self, frame: String) -> Result<()> {
        self.out_tx
            .send(frame)
            .map_err(|_| anyhow::anyhow!("OneBot connection writer is closed"))
    }

    /// Sends an `{action, params, echo}` frame and waits for the frame
    /// that echoes it back.
    pub(in crate::platforms::onebot) async fn call_api(
        &self,
        action: &str,
        params: Value,
    ) -> Result<Value> {
        self.call_api_with_timeout(action, params, API_CALL_TIMEOUT)
            .await
    }

    pub(in crate::platforms::onebot) async fn call_api_with_timeout(
        &self,
        action: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let echo = random_id("act", 12);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(echo.clone(), tx);
        if let Err(error) = self.send_frame(api_frame(action, params, &echo)) {
            self.pending.lock().unwrap().remove(&echo);
            return Err(error);
        }
        let result = tokio::time::timeout(timeout, rx).await;
        self.pending.lock().unwrap().remove(&echo);
        let response = match result {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => bail!("OneBot API {action} failed: connection closed"),
            Err(_) => bail!("OneBot API {action} timed out"),
        };
        let retcode = response.get("retcode").and_then(value_i64).unwrap_or(-1);
        if retcode != 0 {
            let status = response
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let detail = ["wording", "message", "msg"]
                .into_iter()
                .filter_map(|key| response.get(key).and_then(Value::as_str))
                .map(str::trim)
                .find(|value| !value.is_empty())
                .unwrap_or("no error detail returned");
            let detail = sanitize_api_detail(detail);
            bail!(
                "OneBot API {action} failed: status={status}, retcode={retcode}, detail={detail}"
            );
        }
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    }
}

/// Bridges sometimes splice raw protocol bytes into their error strings — a
/// failed kick comes back with the target's protobuf-encoded UID embedded.
/// Those bytes are unreadable, unhelpful, and go straight into the model's
/// context, so strip the unprintables and cap the length.
pub(in crate::platforms::onebot) fn sanitize_api_detail(detail: &str) -> String {
    const MAX_DETAIL_CHARS: usize = 200;
    let mut cleaned = String::with_capacity(detail.len());
    let mut last_was_space = false;
    for ch in detail.chars() {
        let printable = !ch.is_control() && ch != '\u{fffd}';
        if printable {
            cleaned.push(ch);
            last_was_space = ch == ' ';
        } else if !last_was_space && !cleaned.is_empty() {
            cleaned.push(' ');
            last_was_space = true;
        }
    }
    let cleaned = cleaned.trim();
    if cleaned.chars().count() > MAX_DETAIL_CHARS {
        let kept: String = cleaned.chars().take(MAX_DETAIL_CHARS).collect();
        return format!("{kept}…");
    }
    cleaned.to_string()
}

#[derive(Clone, Default)]
pub(crate) struct QqListenerManager {
    pub(in crate::platforms::onebot) inner: Arc<Mutex<QqListenerState>>,
}

#[derive(Default)]
pub(in crate::platforms::onebot) struct QqListenerState {
    pub(in crate::platforms::onebot) active_port: Option<u16>,
    pub(in crate::platforms::onebot) task: Option<JoinHandle<()>>,
}

pub(crate) struct PreparedQqListener {
    manager: QqListenerManager,
    state: DaemonState,
    desired_port: Option<u16>,
    listener: Option<tokio::net::TcpListener>,
    disconnect_connections: bool,
}

impl QqListenerManager {
    pub(crate) fn active_port(&self) -> Option<u16> {
        self.inner.lock().unwrap().active_port
    }

    pub(crate) async fn prepare(
        &self,
        state: &DaemonState,
        current: Option<&OneBotConfig>,
        next: &OneBotConfig,
    ) -> Result<PreparedQqListener> {
        // The default QQ port is the daemon's WebUI port. If WebUI had to
        // fall back from 8300 because it was occupied, keep the short `/ws`
        // endpoint and the QQ listener on that same effective port. A
        // non-default configured port remains a dedicated listener.
        let desired_port = effective_reverse_ws_port(state, next);
        let active_port = self.inner.lock().unwrap().active_port;
        let needs_dedicated_bind =
            desired_port.is_some_and(|port| port != state.web_port && Some(port) != active_port);
        let listener = if needs_dedicated_bind {
            let port = desired_port.expect("dedicated bind requires a port");
            Some(
                tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
                    .await
                    .with_context(|| {
                        format!("binding Tencent QQ reverse WebSocket to 0.0.0.0:{port}")
                    })?,
            )
        } else {
            None
        };
        let disconnect_connections = current.is_some_and(|current| {
            effective_reverse_ws_port(state, current) != desired_port
                || current.access_token != next.access_token
        });
        Ok(PreparedQqListener {
            manager: self.clone(),
            state: state.clone(),
            desired_port,
            listener,
            disconnect_connections,
        })
    }

    pub(crate) async fn shutdown(&self, state: &DaemonState) {
        let task = {
            let mut inner = self.inner.lock().unwrap();
            inner.active_port = None;
            inner.task.take()
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        state.platforms.onebot.lock().unwrap().disconnect_all();
    }
}

pub(in crate::platforms::onebot) fn effective_reverse_ws_port(
    state: &DaemonState,
    config: &OneBotConfig,
) -> Option<u16> {
    if !config.enabled {
        return None;
    }
    if config.reverse_ws_port == miyu_core::ipc::DEFAULT_WEB_PORT
        && state.web_port != miyu_core::ipc::DEFAULT_WEB_PORT
    {
        Some(state.web_port)
    } else {
        Some(config.reverse_ws_port)
    }
}

impl PreparedQqListener {
    pub fn commit(mut self) {
        let previous_port = self.manager.active_port();
        let previous_task = {
            let mut inner = self.manager.inner.lock().unwrap();
            if inner.active_port == self.desired_port {
                None
            } else {
                let previous = inner.task.take();
                inner.active_port = self.desired_port;
                inner.task = self.listener.take().map(|listener| {
                    let app = qq_listener_router(self.state.clone());
                    tokio::spawn(async move {
                        if let Err(error) = axum::serve(
                            listener,
                            app.into_make_service_with_connect_info::<SocketAddr>(),
                        )
                        .await
                        {
                            tracing::error!(target: "miyu::qq", error = %error, "{}", t("Tencent QQ listener stopped", "腾讯 QQ 监听器已停止"));
                        }
                    })
                });
                previous
            }
        };
        if let Some(task) = previous_task {
            task.abort();
        }
        if self.disconnect_connections {
            self.state.platforms.onebot.lock().unwrap().disconnect_all();
        }
        if previous_port != self.desired_port {
            match self.desired_port {
                Some(port) => {
                    tracing::info!(target: "miyu::qq", port, path = "/ws", "{}", t("Tencent QQ listener ready", "腾讯 QQ 监听器已就绪"))
                }
                None => {
                    tracing::info!(target: "miyu::qq", "{}", t("Tencent QQ listener disabled", "腾讯 QQ 监听器已禁用"))
                }
            }
        }
    }
}

pub(in crate::platforms::onebot) fn qq_listener_router(state: DaemonState) -> Router {
    Router::new()
        .route("/ws", get(onebot_ws))
        .route("/onebot/v11/ws", get(onebot_ws))
        .route(
            "/api/platform-assets/{token}",
            get(crate::platforms::platform_asset),
        )
        .with_state(state)
}

pub(in crate::platforms::onebot) fn api_frame(action: &str, params: Value, echo: &str) -> String {
    json!({ "action": action, "params": params, "echo": echo }).to_string()
}

pub(crate) async fn onebot_ws(
    State(state): State<DaemonState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let config = onebot_config(&state);
    if !config.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !connection_authorized(&headers, &config.access_token, peer) {
        if config.access_token.trim().is_empty() {
            tracing::warn!(target: "miyu::qq", %peer, reason = "non_loopback_without_token", "{}", t("OneBot client rejected", "OneBot 客户端已拒绝"));
        } else {
            tracing::warn!(target: "miyu::qq", %peer, reason = "bad_token", "{}", t("OneBot client rejected", "OneBot 客户端已拒绝"));
        }
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let self_id = headers
        .get("x-self-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let asset_base_url = resolve_asset_base_url(&headers, &config);
    ws.max_message_size(MAX_INBOUND_MESSAGE_BYTES)
        .max_frame_size(MAX_INBOUND_MESSAGE_BYTES)
        .on_upgrade(move |socket| connection_loop(state, socket, self_id, asset_base_url))
}

pub(crate) async fn onebot_ws_on_web_port(
    State(state): State<DaemonState>,
    peer: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if state.platforms.qq_listener.active_port() != Some(state.web_port) {
        return StatusCode::NOT_FOUND.into_response();
    }
    onebot_ws(State(state), peer, headers, ws).await
}

pub(in crate::platforms::onebot) fn connection_authorized(
    headers: &HeaderMap,
    expected: &str,
    peer: SocketAddr,
) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        peer.ip().is_loopback()
    } else {
        token_matches(headers, expected)
    }
}

pub(in crate::platforms::onebot) fn resolve_asset_base_url(
    headers: &HeaderMap,
    config: &OneBotConfig,
) -> Option<String> {
    let configured = config.asset_base_url.trim().trim_end_matches('/');
    if configured.starts_with("http://") || configured.starts_with("https://") {
        return Some(configured.to_string());
    }
    headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|host| {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-:[]".contains(&byte))
        })
        .map(|host| format!("http://{host}"))
}

pub(in crate::platforms::onebot) fn onebot_config(state: &DaemonState) -> OneBotConfig {
    state.manager.lock().unwrap().config.platforms.qq.clone()
}

/// Compares digests rather than raw strings so length/prefix timing
/// leaks nothing. An empty configured token disables the check.
pub(in crate::platforms::onebot) fn token_matches(headers: &HeaderMap, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return true;
    }
    let supplied = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("Token "))
                .or(Some(value))
        })
        .map(str::trim);
    let Some(supplied) = supplied else {
        return false;
    };
    Sha256::digest(supplied.as_bytes()) == Sha256::digest(expected.as_bytes())
}

pub(in crate::platforms::onebot) async fn connection_loop(
    state: DaemonState,
    socket: WebSocket,
    self_id: i64,
    asset_base_url: Option<String>,
) {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let handle = ConnectionHandle {
        out_tx,
        pending: Arc::new(Mutex::new(HashMap::new())),
        bot_name: Arc::new(Mutex::new(None)),
        asset_base_url,
        assets: state.platforms.assets.clone(),
        shutdown,
    };
    let generation = state
        .platforms
        .onebot
        .lock()
        .unwrap()
        .register(self_id, handle.clone());
    tracing::info!(target: "miyu::qq", self_id, generation, "{}", t("OneBot client connected", "OneBot 客户端已连接"));
    if self_id != 0 {
        // 掉线时留着的后台任务汇报，连上就补发（09-26）。
        tokio::spawn(held_reports::deliver_for_account(state.clone(), self_id));
    }

    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if sink.send(Message::Text(frame.into())).await.is_err() {
                break;
            }
        }
    });
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_MESSAGES));
    let mut bound_self_id = self_id;

    loop {
        let message = tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            message = stream.next() => {
                let Some(message) = message else { break; };
                message
            }
        };
        let message = match message {
            Ok(message) => message,
            Err(_) => break,
        };
        if bound_self_id != 0
            && !state
                .platforms
                .onebot
                .lock()
                .unwrap()
                .is_current(bound_self_id, generation)
        {
            tracing::info!(target: "miyu::qq",
                self_id,
                generation,
                "{}",
                t("OneBot connection replaced by a newer one", "OneBot 连接已被新连接替换")
            );
            break;
        }
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(frame) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(event_self_id) = frame
            .get("self_id")
            .and_then(Value::as_i64)
            .filter(|id| *id != 0)
        {
            if bound_self_id == 0 {
                bound_self_id = event_self_id;
                let bound = state.platforms.onebot.lock().unwrap().bind(
                    bound_self_id,
                    generation,
                    handle.clone(),
                );
                if !bound {
                    tracing::info!(target: "miyu::qq",
                    self_id = bound_self_id,
                    generation,
                    "{}",
                    t("OneBot connection identity is already owned by a newer connection", "OneBot 连接身份已被新连接占用")
                    );
                    break;
                }
                group_mute_cache()
                    .lock()
                    .unwrap()
                    .remove_account(bound_self_id);
                group_role_cache()
                    .lock()
                    .unwrap()
                    .remove_account(bound_self_id);
                tracing::info!(target: "miyu::qq",
                    self_id = bound_self_id,
                    generation,
                    "{}",
                    t("OneBot connection identity bound from event", "已从事件绑定 OneBot 连接身份")
                );
                tokio::spawn(held_reports::deliver_for_account(
                    state.clone(),
                    bound_self_id,
                ));
            } else if bound_self_id != event_self_id {
                tracing::warn!(target: "miyu::qq",
                    expected = bound_self_id,
                    received = event_self_id,
                    "{}",
                    t("OneBot connection changed self_id", "OneBot 连接更改了 self_id")
                );
                break;
            }
        }
        if frame.get("post_type").is_none() {
            route_api_response(&handle, frame);
            continue;
        }
        if frame.get("post_type").and_then(Value::as_str) == Some("message") {
            let ingress_order = next_ingress_order();
            let activity = observe_message_activity(&state, &frame, bound_self_id, Instant::now());
            // 平台关闭或事件不成立时不做 AppConfig 全量深拷贝:这是每条
            // 入站消息(含不触发回复的普通消息)都会走到的热路径。
            let qq_enabled = state.manager.lock().unwrap().config.platforms.qq.enabled;
            if qq_enabled {
                if let Some(inbound) =
                    ingress_message_event(&frame, bound_self_id, ingress_order, activity.as_ref())
                {
                    let config = state.manager.lock().unwrap().config.clone();
                    match state.platforms.plugins() {
                        Ok(plugins) => {
                            plugins
                                .observe_ingress(&state.paths, &config, &inbound)
                                .await;
                        }
                        Err(error) => tracing::warn!(
                            target: "miyu::qq",
                            error = %error,
                            "{}",
                            t(
                                "OneBot message history initialization failed",
                                "OneBot 消息历史初始化失败"
                            )
                        ),
                    }
                }
            }
            let connection_permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!(target: "miyu::qq",
                        self_id = bound_self_id,
                        "{}",
                        t("OneBot connection event queue is full; dropping a message", "OneBot 连接事件队列已满，丢弃消息")
                    );
                    continue;
                }
            };
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                let _connection_permit = connection_permit;
                handle_message_with_activity(state, handle, frame, ingress_order, activity).await;
            });
        } else if is_group_upload_notice(&frame) {
            let connection_permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!(target: "miyu::qq",
                        self_id = bound_self_id,
                        "{}",
                        t("OneBot connection concurrency is full; dropping a group upload notice", "OneBot 连接并发已满，丢弃群文件上传通知")
                    );
                    continue;
                }
            };
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                let _connection_permit = connection_permit;
                handle_group_file_upload(state, handle, frame).await;
            });
        } else if is_message_recall(&frame) {
            let connection_permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!(target: "miyu::qq",
                        self_id = bound_self_id,
                        "{}",
                        t("OneBot connection concurrency is full; dropping a recall notice", "OneBot 连接并发已满，丢弃撤回通知")
                    );
                    continue;
                }
            };
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                let _connection_permit = connection_permit;
                handle_message_recall(state, handle, frame).await;
            });
        } else if is_friend_add_request(&frame) {
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                handle_friend_add_request(state, handle, frame).await;
            });
        } else if is_group_add_request(&frame) {
            let approval_permit = match group_join_approval_semaphore().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!(
                        target: "miyu::qq",
                        self_id = bound_self_id,
                        "{}",
                        t(
                            "OneBot group join request dropped (approval queue is full)",
                            "OneBot 入群申请已丢弃（审批队列已满）"
                        )
                    );
                    continue;
                }
            };
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                let _approval_permit = approval_permit;
                handle_group_add_request(state, handle, frame).await;
            });
        } else if is_group_invite_request(&frame) {
            tracing::info!(
                target: "miyu::qq",
                self_id = frame.get("self_id").and_then(value_i64).unwrap_or(0),
                group_id = frame.get("group_id").and_then(value_i64).unwrap_or(0),
                "{}",
                t(
                    "OneBot group invite left pending (only join requests are reviewed)",
                    "OneBot 群邀请已保持待处理（仅审批入群申请）"
                )
            );
        } else if is_group_ban_notice(&frame) {
            update_group_ban_notice(&frame);
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                handle_group_management_notice(state, handle, frame).await;
            });
        } else if is_group_decrease_notice(&frame) {
            let state = state.clone();
            let handle = handle.clone();
            tokio::spawn(async move {
                handle_group_management_notice(state, handle, frame).await;
            });
        }
    }

    let removed = state
        .platforms
        .onebot
        .lock()
        .unwrap()
        .remove(bound_self_id, generation);
    if removed {
        group_mute_cache()
            .lock()
            .unwrap()
            .remove_account(bound_self_id);
        group_role_cache()
            .lock()
            .unwrap()
            .remove_account(bound_self_id);
    }
    // 显式 drain pending：handle 的克隆仍被在途消息处理 task 持有，Arc 不会
    // 因本循环退出而归零，必须主动 drop 所有 tx 让在途 call_api 立即失败，
    // 而不是各等满超时（附件发送最长 180s）。
    handle.pending.lock().unwrap().clear();
    writer.abort();
    tracing::info!(target: "miyu::qq",
        self_id = bound_self_id,
        generation,
        "{}",
        t("OneBot client disconnected", "OneBot 客户端已断开")
    );
}

/// Routes an API response frame to its waiting `call_api`; unmatched
/// response failures still get a diagnostic.
pub(in crate::platforms::onebot) fn route_api_response(handle: &ConnectionHandle, frame: Value) {
    let echo = frame
        .get("echo")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(echo) = echo {
        if let Some(waiter) = handle.pending.lock().unwrap().remove(&echo) {
            let _ = waiter.send(frame);
            return;
        }
    }
    let retcode = frame.get("retcode").and_then(Value::as_i64).unwrap_or(0);
    if retcode != 0 {
        tracing::warn!(retcode, "{}", t("OneBot send failed", "OneBot 发送失败"));
    }
}
