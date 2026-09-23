//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/runtime/state.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;
use std::net::Ipv4Addr;

impl DaemonState {
    pub(crate) fn for_test(paths: MiyuPaths, web_port: u16) -> Result<Self> {
        let state_store = StateStore::new(&paths)?;
        let config = AppConfig::default();
        let context = cold_context(&config, &paths, &state_store)?;
        let manager = Arc::new(Mutex::new(ManagerState {
            config,
            active_runs: HashMap::new(),
            admin_busy: false,
            admin_session: None,
            context,
            persona_session_ids: HashMap::new(),
            runs_changed: Arc::new(tokio::sync::Notify::new()),
        }));
        let (actor_tx, _actor_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
        Ok(Self {
            auth: WebAuth::new(None),
            boot_id: Arc::from("boot-test"),
            web_port,
            web_public: false,
            web_bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            stores: StoreRegistry::new(state_store.clone(), paths.clone()),
            paths,
            manager,
            state_store,
            events: EventHub::new(),
            questions: QuestionBroker::new(),
            actor_tx,
            shutdown_tx,
            turn_engine: TurnEngineState::default(),
            platforms: PlatformRuntime::new()?,
            presence: Presence::default(),
        })
    }
}

impl WebAuth {
    pub(crate) fn is_authenticated(&self, supplied: Option<&str>) -> bool {
        self.identity(supplied).is_some()
    }

    /// 限流表当前跟踪了多少个来源。给测试量「有没有无限涨」用。
    pub(crate) fn tracked_login_peers(&self) -> usize {
        self.attempts.lock().unwrap().len()
    }
}
