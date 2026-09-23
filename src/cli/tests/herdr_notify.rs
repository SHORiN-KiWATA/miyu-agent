//! 在 herdr 的 pane 里，回合结束 / 提问的提示音交给 herdr（用户 09-23 拍板）。
//!
//! herdr 自己按 tab 可见性放「完成」「在等你」两声（`[ui.sound]`，默认开），
//! Miyu 再响一声就成了两声；弹窗照走系统通知（herdr 吞 OSC 99，见
//! `miyu_base::terminal::kitty::is_kitty_itself`）。整条链的真机验证在
//! `testkit/herdr/real_herdr.py`。

use crate::cli::repl::jobs::notification_tone;
use miyu_base::config::AppConfig;
use miyu_base::notify::{NotifySound, NotifyTone};

/// 配一个真实存在的提示音文件：`tone()` 就不去往家目录里写内置音。
fn config_with_sound_file() -> (AppConfig, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ding.wav");
    std::fs::write(&file, b"RIFF").unwrap();
    let mut config = AppConfig::default();
    config.notifications.sound = true;
    config.notifications.sound_file = file.to_string_lossy().into_owned();
    (config, dir)
}

#[test]
fn inside_herdr_miyu_leaves_the_sound_to_herdr() {
    let (config, _dir) = config_with_sound_file();
    for sound in [NotifySound::TurnDone, NotifySound::Question] {
        assert!(
            matches!(notification_tone(&config, sound, true), NotifyTone::Silent),
            "herdr 里 Miyu 不该自己放声（{sound:?}）"
        );
    }
}

#[test]
fn outside_herdr_the_configured_sound_still_plays() {
    let (config, _dir) = config_with_sound_file();
    assert!(matches!(
        notification_tone(&config, NotifySound::TurnDone, false),
        NotifyTone::File(_)
    ));
}
