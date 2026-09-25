//! 后台任务的状态订阅与展示。
//!
//! 轮询线程（`spawn_jobs_poll_thread`）把 daemon 那边的任务状态拉过来，REPL 只
//! 读快照。`JOBS_FEED_MARK_LIMIT` 限制「已读」标记的数量——它只用于去重通知，
//! 无限增长毫无意义。

use crate::cli::*;

pub(in crate::cli) const JOB_SPINNER_FRAMES: [char; 10] =
    ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// 后台任务条的用时:时分秒(过了一小时也带秒,09-23 与其它用时统一)。
pub(in crate::cli) fn format_job_duration(seconds: u64) -> String {
    miyu_base::durations::format_hms(std::time::Duration::from_secs(seconds))
}

/// 这条排队消息是 daemon 合成的后台任务报告吗。判据和剥前缀的那个函数同源，
/// 别在别处再写一份前缀字面量。
pub(in crate::cli) fn is_job_wake_headline(headline: &str) -> bool {
    headline.starts_with("[后台任务完成] ") || headline.starts_with("[后台命令完成] ")
}

/// 这条排队消息是不是 daemon 合成的通知：后台任务报告，或另一个会话里的 AI 发来的
/// 跨会话消息（09-23）。它们不是谁敲的话，排在队里不占气泡，被读到时走时间线上的
/// 通知那条路。
pub(in crate::cli) fn is_daemon_notice(display: &str) -> bool {
    is_job_wake_headline(display)
        || miyu_core::state::parse_cross_session_message(display).is_some()
        || miyu_core::state::service_restart_attempt(display).is_some()
}

/// Strips the bracketed prefix off a background-job wake headline, leaving
/// `子代理完成 82bea3 · 标题`. The older `[后台命令完成] ` spelling still shows
/// up in sessions recorded before the rename.
pub(in crate::cli) fn job_wake_headline(headline: &str) -> String {
    headline
        .strip_prefix("[后台任务完成] ")
        .or_else(|| headline.strip_prefix("[后台命令完成] "))
        .map(str::to_string)
        .unwrap_or_else(|| headline.to_string())
}

/// Fires a desktop notification unless the REPL window has focus.
///
/// `focused` is `None` when there is no live tail — a one-shot `miyu ask` has
/// no window to be away from, so it stays quiet.
///
/// kitty 那条路由终端自己弹（09-18）：只有终端自己能在点击时把自己的窗口拉到
/// 前面，顺带按声音主题响一声。它也自己判焦点（`o=unfocused`），比终端上报的
/// `focused` 准——终端不上报焦点时那个标志会一直钉在 `true`，本来会一声不响。
pub(in crate::cli) fn notify_if_unfocused(
    config: &AppConfig,
    focused: Option<bool>,
    title: &str,
    body: &str,
    sound: miyu_base::notify::NotifySound,
) {
    if !config.notifications.enabled || focused.is_none() {
        return;
    }
    let tone = notification_tone(config, sound, miyu_base::terminal::herdr::in_pane());
    let body = miyu_base::notify::clip_body(body, 120);
    if miyu_base::notify::notify_via_kitty(title, &body, &tone) {
        // 自定义音频文件 kitty 放不了（`s=` 只认声音主题里的名字），得我们自己
        // 放；而它是不是该响就得自己判了——kitty 有焦点时会把通知整条扣下，
        // 声音不跟着扣就成了「人对着屏幕坐着，它自己叮一声」。
        if matches!(tone, miyu_base::notify::NotifyTone::File(_)) && focused == Some(false) {
            miyu_base::notify::play_tone(&tone);
        }
        return;
    }
    if focused != Some(false) {
        return;
    }
    miyu_base::notify::notify_with_sound(title, &body, &tone);
}

/// 这一声由谁来放。
///
/// 在 herdr 的 pane 里交给 herdr（用户 09-23 拍板）：它按 tab 可见性自己放
/// 「完成」「在等你」两声（`[ui.sound]`，默认开），跟 Claude 在 herdr 里的体验
/// 一致；Miyu 再响一声就成了两声。弹窗不受影响，照走系统通知。
pub(in crate::cli) fn notification_tone(
    config: &AppConfig,
    sound: miyu_base::notify::NotifySound,
    in_herdr: bool,
) -> miyu_base::notify::NotifyTone {
    if in_herdr {
        return miyu_base::notify::NotifyTone::Silent;
    }
    config.notifications.tone(sound)
}

