//! Best-effort desktop notifications.
//!
//! Shells out to whatever the platform provides rather than pulling in a
//! notification crate — same trade the clipboard backends make. Every failure
//! is swallowed: a machine without a notification daemon, a headless session,
//! or a sandbox that blocks spawning must never turn a notification into a
//! user-visible error.

use std::process::{Command, Stdio};

/// 通知的提示音。名字取自 XDG 声音主题（freedesktop sound naming spec），
/// 由终端/系统按用户当前的声音主题去找文件，所以仓库里不用带音频资产。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifySound {
    /// 回合跑完了。
    TurnDone,
    /// 有人在等你回话——比 [`NotifySound::TurnDone`] 急促。
    Question,
}

/// 这条通知响什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifyTone {
    /// 系统声音主题里的那一声。
    Theme(NotifySound),
    /// 用户自己指定的音频文件。
    File(std::path::PathBuf),
    Silent,
}

impl NotifyTone {
    /// kitty 的 `s=` 只认声音主题里的**名字**，喂不了文件路径——自定义文件时
    /// 让它闭嘴（`silent`），那个文件由我们自己放。
    fn theme_name(&self) -> &str {
        match self {
            NotifyTone::Theme(sound) => sound.xdg_name(),
            NotifyTone::File(_) | NotifyTone::Silent => "silent",
        }
    }
}

impl NotifySound {
    /// XDG 声音主题里的名字。
    fn xdg_name(self) -> &'static str {
        match self {
            NotifySound::TurnDone => "complete",
            NotifySound::Question => "message-new-instant",
        }
    }

    /// macOS 系统自带音（`display notification … sound name`）。
    fn macos_name(self) -> &'static str {
        match self {
            NotifySound::TurnDone => "Glass",
            NotifySound::Question => "Ping",
        }
    }

    /// 内置提示音：文件名 + 字节。`assets/voice/` 里的木琴音本来是给语音功能
    /// 做的，09-18 用户试听后挑了它们当默认通知音——不是每台机器都装了 XDG
    /// 声音主题，而这一声是 Miyu 自己的。
    ///
    /// 两个事件现在**用同一段**（用户挑的 `wake`，上行两音），所以盘上也只落
    /// 一份；哪天分开了，换成两个名字就行。
    fn builtin_asset(self) -> (&'static str, &'static [u8]) {
        match self {
            NotifySound::TurnDone | NotifySound::Question => {
                ("wake.wav", include_bytes!("../../../assets/voice/wake.wav"))
            }
        }
    }
}

