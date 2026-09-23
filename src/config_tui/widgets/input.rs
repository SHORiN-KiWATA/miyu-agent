//! 设置界面的按键读取：等键的同时按 30ms 一帧推进动画，尺寸变了叫醒当前界面
//! 而不提交它。
//!
//! 「等一个窗口」这一段（[`read_window`]）是纯逻辑、可测：忽略的事件（焦点
//! 进出、鼠标）**不重置**剩余预算，否则一串杂事件能把超时无限推迟。

use crate::config_tui::*;
use std::time::Instant;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::config_tui) enum Input {
    /// 按键带上修饰键(09-23):功能表的 Ctrl+A 全开/全关要认 Ctrl,以前这里
    /// 用 `..` 把 modifiers 丢了,Ctrl+A 与裸 a 不可分。
    Key(KeyCode, KeyModifiers),
    Resize,
}

/// 在 `window` 这段时间里等一个「有意义」的事件。`None` = 窗口到期。
fn read_window(
    window: Duration,
    mut next: impl FnMut(Duration) -> io::Result<Option<Event>>,
    mut elapsed: impl FnMut() -> Duration,
) -> Result<Option<Input>> {
    loop {
        let remaining = window.saturating_sub(elapsed());
        if remaining.is_zero() {
            return Ok(None);
        }
        match next(remaining)? {
            Some(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => {
                return Ok(Some(Input::Key(code, modifiers)));
            }
            Some(Event::Resize(..)) => return Ok(Some(Input::Resize)),
            Some(_) => {}
            None => return Ok(None),
        }
    }
}

/// 等一个窗口的真实现：crossterm 的 poll + read。
pub(in crate::config_tui) fn poll_window(window: Duration) -> Result<Option<Input>> {
    let started = Instant::now();
    read_window(
        window,
        |remaining| {
            if !event::poll(remaining)? {
                return Ok(None);
            }
            event::read().map(Some)
        },
        || started.elapsed(),
    )
}

/// 菜单/编辑循环用的读键。尺寸变了回 `Null`——调用方会重新算一遍内容再画，
/// 拿旧帧糊弄会留下按旧宽度排的行。
pub(in crate::config_tui) fn read_key(ui: &mut Ui) -> Result<KeyCode> {
    Ok(match ui.wait_key(None)? {
        Some(Input::Key(key, _)) => key,
        _ => KeyCode::Null,
    })
}

/// 读键并带上修饰键。功能表的 Ctrl+A 要认 Ctrl;其余调用方用 [`read_key`]
/// 就够(它们按裸键分发)。
pub(in crate::config_tui) fn read_key_with_mods(ui: &mut Ui) -> Result<(KeyCode, KeyModifiers)> {
    Ok(match ui.wait_key(None)? {
        Some(Input::Key(code, modifiers)) => (code, modifiers),
        _ => (KeyCode::Null, KeyModifiers::empty()),
    })
}

pub(in crate::config_tui) fn read_key_with_timeout(
    ui: &mut Ui,
    timeout: Option<Duration>,
) -> Result<Option<KeyCode>> {
    Ok(match ui.wait_key(timeout)? {
        Some(Input::Key(key, _)) => Some(key),
        Some(Input::Resize) | None => None,
    })
}

/// 「按任意键继续」。尺寸变了要重画、**不**算按键。
pub(in crate::config_tui) fn wait_for_key(
    ui: &mut Ui,
    mut draw: impl FnMut(&mut Ui) -> Result<()>,
) -> Result<()> {
    loop {
        draw(ui)?;
        if matches!(ui.wait_key(None)?, Some(Input::Key(..))) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_and_real_null_key_remain_distinct() {
        let mut events = [Event::Resize(80, 24), Event::Key(KeyCode::Null.into())].into_iter();
        let mut next = |_| Ok(events.next());
        assert_eq!(
            read_window(Duration::from_millis(100), &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Resize)
        );
        assert_eq!(
            read_window(Duration::from_millis(100), &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Key(KeyCode::Null, KeyModifiers::empty()))
        );
    }

    #[test]
    fn ignored_events_do_not_restart_timeout_budget() {
        let mut polls = Vec::new();
        let mut events = [Some(Event::FocusGained), None].into_iter();
        let mut times = [0, 40, 40].into_iter();
        let result = read_window(
            Duration::from_millis(100),
            |remaining| {
                polls.push(remaining.as_millis());
                Ok(events.next().unwrap())
            },
            || Duration::from_millis(times.next().unwrap()),
        )
        .unwrap();
        assert_eq!(result, None);
        assert_eq!(polls, [100, 60]);
    }

    #[test]
    fn resize_does_not_consume_the_next_editing_key() {
        let mut events = [Event::Resize(80, 24), Event::Key(KeyCode::Char('x').into())].into_iter();
        let mut next = |_| Ok(events.next());
        assert_eq!(
            read_window(Duration::from_millis(100), &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Resize)
        );
        assert_eq!(
            read_window(Duration::from_millis(100), &mut next, || Duration::ZERO).unwrap(),
            Some(Input::Key(KeyCode::Char('x'), KeyModifiers::empty()))
        );
    }

    #[test]
    fn expired_window_polls_nothing() {
        let mut polls = 0;
        let result = read_window(
            Duration::from_millis(30),
            |_| {
                polls += 1;
                Ok(None)
            },
            || Duration::from_millis(30),
        )
        .unwrap();
        assert_eq!(result, None);
        assert_eq!(polls, 0, "窗口已经到期就不该再 poll 一次");
    }
}
