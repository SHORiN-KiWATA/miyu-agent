//! 测试夹具:只在 cfg(test) 编译,生产二进制零字节。从 `src/web/mod.rs` 搬来(09-16 夹具搬家)。
#![allow(dead_code)]
use super::*;

impl DaemonState {
    pub(crate) fn for_test_with_actor(
        paths: MiyuPaths,
        web_port: u16,
    ) -> Result<(Self, std::thread::JoinHandle<Result<()>>)> {
        let state_store = StateStore::new(&paths)?;
        let config = AppConfig::default();
        let context = cold_context(&config, &paths, &state_store)?;
        let manager = Arc::new(Mutex::new(ManagerState {
            config: config.clone(),
            active_runs: HashMap::new(),
            admin_busy: false,
            admin_session: None,
            context,
            persona_session_ids: HashMap::new(),
            runs_changed: Arc::new(tokio::sync::Notify::new()),
            compact_requests: HashMap::new(),
            live_turns: HashMap::new(),
            recent_wakes: Default::default(),
        }));
        let events = EventHub::new();
        let questions = QuestionBroker::new();
        let turn_engine = TurnEngineState::default();
        let stores = StoreRegistry::new(state_store.clone(), paths.clone());
        let (actor_tx, actor_join) = spawn_actor(
            config,
            paths.clone(),
            state_store.clone(),
            stores.clone(),
            manager.clone(),
            events.clone(),
            questions.clone(),
            turn_engine.clone(),
            None,
        )?;
        let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
        Ok((
            Self {
                auth: WebAuth::new(None),
                boot_id: Arc::from("boot-test"),
                web_port,
                web_public: false,
                web_bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
                paths,
                manager,
                stores,
                state_store,
                events,
                questions,
                actor_tx,
                shutdown_tx,
                turn_engine,
                platforms: PlatformRuntime::new()?,
                presence: crate::runtime::Presence::default(),
            },
            actor_join,
        ))
    }
}