/// 把内置提示音落到缓存目录并返回路径——外挂播放器只认文件，内嵌的字节得先
/// 上盘。字节数对不上就重写（换了版本音变了也跟得上）。
pub fn builtin_sound_file(sound: NotifySound) -> Option<std::path::PathBuf> {
    let (name, bytes) = sound.builtin_asset();
    let path = crate::paths::miyu_home_dir()?
        .join("cache")
        .join("sounds")
        .join(name);
    if std::fs::metadata(&path)
        .map(|meta| meta.len() == bytes.len() as u64)
        .unwrap_or(false)
    {
        return Some(path);
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// Spawns the notification and returns immediately. The child is detached; we
/// never wait on it, so a hung helper cannot stall a turn.
pub fn notify(title: &str, body: &str) {
    notify_with_sound(title, body, &NotifyTone::Silent);
}

/// 同 [`notify`]，另外放一声提示音。
///
/// 声音走的是和通知同一条「外挂命令」路线：通知守护进程里只有一部分认
/// `sound-name` 提示（mako 就不认），指望不上，所以 Linux 侧自己放。
pub fn notify_with_sound(title: &str, body: &str, tone: &NotifyTone) {
    if cfg!(target_os = "macos") {
        // `display notification` takes AppleScript string literals, so quotes
        // and backslashes in model-authored text have to be escaped.
        // 声音是这句自带的子句，不用另开进程；自定义文件 AppleScript 放不了，
        // 单开 afplay。
        let sound_clause = match tone {
            NotifyTone::Theme(sound) => format!(" sound name \"{}\"", sound.macos_name()),
            NotifyTone::File(_) | NotifyTone::Silent => String::new(),
        };
        let script = format!(
            "display notification \"{}\" with title \"{}\"{sound_clause}",
            applescript_escape(body),
            applescript_escape(title)
        );
        spawn("osascript", &["-e", &script]);
        play_tone(tone);
        return;
    }
    if cfg!(target_os = "windows") {
        // 气泡自带系统音，`sound` 在这条路上无事可做。
        let script = format!(
            "[System.Reflection.Assembly]::LoadWithPartialName('System.Windows.Forms') | Out-Null; \
             $n = New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon = [System.Drawing.SystemIcons]::Information; \
             $n.Visible = $true; $n.ShowBalloonTip(5000, '{}', '{}', 'Info')",
            powershell_escape(title),
            powershell_escape(body)
        );
        spawn("powershell", &["-NoProfile", "-Command", &script]);
        return;
    }
    // Linux/BSD: notify-send is the de-facto interface, and `--` keeps a body
    // starting with a dash from being read as a flag.
    spawn("notify-send", &["-a", "Miyu", "--", title, body]);
    play_tone(tone);
}

/// 把通知交给 kitty 自己弹（通知协议 OSC 99）。发出去了返回 `true`。
///
/// 比 `notify-send` 多两件后者给不了的事：
/// - **点通知能跳回发它的那个窗口**：Wayland 下外部进程抢不了焦点，得由窗口
///   自己拿通知守护进程发的 xdg-activation token 去激活自己；kitty 的通知默认
///   就带 `focus` 动作（09-18 在 niri + mako + kitty 0.48 上实测走通整条链）。
/// - **有声音**：`s=` 让 kitty 按 XDG 声音主题自己播一声，不看通知守护进程支
///   不支持声音（mako 就不支持）。
///
/// 只认原生 kitty（`TERM=xterm-kitty`）：别的终端要么不认这串转义，要么直接把
/// 它当正文打出来。stdout 不是终端时同理不发。herdr 里也不发：它转得了 kitty
/// 的图，却吞掉这串通知（见 `is_kitty_itself`）。
pub fn notify_via_kitty(title: &str, body: &str, tone: &NotifyTone) -> bool {
    use std::io::{IsTerminal, Write};

    if !crate::terminal::kitty::is_kitty_itself() {
        return false;
    }
    let mut stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return false;
    }
    // 同一个进程始终复用一个 id：kitty 拿新的那条替掉旧的，通知栏里这个 REPL
    // 永远只占一格。带 pid 是为了两个 TUI 各占各的，别互相顶掉。
    let sequence = kitty_sequence(&format!("miyu-{}", std::process::id()), title, body, tone);
    // 一次写完：分两次写会被别的线程从中间插进来，转义序列劈两半就成乱码了。
    stdout.write_all(sequence.as_bytes()).is_ok() && stdout.flush().is_ok()
}

/// kitty 通知协议的那两段转义序列：标题一段、正文一段，同一个 `i=` 拼起来。
fn kitty_sequence(ident: &str, title: &str, body: &str, tone: &NotifyTone) -> String {
    use base64::Engine as _;
    let b64 = |value: &str| base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
    let app = b64("Miyu");
    let sound = b64(tone.theme_name());
    // o=unfocused：让 kitty 自己判有没有焦点——它连「在非当前标签页」都算得准，
    // 比终端上报的焦点靠谱；e=1 让标题正文走 base64，免得里头的 `;` `:` 把元
    // 数据截断；正文是第二段，`d=1` 收尾。
    format!(
        "\x1b]99;i={ident}:e=1:d=0:a=focus:o=unfocused:u=1:f={app}:s={sound}:p=title;{}\x1b\\\
         \x1b]99;i={ident}:e=1:d=1:p=body;{}\x1b\\",
        b64(title),
        b64(body)
    )
}

/// 自己放一声提示音。
///
/// 和通知一样是外挂命令、失败全吞。主题音先用 libcanberra——它按用户的声音主题
/// 找文件，和 kitty 那条路同名同音；没装就退到直接放 freedesktop 主题里的那个
/// 文件。都没有就安静，不报错。
pub fn play_tone(tone: &NotifyTone) {
    let path = match tone {
        NotifyTone::Silent => return,
        NotifyTone::File(path) => path.clone(),
        NotifyTone::Theme(sound) => {
            if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
                // macOS 的主题音写在 AppleScript 那句里、Windows 的气泡自带系
                // 统音，都不用另开进程。
                return;
            }
            let name = sound.xdg_name();
            if spawn_ok("canberra-gtk-play", &["-i", name]) {
                return;
            }
            std::path::PathBuf::from(format!("/usr/share/sounds/freedesktop/stereo/{name}.oga"))
        }
    };
    if !path.is_file() {
        return;
    }
    let path = path.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        spawn("afplay", &[&path]);
        return;
    }
    // canberra 排第一:和主题音同一个播放器,音量/设备行为一致。
    for (player, args) in [
        ("canberra-gtk-play", vec!["-f", &path]),
        ("pw-play", vec![path.as_str()]),
        ("paplay", vec![path.as_str()]),
        (
            "ffplay",
            vec!["-nodisp", "-autoexit", "-loglevel", "quiet", &path],
        ),
    ] {
        if spawn_ok(player, &args) {
            return;
        }
    }
}

