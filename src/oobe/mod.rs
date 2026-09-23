//! 新手引导（OOBE）。
//!
//! 入口两个：新用户第一次跑裸 `miyu`（`config.oobe_done` 还是 false），或者
//! 手动 `miyu oobe`。`miyu init` 仍是非交互的脚本化初始化（建目录、写默认配置、
//! 导默认知识库），**不**标记引导完成——脚本部署完的机器上第一次由人打开 TUI
//! 照样走一遍引导，只是已经装好的东西（shell hook）会显示成「已经装了」。
//! 旧版本升上来的配置在 `config_version` 迁移里直接标成完成，老用户不会被拦。
//!
//! 六屏：人格 → 功能 → 认识你 → 终端集成 → 沙盒模式 → 接模型。每屏「下一步」即落盘
//! （[`apply`]），接模型放最后是为了让 CLI 目录的探测藏在前五屏后面跑。
//! 不做完成页：走完直接进 REPL，空会话的 banner 就是第一帧。
//!
//! 画面是备用屏上的 ratatui；控制流独占 stdin，跑完再把终端交还给 REPL。

mod apply;
mod probe;
mod providers;
mod ui;

use anyhow::Result;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use miyu_base::config::AppConfig;
use miyu_base::paths::MiyuPaths;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::time::{Duration, Instant};

pub(crate) use ui::Outcome;

/// 裸 `miyu` 要不要先走引导。
pub(crate) fn needed(config: &AppConfig) -> bool {
    !config.oobe_done
}

/// 终端状态守卫：不管怎么退出（含 panic），都把 raw mode 还回去。
///
/// `keep_alt`：紧接着要进全屏 REPL 的话，备用屏**不退**——退了再进，中间会闪
/// 一下用户的 shell 画面。退出前把备用屏擦干净，交给下一个画面。
struct TerminalGuard {
    keep_alt: bool,
}

impl TerminalGuard {
    fn enter(keep_alt: bool) -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        Ok(Self { keep_alt })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.keep_alt {
            // 画面留着、光标不动：全屏 REPL 接手时直接在这一帧上重画,
            // 中间既不闪也不把光标甩到左上角。
            miyu_base::terminal::hold_alt_screen();
        } else {
            let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        }
        let _ = disable_raw_mode();
    }
}

/// 跑一遍引导。返回怎么收的场；配置已经写好（或按 [`Outcome::Aborted`] 一个字没写）。
///
/// `keep_alt_screen`：调用方紧接着要进全屏画面，备用屏留着别退。
pub fn run(paths: &MiyuPaths, keep_alt_screen: bool) -> Result<Outcome> {
    let config = AppConfig::load_or_default(paths)?;
    // models.dev 目录：选完模型按它补上下文窗口与模态（和设置界面同一套动作）。
    miyu_base::models_cache::try_load(paths);
    miyu_base::models_cache::spawn_background_refresh(paths.clone());
    let mut app = ui::App::new(config, paths.clone());
    let guard = TerminalGuard::enter(keep_alt_screen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    // 备用屏理应是空的，但不是每个终端都真有备用屏（tmux 关了 alternate-screen、
    // pyte 之类的模拟器）：先整屏擦一次，ratatui 的 diff 才不会把旧字留在屏上。
    terminal.clear()?;

    // 导航态下输入法开着会把 jk 吃进候选框，所以先关；退出时还原（见 restore）。
    app.fcitx.set(probe::Ime::Off);

    let tick = Duration::from_millis(30);
    let mut last = Instant::now();
    // 不在编辑态时光标停在上一次插入点,不往左上角跑(kitty cursor_trail 会画尾巴)。
    let mut parked: (u16, u16) = (0, 0);

    let result = loop {
        // 一帧一个同步块，先把光标藏起来再画：ratatui 写内容那一路光标跟着每个
        // MoveTo 跳（kitty 的 cursor_trail 连隐藏光标的位移都画），同步块让终端
        // 只按帧末的位置算——编辑时停在插入点，其余时候停在左上角。
        let _ = execute!(io::stdout(), BeginSynchronizedUpdate, Hide);
        if let Err(error) = terminal.draw(|frame| ui::draw::draw(frame, &mut app)) {
            let _ = execute!(io::stdout(), EndSynchronizedUpdate);
            break Err(error.into());
        }
        let _ = match app.caret_screen {
            Some((x, y)) => {
                parked = (x, y);
                execute!(io::stdout(), MoveTo(x, y), Show, EndSynchronizedUpdate)
            }
            None => execute!(
                io::stdout(),
                MoveTo(parked.0, parked.1),
                Hide,
                EndSynchronizedUpdate
            ),
        };
        match event::poll(tick.saturating_sub(last.elapsed())) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    let printed = ui::keys::on_key(&mut app, key.code, key.modifiers);
                    if printed {
                        // 装 shell hook 那种会往 stdout 打提示：整屏重画盖掉。
                        let _ = terminal.clear();
                    }
                }
                Ok(Event::Resize(_, _)) => {
                    let _ = terminal.clear();
                }
                Ok(_) => {}
                Err(error) => break Err(error.into()),
            },
            Ok(false) => {}
            Err(error) => break Err(error.into()),
        }
        if last.elapsed() >= tick {
            last = Instant::now();
            app.tick = app.tick.wrapping_add(1);
            if app.fade < 99 {
                app.fade += 1;
            }
            if app.intro < ui::INTRO_END {
                app.intro += 1;
            }
            // 自检在动画底下悄悄跑完：两帧一步，不占屏也不拖时间。
            if app.tick % 2 == 0 {
                app.loader.advance();
                app.facts_ready();
            }
            app.prefetch.pump();
            app.poll_fetch();
        }
        if let Some(outcome) = app.outcome {
            break Ok(outcome);
        }
    };

    app.fcitx.restore();
    drop(terminal);
    drop(guard);
    result
}
