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

/// Status strip under the footer: a leading blank line, then one line per
/// background command with a blank line between entries. Timers are
/// right-aligned to the terminal width.
/// `hovered`:鼠标正悬在哪一条上(下标),那一行不 dim——和正文里可点的块一个规矩:
/// 悬浮提亮,好让人知道这行能点(用户 09-18:任务条行悬浮没有高亮)。
pub(in crate::cli) fn background_job_lines(
    jobs: &[miyu_engine::tools::jobs::JobOverview],
    spinner_phase: usize,
    cols: usize,
    hovered: Option<usize>,
) -> Vec<String> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let kind_label = |job: &miyu_engine::tools::jobs::JobOverview| match job.kind.as_str() {
        // 开发模式的子代理单列一类：那一条是去写代码的，「开发中」比「子代理」
        // 更说明它在干嘛。
        "dev" => miyu_base::i18n::text("dev", "开发中"),
        "subagent" => miyu_base::i18n::text("agent", "子代理"),
        _ => miyu_base::i18n::text("cmd", "命令"),
    };
    // Pad kinds to one column so mixed command/subagent rows keep their ids
    // and titles vertically aligned.
    let kind_col = jobs
        .iter()
        .map(|job| visible_width(kind_label(job)))
        .max()
        .unwrap_or(0);
    let mut lines = vec![String::new()];
    for (index, job) in jobs.iter().enumerate() {
        let marker = JOB_SPINNER_FRAMES[spinner_phase % JOB_SPINNER_FRAMES.len()];
        let kind_word = kind_label(job);
        let kind_pad = " ".repeat(kind_col.saturating_sub(visible_width(kind_word)));
        // 后代的任务(子代理开的后台命令、后台孙代理)前面挂个 ↳,看得出不是这一层开的。
        let nested = job
            .root_session_id
            .as_deref()
            .is_some_and(|root| job.session_id.as_deref() != Some(root));
        let mut left = format!(
            "{marker} {kind_word}{kind_pad} {}{} · {}",
            if nested { "↳ " } else { "" },
            job.job_id,
            job.title
        );
        // 时间左边先报量：一条子代理跑五分钟，光有秒数看不出它是在干活还是
        // 卡住了（用户：这里时间左侧应该有一个 token 记述）。命令类任务没有
        // 这个概念，那儿就是空的。
        let timer = match job.metric.as_deref().filter(|text| !text.trim().is_empty()) {
            Some(metric) => format!(
                "{}  {}",
                metric.trim(),
                format_job_duration(job.runtime_seconds)
            ),
            None => format_job_duration(job.runtime_seconds),
        };
        let timer_width = visible_width(&timer);
        // Never exceed the terminal width: a wrapped strip line would shift
        // the whole tail and flicker.
        let max_left = cols.saturating_sub(timer_width).saturating_sub(2);
        while visible_width(&left) > max_left && !left.is_empty() {
            left.pop();
        }
        let left_width = visible_width(&left);
        let pad = cols
            .saturating_sub(left_width)
            .saturating_sub(timer_width)
            .max(1);
        let dim = if hovered == Some(index) {
            ""
        } else {
            "\x1b[2m"
        };
        lines.push(format!("{dim}{left}{}{timer}\x1b[0m", " ".repeat(pad)));
    }
    lines
}

/// Strips the bracketed prefix off a background-job wake headline, leaving
/// `子代理完成 82bea3 · 标题`. The older `[后台命令完成] ` spelling still shows
/// up in sessions recorded before the rename.
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
}

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
    /// 后台子代理面板**正开着**哪个任务。有值这条轮询就顺带拉它的原始标记流。
    pub(in crate::cli) trace_job: std::sync::Mutex<Option<String>>,
    /// 拉回来的那份：`(job_id, 全部标记, 游标)`。面板每帧读它，攒步照旧是无状态
    /// 重算——比 150ms 重读整份日志便宜，而且不受「按自然段落盘」那道闸的限制。
    pub(in crate::cli) trace: std::sync::Mutex<Option<(String, Vec<String>, u64)>>,
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
pub(in crate::cli) fn spawn_jobs_poll_thread(paths: MiyuPaths) -> std::sync::Arc<SharedJobsFeed> {
    let shared = std::sync::Arc::new(SharedJobsFeed::default());
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
            let mut jobs = jobs;
            let repl_session = { feed.repl_session.lock().unwrap().clone() };
            retain_session_jobs(&mut jobs, repl_session.as_deref());
            *feed.jobs.lock().unwrap() = jobs;
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
                    for (seq, turn_id, display, reply) in rows {
                        seen.insert(session_id.clone(), seq);
                        if feed.rendered_turns.lock().unwrap().contains(&turn_id) {
                            continue;
                        }
                        feed.reports.lock().unwrap().push(BackgroundReport {
                            turn_id,
                            headline: display,
                            reply,
                        });
                    }
                }
            }
            // 面板开着的时候按 150ms 跟标记流：整份任务总览一秒一次就够，但面板
            // 要的是「它还活着」的手感。拉不到就什么都不动，面板自己退回读日志。
            for _ in 0..TRACE_TICKS_PER_POLL {
                let want = { feed.trace_job.lock().unwrap().clone() };
                if let Some(job_id) = want {
                    let after = {
                        let trace = feed.trace.lock().unwrap();
                        match trace.as_ref() {
                            Some((id, _, cursor)) if *id == job_id => *cursor,
                            _ => 0,
                        }
                    };
                    if let Ok((markers, cursor, reset)) =
                        runtime.block_on(fetch_job_trace(&paths, &job_id, after))
                    {
                        let mut slot = feed.trace.lock().unwrap();
                        match slot.as_mut() {
                            // 接着上次那份往后攒。`reset` = 缓冲把中间挤掉了／任务
                            // 已经不在 daemon 里，这份得从头算。
                            Some((id, seen, at)) if *id == job_id && !reset => {
                                seen.extend(markers);
                                *at = cursor;
                            }
                            _ => *slot = Some((job_id.clone(), markers, cursor)),
                        }
                    }
                }
                std::thread::sleep(TRACE_TICK);
            }
        }
    });
    shared
}

/// 面板跟标记流的节奏。和原来重读日志那个间隔一样——换的是「读什么」，不是
/// 「多久读一次」。
const TRACE_TICK: std::time::Duration = std::time::Duration::from_millis(150);
/// 一轮总览（1s）里跟几次标记流。
const TRACE_TICKS_PER_POLL: usize = 7;

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

/// 后台子代理的原始进度标记，从绝对序号 `after` 之后取。
///
/// 返回 `(标记, 新游标, 要不要重新攒)`。`reset` 为真有两种情形：环形缓冲把 `after`
/// 挤掉了，或者这个任务在 daemon 里已经不在了（跑完清掉、daemon 重启过）——两种
/// 都得让面板退回读日志那条路。
pub(in crate::cli) async fn fetch_job_trace(
    paths: &MiyuPaths,
    job_id: &str,
    after: u64,
) -> Result<(Vec<String>, u64, bool)> {
    let mut stream = ipc::connect(&paths.ipc_socket()).await?;
    ipc::send(
        &mut stream,
        &IpcRequest::new(IpcCommand::JobTrace {
            job_id: job_id.to_string(),
            after,
        }),
    )
    .await?;
    match ipc::receive::<IpcFrame>(&mut stream).await? {
        Some(IpcFrame::AdminResult { data, .. }) => Ok((
            data.get("markers")
                .and_then(serde_json::Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            data.get("cursor")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(after),
            data.get("reset")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        )),
        other => anyhow::bail!("unexpected frame for job trace: {other:?}"),
    }
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