/// Shared feed state between the remote REPL and its IPC poll thread.
#[derive(Default)]
pub(in crate::cli) struct SharedJobsFeed {
    /// The owning REPL's current session — strip snapshots are filtered to
    /// it (daemon "current session" can drift from the REPL's after /new).
    pub(in crate::cli) repl_session: std::sync::Mutex<Option<String>>,
    pub(in crate::cli) jobs: std::sync::Mutex<Vec<miyu_engine::tools::jobs::JobOverview>>,
    /// Rendered wake-turn reports waiting to be printed into the scrollback.
    pub(in crate::cli) reports: std::sync::Mutex<Vec<BackgroundReport>>,
    /// Latest session Σ read straight from the store. Background subagents
    /// bill to the session that launched them, but they finish long after the
    /// turn that spawned them published its totals — without this the footer
    /// sat on a stale Σ until the user happened to send another prompt.
    ///
    /// 带着读库那一刻的代次（见 `footer_generation`）。
    pub(in crate::cli) cumulative: std::sync::Mutex<Option<(u64, TurnTokens)>>,
    /// footer 每被显式刷新一次就换一代（`LiveReplTail::set_footer`）。
    ///
    /// 回合跑着时这一轮的用量还没落库，轮询读到的是回合前的旧 Σ；回合一结束
    /// footer 刷成新数，空闲循环第一拍却把那份旧读数套了回去——屏上 Σ 先跳回
    /// 这一轮开始前的数，下一次轮询才又改回来（用户 09-23：「取消之后它会先回到
    /// 最开始的数然后再加上去」）。所以只认刷新之后才开读的那份。
    pub(in crate::cli) footer_generation: std::sync::atomic::AtomicU64,
    /// 这条 REPL 的会话上挂着的目标。轮询线程一秒问一次（`GoalStatus`）——
    /// 输入框右上角那行 `/goal …` 靠它自己往前走（轮次、暂停、受阻、上一轮
    /// 空转停下来等人），不必等下一条命令或下一个回合。
    pub(in crate::cli) goal: std::sync::Mutex<Option<miyu_core::ipc::GoalHint>>,
    /// 两条车道各自开一条新会话时的上下文（`[普通, 开发]`）。大厅里按 Tab 只换显示，
    /// 换过去那条车道还没有会话，footer 上的数靠它（`EmptySessionContext`）。轮询
    /// 线程起来时问一次。
    pub(in crate::cli) empty_context: std::sync::Mutex<[Option<u64>; 2]>,
    /// Active daemon-initiated wake runs.
    pub(in crate::cli) wake_runs: std::sync::Mutex<Vec<WakeRun>>,
    /// 人起的活跃轮 `(run_id, session_id)`：同一个会话的**别的**客户端起的。
    pub(in crate::cli) peer_runs: std::sync::Mutex<Vec<(String, String)>>,
    /// **我自己**起的轮。回合刚结束到 daemon 把它从活跃表里摘掉之间有个窗口，
    /// 不记下来的话这个 REPL 会把自己刚跑完的那一轮当成「别人的」再画一遍。
    pub(in crate::cli) own_runs: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Wake runs already attached to (never re-follow), and turn ids that
    /// were rendered live (their DB report must not print again).
    pub(in crate::cli) followed_runs: std::sync::Mutex<std::collections::HashSet<String>>,
    pub(in crate::cli) rendered_turns: std::sync::Mutex<std::collections::HashSet<String>>,
    /// 访问路径（访问栈，从车道上那条会话往上），栈顶是回去的那条。任务条要列父会话名下的
    /// 子代理，轮询线程按它再拉一份（09-25）。没在访问就是空的。
    pub(in crate::cli) visit_path: std::sync::Mutex<Vec<String>>,
    /// 各条会话名下的子代理会话（什么状态都有，任务条自己挑），按会话号存。轮询线程一秒
    /// 拉一次正在看的这条和父会话的；访问路径上的都留着——退回上一层那一下，那一层的子代理
    /// 表就在手里，任务条不空一拍（用户 09-25：切回来时状态行会消失一瞬间）。
    pub(in crate::cli) children:
        std::sync::Mutex<std::collections::HashMap<String, Vec<super::strip::SubagentRow>>>,
}