/// 同 [`notify`],但在 Linux 上走 `notify-send -p`(打印通知 id)并用 `-r`
/// 替换上一条,让一串相关通知(「在听」→「收到」→ 回复)只占一个气泡。
/// 返回本条的 id 供下次替换;拿不到 id(旧版 notify-send 不认 `-p`、非 Linux)
/// 时退回普通通知并返回 None。
///
/// 会等 notify-send 退出(它发完就退,几十毫秒),调用方应放在专用线程里
/// 保序,别堵在回合上。
pub fn notify_replacing(title: &str, body: &str, previous: Option<u32>) -> Option<u32> {
    if !cfg!(target_os = "linux") {
        notify(title, body);
        return None;
    }
    let mut args: Vec<String> = vec!["-a".into(), "Miyu".into(), "-p".into()];
    if let Some(id) = previous {
        args.push("-r".into());
        args.push(id.to_string());
    }
    args.extend(["--".to_string(), title.to_string(), body.to_string()]);
    let output = Command::new("notify-send")
        .args(&args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .ok(),
        Ok(_) => {
            // 老版本不认 -p/-r:退回普通发送,链条断了也别丢通知。
            spawn("notify-send", &["-a", "Miyu", "--", title, body]);
            None
        }
        Err(_) => None,
    }
}

fn spawn(program: &str, args: &[&str]) {
    let _ = spawn_ok(program, args);
}

/// 起得来返回 true——只说明这台机器上有这个命令，不保证它干成了事。
fn spawn_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn applescript_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

fn powershell_escape(value: &str) -> String {
    value.replace('\'', "''").replace('\n', " ")
}

/// Clips a notification body to something a popup can actually show.
pub fn clip_body(value: &str, max_chars: usize) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= max_chars {
        return single_line;
    }
    let kept: String = single_line.chars().take(max_chars).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_keeps_helper_arguments_intact() {
        assert_eq!(applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(applescript_escape("line\nbreak"), "line break");
        assert_eq!(powershell_escape("it's"), "it''s");
    }

    #[test]
    fn body_is_clipped_to_one_line() {
        assert_eq!(clip_body("  a\n  b  ", 10), "a b");
        assert_eq!(clip_body("abcdefghij", 5), "abcde…");
    }

    #[test]
    fn a_missing_backend_is_silent() {
        // The point of the module: no panic, no error, no blocking, even when
        // nothing on the machine can show a notification.
        spawn("miyu-nonexistent-notification-backend", &["x"]);
    }

    /// 转义序列的形状是外部协议(kitty notifications),写错了不会报错、只会
    /// 在终端里变成一行乱码,所以这里把它拆开逐项对。
    #[test]
    fn kitty_sequence_shape() {
        use base64::Engine as _;
        let sequence = kitty_sequence(
            "miyu-42",
            "标题",
            "正文",
            &NotifyTone::Theme(NotifySound::Question),
        );
        let decode = |value: &str| {
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .unwrap(),
            )
            .unwrap()
        };
        let (title_chunk, body_chunk) = sequence.split_once("\x1b\\").unwrap();
        let (meta, payload) = title_chunk
            .strip_prefix("\x1b]99;")
            .unwrap()
            .split_once(';')
            .unwrap();
        let fields: std::collections::HashMap<&str, &str> = meta
            .split(':')
            .filter_map(|part| part.split_once('='))
            .collect();
        assert_eq!(fields["i"], "miyu-42");
        assert_eq!(fields["d"], "0", "标题不是最后一段");
        assert_eq!(fields["a"], "focus", "点通知要能跳回这个窗口");
        assert_eq!(fields["o"], "unfocused", "有焦点时由 kitty 自己扣下");
        assert_eq!(fields["p"], "title");
        assert_eq!(decode(fields["s"]), "message-new-instant");
        assert_eq!(decode(fields["f"]), "Miyu");
        assert_eq!(decode(payload), "标题");

        let (meta, payload) = body_chunk
            .strip_prefix("\x1b]99;")
            .unwrap()
            .strip_suffix("\x1b\\")
            .unwrap()
            .split_once(';')
            .unwrap();
        let fields: std::collections::HashMap<&str, &str> = meta
            .split(':')
            .filter_map(|part| part.split_once('='))
            .collect();
        assert_eq!(fields["i"], "miyu-42", "两段得用同一个 id 才拼得起来");
        assert_eq!(fields["d"], "1", "正文是最后一段");
        assert_eq!(fields["p"], "body");
        assert_eq!(decode(payload), "正文");
    }

    /// 静音和「自己放文件」在 kitty 眼里是同一件事：别替我放。
    #[test]
    fn kitty_is_told_to_stay_quiet_unless_it_owns_the_sound() {
        use base64::Engine as _;
        let silent = base64::engine::general_purpose::STANDARD.encode("silent");
        for tone in [
            NotifyTone::Silent,
            NotifyTone::File("/tmp/miyu-ding.wav".into()),
        ] {
            let sequence = kitty_sequence("miyu-1", "t", "b", &tone);
            assert!(
                sequence.contains(&format!("s={silent}")),
                "{tone:?} 不该让 kitty 出声"
            );
        }
    }
}