/// 这个 REPL 进程里那一条。后台面板要读 `trace`，而它拿不到 `SharedJobsFeed` 的
/// 引用（`Screen` 不持有它）——与其把引用一路穿下去，不如让轮询线程把自己登记
/// 在这儿。一个 REPL 进程只有一条。
static FEED: std::sync::OnceLock<std::sync::Arc<SharedJobsFeed>> = std::sync::OnceLock::new();

pub(in crate::cli) fn feed() -> Option<&'static std::sync::Arc<SharedJobsFeed>> {
    FEED.get()
}

/// footer 刚被显式刷新：在这之前开读的 Σ 一律作废（见 `footer_generation`）。
pub(in crate::cli) fn invalidate_polled_cumulative() {
    if let Some(feed) = FEED.get() {
        feed.footer_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// 两个去重集合的容量兜底。常开 REPL 的后台唤醒一直发生,集合只增不减;
/// 死掉的 id 不会再被查到(run 不再出现在 wake_runs、turn 已过水位线),
/// 超限时清掉无副作用。
pub(in crate::cli) const JOBS_FEED_MARK_LIMIT: usize = 4_096;

#[derive(Clone)]
pub(in crate::cli) struct BackgroundReport {
    pub(in crate::cli) turn_id: String,
    pub(in crate::cli) headline: String,
    pub(in crate::cli) reply: String,
    /// 后台任务报告附的结果段：铃铛那一行点开看（09-26）。
    pub(in crate::cli) job_report: Option<miyu_core::state::JobReportResult>,
    /// 回复末尾那行 `✻`：跑完或被打断的轮才有（报错的轮不画，回放也没有它）。
    pub(in crate::cli) turn_end: Option<ReportTurnEnd>,
}

/// 补印的那一轮收尾要的几样（见 `render::timeline::TurnEnd`）。
#[derive(Clone)]
pub(in crate::cli) struct ReportTurnEnd {
    pub(in crate::cli) model: Option<String>,
    pub(in crate::cli) elapsed: std::time::Duration,
    pub(in crate::cli) finished_at: chrono::DateTime<chrono::Local>,
    pub(in crate::cli) interrupted: bool,
}

/// 任务条那棵树上的任务：正在看的会话和访问路径上各条会话自己的，以及它们名下的（树根是
/// 它们的），没挂会话的老任务也留着。任务条第一层只列会话自己的，后代的收进「（+N）」，但切进去
/// 那一下就要展开——手里先留着，不等下一轮轮询。
pub(in crate::cli) fn retain_tree_jobs(
    jobs: &mut Vec<miyu_engine::tools::jobs::JobOverview>,
    current: &str,
    path: &[String],
) {
    let in_tree = |session: Option<&str>| {
        session.is_some_and(|session| session == current || path.iter().any(|at| at == session))
    };
    jobs.retain(|job| {
        job.session_id.is_none()
            || in_tree(job.session_id.as_deref())
            || in_tree(job.root_session_id.as_deref())
    });
}

/// Session isolation for the strip: keep only `session`'s jobs (sessionless
/// jobs stay visible as a legacy fallback; `None` session shows everything).
pub(in crate::cli) fn retain_session_jobs(
    jobs: &mut Vec<miyu_engine::tools::jobs::JobOverview>,
    session: Option<&str>,
) {
    if let Some(session) = session {
        // 后代(子代理/孙代理)的后台任务也列:它们归自己的会话,树根是当前会话
        // (09-18 会话化;用户:主会话里看不见孙代理)。
        jobs.retain(|job| {
            job.session_id.is_none()
                || job.session_id.as_deref() == Some(session)
                || job.root_session_id.as_deref() == Some(session)
        });
    }
}

impl SharedJobsFeed {
    /// 换成这个 REPL 现在看着的会话，访问路径不动（切进子会话、回去走 `set_scope`）。已经拉
    /// 回来的任务表里不属于它的当场摘掉：等下一轮轮询（约 1 秒）才换的话，这一秒里状态行上
    /// 还是上一个会话的后台任务。
    pub(in crate::cli) fn set_repl_session(&self, session: &str) {
        let path = self.visit_path.lock().unwrap().clone();
        self.set_scope(session, &path);
    }

    /// 换任务条看的那一段树：正在看的会话、访问路径（栈底是车道上那条，栈顶是回去的那条）。
    /// 两样一起换、一起按新的范围摘任务——分两步的话，中间那一步按半新半旧的范围摘，退回主
    /// 会话时会把主会话自己的任务先摘掉，状态行空一拍（用户 09-25）。
    pub(in crate::cli) fn set_scope(&self, session: &str, path: &[String]) {
        let mut current = self.repl_session.lock().unwrap();
        let mut visit_path = self.visit_path.lock().unwrap();
        if current.as_deref() == Some(session) && visit_path.as_slice() == path {
            return;
        }
        *current = Some(session.to_string());
        *visit_path = path.to_vec();
        retain_tree_jobs(&mut self.jobs.lock().unwrap(), session, path);
        // 子代理表只留访问路径上的和这一条的：再往回退都用得上，别的用不上了。
        self.children
            .lock()
            .unwrap()
            .retain(|owner, _| owner == session || path.contains(owner));
    }

    /// 刚拉回来的 `session` 名下的子代理放上去。它不在这会儿看的那一段树上（拉的途中切走
    /// 了）就不收：收了也画不出来，只会留在表里。
    pub(in crate::cli) fn publish_children(
        &self,
        session: &str,
        rows: Vec<super::strip::SubagentRow>,
    ) {
        let current = self.repl_session.lock().unwrap();
        let path = self.visit_path.lock().unwrap();
        if current.as_deref() == Some(session) || path.iter().any(|owner| owner == session) {
            self.children
                .lock()
                .unwrap()
                .insert(session.to_string(), rows);
        }
    }

    /// 刚拉回来的任务表按这个 REPL 看着的那一段树过滤后放上去，返回过滤后的那份。
    ///
    /// 过滤和写入都在会话锁里做：拉取途中 REPL 换了会话，按旧会话过滤的那份就不会
    /// 盖上来。还不知道自己是哪条会话时一条都不认——原来这时「全都显示」，新开的终端
    /// 于是先闪一下别的会话的后台任务（todolist 09-24）。
    pub(in crate::cli) fn publish_jobs(
        &self,
        mut jobs: Vec<miyu_engine::tools::jobs::JobOverview>,
    ) -> Vec<miyu_engine::tools::jobs::JobOverview> {
        let session = self.repl_session.lock().unwrap();
        let path = self.visit_path.lock().unwrap();
        match session.as_deref() {
            Some(session) => retain_tree_jobs(&mut jobs, session, &path),
            None => jobs.clear(),
        }
        *self.jobs.lock().unwrap() = jobs.clone();
        jobs
    }

    /// 任务条此刻该列的行（`strip_tree`），会话和各层的子代理表从这儿取。`visits` 是访问栈。
    pub(in crate::cli) fn strip_items(
        &self,
        visits: &[super::strip::ParentRow],
        jobs: &[miyu_engine::tools::jobs::JobOverview],
    ) -> Vec<super::strip::StripItem> {
        let current = self.repl_session.lock().unwrap().clone();
        let children = self.children.lock().unwrap();
        super::strip_tree::strip_items(
            &super::strip_tree::StripScope {
                current: current.as_deref(),
                path: visits,
                children: Some(&children),
            },
            jobs,
        )
    }
}

/// Source of background-command snapshots for the idle status strip.
pub(in crate::cli) enum JobsFeed {
    /// Remote REPL: snapshots pushed by the IPC poll thread.
    Shared(std::sync::Arc<SharedJobsFeed>),
    /// Direct REPL: read the in-process registry, scoped to this REPL's
    /// session. 直连道以前直接读整张表不过滤——远端道在 poll 线程里过滤了，
    /// 两条路语义不一致，直连 REPL 会看到别的会话的后台命令。
    Local(Option<String>),
}

impl JobsFeed {
    pub(in crate::cli) fn current(&self) -> Vec<miyu_engine::tools::jobs::JobOverview> {
        match self {
            JobsFeed::Shared(shared) => shared.jobs.lock().unwrap().clone(),
            JobsFeed::Local(session) => {
                let mut jobs = miyu_engine::tools::jobs::overview();
                retain_session_jobs(&mut jobs, session.as_deref());
                jobs
            }
        }
    }

    /// The store's current Σ for the REPL's session, or `None` when this feed
    /// has no store behind it.
    pub(in crate::cli) fn cumulative(&self) -> Option<TurnTokens> {
        match self {
            JobsFeed::Shared(shared) => {
                let current = shared
                    .footer_generation
                    .load(std::sync::atomic::Ordering::Acquire);
                shared
                    .cumulative
                    .lock()
                    .unwrap()
                    .filter(|(generation, _)| *generation == current)
                    .map(|(_, totals)| totals)
            }
            JobsFeed::Local(_) => None,
        }
    }

    /// 这条 REPL 的会话上挂着的目标（`/goal`）。直连道没有 daemon，也就没有
    /// 续轮驱动器——那边永远是 None。
    pub(in crate::cli) fn goal(&self) -> Option<miyu_core::ipc::GoalHint> {
        match self {
            JobsFeed::Shared(shared) => shared.goal.lock().unwrap().clone(),
            JobsFeed::Local(_) => None,
        }
    }

    /// 这条车道开一条新会话时的上下文（事先问好的，见 `empty_context`）。还没问到
    /// 是 `None`，直连模式没有这一问。
    pub(in crate::cli) fn empty_session_context(&self, lane: PersonaLane) -> Option<u64> {
        match self {
            JobsFeed::Shared(shared) => {
                shared.empty_context.lock().unwrap()[usize::from(lane.is_dev())]
            }
            JobsFeed::Local(_) => None,
        }
    }

    /// 刚从 daemon 手里拿到一份更新的目标状态（`/goal` 命令的回执里就带着）。
    ///
    /// 必须写回这里而不只是写 footer：轮询一秒一次，这一秒里每一拍
    /// `tick_goal_hint` 都会拿这份快照去盖 footer——不同步的话「清掉的目标」
    /// 会自己回来待满一秒。
    pub(in crate::cli) fn set_goal(&self, goal: Option<miyu_core::ipc::GoalHint>) {
        if let JobsFeed::Shared(shared) = self {
            *shared.goal.lock().unwrap() = goal;
        }
    }

    pub(in crate::cli) fn take_reports(&self) -> Vec<BackgroundReport> {
        match self {
            JobsFeed::Shared(shared) => {
                let mut reports = shared.reports.lock().unwrap();
                let rendered = shared.rendered_turns.lock().unwrap();
                let taken = reports
                    .drain(..)
                    .filter(|report| !rendered.contains(&report.turn_id))
                    .collect();
                taken
            }
            JobsFeed::Local(_) => Vec::new(),
        }
    }

    /// 记下「这一轮是我自己起的」，别把它当成别人的再画一遍。
    pub(in crate::cli) fn mark_own_run(&self, run_id: &str) {
        let JobsFeed::Shared(shared) = self else {
            return;
        };
        let mut own = shared.own_runs.lock().unwrap();
        if own.len() >= JOBS_FEED_MARK_LIMIT {
            own.clear();
        }
        own.insert(run_id.to_string());
    }

    /// 这一轮已经挂上去看了（换会话后挂上它正在跑的那一轮，见 `follow_active_run_here`），
    /// 空闲循环别再当成别人的轮认领一次：那会把整轮从头再画一遍。
    pub(in crate::cli) fn mark_followed(&self, run_id: &str) {
        let JobsFeed::Shared(shared) = self else {
            return;
        };
        let mut followed = shared.followed_runs.lock().unwrap();
        if followed.len() >= JOBS_FEED_MARK_LIMIT {
            followed.clear();
        }
        followed.insert(run_id.to_string());
    }

    /// `session` 上**别人**起的、还没挂过的那一轮；认领一次就记下，免得重复挂。
    ///
    /// 空闲循环里才会走到这儿，所以「我自己正在跑的轮」不会出现在这里；真正要
    /// 防的是刚跑完那一瞬间（daemon 还没把它从活跃表摘掉），靠 `own_runs`。
    pub(in crate::cli) fn claim_peer_run(&self, session: &str) -> Option<(String, String)> {
        let JobsFeed::Shared(shared) = self else {
            return None;
        };
        let peer_runs = shared.peer_runs.lock().unwrap();
        let own = shared.own_runs.lock().unwrap();
        let mut followed = shared.followed_runs.lock().unwrap();
        for (run_id, run_session) in peer_runs.iter() {
            if run_session != session || own.contains(run_id) || followed.contains(run_id) {
                continue;
            }
            if followed.len() >= JOBS_FEED_MARK_LIMIT {
                followed.retain(|id| peer_runs.iter().any(|(r, _)| r == id));
            }
            followed.insert(run_id.clone());
            return Some((run_id.clone(), String::new()));
        }
        None
    }

    /// Next wake run in `session` that has not been followed yet; marks it
    /// followed so the caller attaches exactly once.
    pub(in crate::cli) fn claim_wake_run(&self, session: &str) -> Option<WakeRun> {
        let JobsFeed::Shared(shared) = self else {
            return None;
        };
        let wake_runs = shared.wake_runs.lock().unwrap();
        let mut followed = shared.followed_runs.lock().unwrap();
        for run in wake_runs.iter() {
            if run.session_id == session && !followed.contains(&run.run_id) {
                if followed.len() >= JOBS_FEED_MARK_LIMIT {
                    followed.retain(|id| wake_runs.iter().any(|run| &run.run_id == id));
                }
                followed.insert(run.run_id.clone());
                return Some(run.clone());
            }
        }
        None
    }
}

/// Poll the daemon for background commands while the remote REPL idles:
/// 1s when commands are live, 3s when quiet — a unix-socket roundtrip
/// costs microseconds either way.
/// `session` 是这个 REPL 起步时的会话：轮询线程第一次拉任务表就按它过滤。原来要等
/// 主循环转到第一圈才写进来，在那之前拉到的一份没有过滤。
pub(in crate::cli) fn spawn_jobs_poll_thread(
    paths: MiyuPaths,
    session: &str,
) -> std::sync::Arc<SharedJobsFeed> {
    let shared = std::sync::Arc::new(SharedJobsFeed::default());
    shared.set_repl_session(session);
    let _ = FEED.set(shared.clone());
    let feed = shared.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        // Track per-session watermarks so wake replies print exactly once,
        // and never replay history from before this REPL started.
        let mut seen: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let viewer = presence_viewer_id();
        // The store open can lose a race against daemon writes (SQLITE_BUSY);
        // retry every cycle instead of deciding at startup forever.
        let mut store: Option<StateStore> = None;
        // 两条车道的空会话上下文只问一次：配置不变它就不变（daemon 那侧也按配置缓存
        // 着）。问不到（老 daemon 不认这条命令）就作罢，footer 照旧显示「—」。
        let mut asked_empty_context = false;
        loop {
            if store.is_none() {
                store = StateStore::new(&paths).ok();
            }
            if !std::mem::replace(&mut asked_empty_context, true) {
                for lane in [PersonaLane::Active, PersonaLane::Dev] {
                    let tokens = runtime.block_on(async {
                        tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            fetch_empty_session_context(&paths, lane),
                        )
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .flatten()
                    });
                    feed.empty_context.lock().unwrap()[usize::from(lane.is_dev())] = tokens;
                }
            }
            let (jobs, _daemon_session, wake_runs, peer_runs) = runtime
                .block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        fetch_jobs_overview(&paths),
                    )
                    .await
                    .unwrap_or_else(|_| Ok((Vec::new(), None, Vec::new(), Vec::new())))
                })
                .unwrap_or_default();
            feed.publish_jobs(jobs);
            let repl_session = { feed.repl_session.lock().unwrap().clone() };
            *feed.wake_runs.lock().unwrap() = wake_runs;
            *feed.peer_runs.lock().unwrap() = peer_runs;
            // 目标按**这个 REPL 的会话**单独问一次：任务总览回的那份
            // `SessionState` 说的是 daemon 的当前会话，跟 REPL 的会话常常不是
            // 一条（`GetReplSession` 不动当前会话指针）。
            if let Some(session) = repl_session.as_deref() {
                // 在线登记：这个终端开着这条会话，别的会话里的 AI 发消息时列得出它。
                // 退出不用注销，daemon 那边十几秒没收到就算关了。
                let _ = runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        report_presence(&paths, &viewer, session),
                    )
                    .await
                });
                let goal = runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        fetch_goal_status(&paths, session),
                    )
                    .await
                    .unwrap_or(Ok(None))
                });
                if let Ok(goal) = goal {
                    *feed.goal.lock().unwrap() = goal;
                }
                // 这条会话名下的子代理会话：任务条列它们，点进去看（会话项目第 3 段）；在子会话
                // 里还要访问路径上每一层名下的（树从主会话画起，09-26）。超时、出错就留着上一份，
                // 不清空——清了任务条会闪。
                let path = feed.visit_path.lock().unwrap().clone();
                for owner in std::iter::once(session).chain(path.iter().map(String::as_str)) {
                    let rows = runtime.block_on(async {
                        tokio::time::timeout(
                            std::time::Duration::from_millis(500),
                            super::strip::fetch_subagent_rows(&paths, owner),
                        )
                        .await
                    });
                    if let Ok(Ok(rows)) = rows {
                        feed.publish_children(owner, rows);
                    }
                }
            }
            if let (Some(store), Some(session)) = (store.as_ref(), repl_session.as_deref()) {
                // 代次在**读库之前**取：读的中途 footer 被刷新了，这份就算旧的。
                let generation = feed
                    .footer_generation
                    .load(std::sync::atomic::Ordering::Acquire);
                if let Ok(totals) = store.pinned(session).session_cumulative_token_totals() {
                    *feed.cumulative.lock().unwrap() = Some((generation, totals));
                }
            }
            // 唤醒轮在这个终端挂上去之前就跑完了：从库里补印。按**这个 REPL 的
            // 会话**查——任务总览回的会话是 daemon 的当前指针，终端开着别的会话时
            // 两者不是一条，按它查会把别的会话的汇报（09-23 起还有跨会话消息）
            // 印进这里。
            if let (Some(store), Some(session_id)) = (store.as_ref(), repl_session.clone()) {
                let watermark = match seen.entry(session_id.clone()) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let latest = store.latest_turn_seq(&session_id).unwrap_or(0);
                        *entry.insert(latest)
                    }
                };
                if let Ok(rows) = store.background_report_replies_after(&session_id, watermark) {
                    for row in rows {
                        seen.insert(session_id.clone(), row.seq);
                        if feed.rendered_turns.lock().unwrap().contains(&row.turn_id) {
                            continue;
                        }
                        let turn_end = (row.status != "failed")
                            .then(|| {
                                render::timeline::turn_end_span(
                                    row.started_at.as_deref(),
                                    row.finished_at.as_deref(),
                                )
                            })
                            .flatten()
                            .map(|(elapsed, finished_at)| ReportTurnEnd {
                                model: row.assistant_model.clone(),
                                elapsed,
                                finished_at,
                                interrupted: row.status == "interrupted",
                            });
                        feed.reports.lock().unwrap().push(BackgroundReport {
                            turn_id: row.turn_id,
                            headline: row.display_content,
                            reply: row.reply,
                            job_report: row.job_report,
                            turn_end,
                        });
                    }
                }
            }
            std::thread::sleep(POLL_EVERY);
        }
    });
    shared
}

/// 轮询任务总览的间隔。原来这一秒里还按 150ms 跟后台子代理浮层的标记流（顺带就是
/// 这条轮询唯一的 sleep），浮层 09-25 退役，只剩这一下。
const POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(1000);

/// `(任务总览, daemon 当前会话, 唤醒轮, 人起的活跃轮)`。
///
/// 最后一项是 `(run_id, session_id)`：同一个会话的另一个客户端靠它发现
/// 「这儿有一轮在跑」并挂上去。
pub(in crate::cli) type JobsOverviewSnapshot = (
    Vec<miyu_engine::tools::jobs::JobOverview>,
    Option<String>,
    Vec<WakeRun>,
    Vec<(String, String)>,
);

/// daemon 替会话起的一轮（后台任务跑完、目标续轮、跨会话消息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct WakeRun {
    pub(in crate::cli) run_id: String,
    pub(in crate::cli) session_id: String,
    pub(in crate::cli) label: String,
    /// 挂上去时从这一轮开头补：跨会话消息起的轮，开头那条消息就是要看的内容
    /// （09-23）。后台任务汇报照旧只接实时，抬头由 `label` 画。
    pub(in crate::cli) from_start: bool,
}

/// 一条会话此刻的目标（`/goal`）。
///
/// 单开一条命令是因为另外两条都不合用：任务总览回的 `SessionState` 说的是
/// daemon 的当前会话，而 `GetSessionState` 对非当前会话要现装一个 Agent 估
/// 上下文——一秒一次的轮询用不起。
pub(in crate::cli) async fn fetch_goal_status(
    paths: &MiyuPaths,
    session_id: &str,
) -> Result<Option<miyu_core::ipc::GoalHint>> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::GoalStatus {
            target: miyu_core::ipc::SessionRef::Id {
                id: session_id.to_string(),
            },
        }),
    )
    .await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { data, .. }) => Ok(goal_hint_from_admin_data(&data)),
        _ => Ok(None),
    }
}

/// 这个终端的窗口编号：进程号加启动时刻，同一台机器上不会撞。
fn presence_viewer_id() -> String {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("tui-{}-{started:x}", std::process::id())
}

async fn report_presence(paths: &MiyuPaths, viewer: &str, session_id: &str) -> Result<()> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::Presence {
            viewer: viewer.to_string(),
            session: Some(session_id.to_string()),
        }),
    )
    .await?;
    let _ = ipc::receive::<IpcFrame>(&mut stream).await?;
    Ok(())
}

/// 这条车道开一条新会话时的上下文（`EmptySessionContext`）。daemon 不认这条命令
/// 或者没带数，都是 `None`。
pub(in crate::cli) async fn fetch_empty_session_context(
    paths: &MiyuPaths,
    lane: PersonaLane,
) -> Result<Option<u64>> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::EmptySessionContext {
            mode: lane.is_dev().then(|| "dev".to_string()),
        }),
    )
    .await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { data, .. }) => Ok(data
            .get("context_tokens")
            .and_then(serde_json::Value::as_u64)),
        _ => Ok(None),
    }
}

/// `AdminResult` 的 data 里那份目标状态。`/goal` 命令的回执也带同一个键——
/// 解析只此一处，两条路的形状不会分叉。
pub(in crate::cli) fn goal_hint_from_admin_data(
    data: &serde_json::Value,
) -> Option<miyu_core::ipc::GoalHint> {
    serde_json::from_value(data.get("goal")?.clone()).ok()?
}

pub(in crate::cli) async fn fetch_jobs_overview(paths: &MiyuPaths) -> Result<JobsOverviewSnapshot> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(&mut stream, &IpcRequest::new(IpcCommand::JobsOverview)).await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { state, data }) => {
            let peer_runs = data
                .get("peer_runs")
                .and_then(serde_json::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some((
                                row.get("run_id")?.as_str()?.to_string(),
                                row.get("session_id")?.as_str()?.to_string(),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let wake_runs = data
                .get("wake_runs")
                .and_then(serde_json::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some(WakeRun {
                                run_id: row.get("run_id")?.as_str()?.to_string(),
                                session_id: row.get("session_id")?.as_str()?.to_string(),
                                label: row
                                    .get("label")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                from_start: row
                                    .get("from_start")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok((
                data.get("jobs")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .unwrap_or_default()
                    .unwrap_or_default(),
                Some(state.session_id),
                wake_runs,
                peer_runs,
            ))
        }
        _ => Ok((Vec::new(), None, Vec::new(), Vec::new())),
    }
}
