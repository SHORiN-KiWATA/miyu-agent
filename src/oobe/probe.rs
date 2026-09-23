//! 开机自检。**一帧跑一步**，藏在开场动画底下悄悄跑完，不占屏也不拖时间。
//!
//! 全部走 env / `/proc` / `PATH` 查找，只有输入法那步起了子进程
//! （`fcitx5-remote`，毫秒级）。不碰网络、不跑 `--version`——那些动辄几百毫秒，
//! 会把「载入」拖成「卡住」。CLI 的模型目录（最长 20 秒）另起线程，见
//! [`super::providers`]。
//!
//! 每步都记真实耗时。平时不显示，`MIYU_OOBE_VERBOSE=1` 调出来看——排查环境问题时有用。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

pub(super) fn which(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(bin);
            candidate.is_file() || candidate.is_symlink()
        })
    })
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// 从 `KEY=value` 这类文件里取一项，去掉引号。
fn kv(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        line.strip_prefix(key)?
            .strip_prefix('=')
            .map(|value| value.trim().trim_matches('"').to_string())
    })
}

fn join(items: &[&str]) -> String {
    items.join(" · ")
}

/// 一行自检结果。
pub(super) struct Row {
    pub label: &'static str,
    pub value: String,
    /// false = 没检出，显示成暗色的 `·` 而不是绿勾。不是错误，只是没有。
    pub ok: bool,
    pub micros: u128,
}

/// 自检步骤。顺序就是显示顺序。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Os,
    Terminal,
    Shells,
    Ime,
    Channels,
    Runtime,
    Audio,
    Network,
}

const STEPS: [Step; 8] = [
    Step::Os,
    Step::Terminal,
    Step::Shells,
    Step::Ime,
    Step::Channels,
    Step::Runtime,
    Step::Audio,
    Step::Network,
];

/// 探测完的、后面几屏还要用的东西。
#[derive(Default, Clone)]
pub(super) struct Facts {
    /// 找到的 shell，当前 shell 排第一。
    pub shells: Vec<&'static str>,
    pub current_shell: String,
    /// `claude` / `codex` / `agy` 装没装。
    pub clis: Vec<(&'static str, bool)>,
    pub env_key: bool,
    pub fcitx: bool,
    pub has_python: bool,
    pub has_audio: bool,
}

impl Facts {
    pub fn has(&self, bin: &str) -> bool {
        self.clis.iter().any(|(name, ok)| *name == bin && *ok)
            || self.shells.iter().any(|name| *name == bin)
    }
}

pub(super) struct Loader {
    pub rows: Vec<Row>,
    pub facts: Facts,
    next: usize,
    /// 色深描述由外面塞进来——`palette` 才知道最终落成哪一档。
    depth_label: &'static str,
}

impl Loader {
    pub fn new(depth_label: &'static str) -> Self {
        Self {
            rows: Vec::new(),
            facts: Facts::default(),
            next: 0,
            depth_label,
        }
    }

    pub fn done(&self) -> bool {
        self.next >= STEPS.len()
    }

    /// 跑下一步。返回 false 表示已经跑完了。
    pub fn advance(&mut self) -> bool {
        if self.done() {
            return false;
        }
        let step = STEPS[self.next];
        let started = Instant::now();
        let (label, value, ok) = self.run(step);
        self.rows.push(Row {
            label,
            value,
            ok,
            micros: started.elapsed().as_micros(),
        });
        self.next += 1;
        true
    }

    fn run(&mut self, step: Step) -> (&'static str, String, bool) {
        match step {
            Step::Os => {
                let release = read("/etc/os-release").unwrap_or_default();
                let name = kv(&release, "PRETTY_NAME")
                    .or_else(|| kv(&release, "NAME"))
                    .unwrap_or_else(|| std::env::consts::OS.to_string());
                let kernel = read("/proc/sys/kernel/osrelease")
                    .map(|text| text.trim().to_string())
                    .unwrap_or_default();
                let arch = std::env::consts::ARCH;
                let value = if kernel.is_empty() {
                    format!("{name} · {arch}")
                } else {
                    format!("{name} · {kernel} · {arch}")
                };
                ("系统", value, true)
            }
            Step::Terminal => {
                let term = env("TERM").unwrap_or_else(|| "?".into());
                let graphics = term.contains("kitty") || env("KITTY_WINDOW_ID").is_some();
                let mut parts = vec![term];
                if let Some(program) = env("TERM_PROGRAM") {
                    parts.push(program);
                }
                parts.push(self.depth_label.to_string());
                if graphics {
                    parts.push("图形协议".into());
                }
                ("终端", parts.join(" · "), true)
            }
            Step::Shells => {
                let all = ["fish", "bash", "zsh"];
                let mut found: Vec<&'static str> =
                    all.into_iter().filter(|name| which(name)).collect();
                self.facts.current_shell = env("SHELL")
                    .and_then(|shell| {
                        Path::new(&shell)
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                    })
                    .unwrap_or_default();
                let current = self.facts.current_shell.clone();
                found.sort_by_key(|name| u8::from(*name != current));
                self.facts.shells = found.clone();
                let ok = !found.is_empty();
                let value = if ok { join(&found) } else { "没找到".into() };
                ("shell", value, ok)
            }
            Step::Ime => {
                let alive = Command::new("fcitx5-remote")
                    .arg("--check")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false);
                self.facts.fcitx = alive;
                if !alive {
                    return ("输入法", "未检出".into(), false);
                }
                ("输入法", "fcitx5 · 编辑态托管".into(), true)
            }
            Step::Channels => {
                // 和中转线启动 CLI 同一个口径:官方安装脚本把 claude 放在 ~/.local/bin,
                // 拉起引导的终端 PATH 里不一定有,光查 PATH 会误报「没找到 CLI」。
                let clis: Vec<(&'static str, bool)> = ["claude", "codex", "agy", "codebuddy"]
                    .into_iter()
                    .map(|name| (name, miyu_base::paths::program_available(name)))
                    .collect();
                let keys = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "DEEPSEEK_API_KEY"];
                self.facts.env_key = keys.iter().any(|key| std::env::var_os(key).is_some());
                self.facts.clis = clis.clone();
                let found: Vec<&str> = clis
                    .iter()
                    .filter(|(_, ok)| *ok)
                    .map(|(name, _)| *name)
                    .collect();
                let mut value = if found.is_empty() {
                    "没找到 CLI".to_string()
                } else {
                    join(&found)
                };
                if self.facts.env_key {
                    value.push_str(" · 环境变量有 key");
                }
                ("模型通道", value, !found.is_empty() || self.facts.env_key)
            }
            Step::Runtime => {
                let want = ["python3", "node", "uv", "git", "rg"];
                let found: Vec<&str> = want.into_iter().filter(|name| which(name)).collect();
                self.facts.has_python = found.contains(&"python3");
                let value = if found.is_empty() {
                    "没有 python3，脚本工具用不了".into()
                } else {
                    join(&found)
                };
                ("运行时", value, self.facts.has_python)
            }
            Step::Audio => {
                let found: Vec<&str> = ["pipewire", "pulseaudio", "pactl", "wpctl"]
                    .into_iter()
                    .filter(|name| which(name))
                    .collect();
                self.facts.has_audio = !found.is_empty();
                let value = if self.facts.has_audio {
                    join(&found)
                } else {
                    "没有，语音功能不可用".into()
                };
                ("音频", value, self.facts.has_audio)
            }
            Step::Network => {
                let proxy = ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy"]
                    .iter()
                    .find_map(|key| env(key));
                match proxy {
                    Some(proxy) => ("网络", format!("走代理 {proxy}"), true),
                    None => ("网络", "直连".into(), true),
                }
            }
        }
    }
}

/// 输入法托管：导航态下输入法开着会把 `jk` 吃进候选框，所以进编辑态才开，
/// 出来就关；退出时还原到进来之前的状态。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Ime {
    Off,
    On,
}

pub(super) struct Fcitx {
    available: bool,
    /// 进来之前是什么状态。退出时还原回去——把用户的输入法留在关着是很讨厌的。
    original: Option<Ime>,
}

impl Fcitx {
    pub fn probe() -> Self {
        // 自动化跑的时候别碰真输入法：反复起停会把用户的输入法开开关关。
        if std::env::var_os("MIYU_OOBE_NO_IME").is_some() {
            return Self {
                available: false,
                original: None,
            };
        }
        let available = Command::new("fcitx5-remote")
            .arg("--check")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let mut me = Self {
            available,
            original: None,
        };
        me.original = me.state();
        me
    }

    pub fn restore(&self) {
        if let Some(state) = self.original {
            self.set(state);
        }
    }

    pub fn state(&self) -> Option<Ime> {
        if !self.available {
            return None;
        }
        let output = Command::new("fcitx5-remote").output().ok()?;
        match String::from_utf8_lossy(&output.stdout).trim() {
            "2" => Some(Ime::On),
            "1" => Some(Ime::Off),
            _ => None,
        }
    }

    /// 开关输入法。**丢后台线程**——`fcitx5-remote` 起一次进程约 2ms，
    /// 压在 30ms 的渲染循环里会顿一下。这里不需要结果，发出去就行。
    pub fn set(&self, state: Ime) {
        if !self.available {
            return;
        }
        std::thread::spawn(move || {
            let _ = Command::new("fcitx5-remote")
                .arg(if state == Ime::On { "-o" } else { "-c" })
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_runs_every_step_once() {
        let mut loader = Loader::new("真彩");
        let mut steps = 0;
        while loader.advance() {
            steps += 1;
        }
        assert_eq!(steps, STEPS.len());
        assert_eq!(loader.rows.len(), STEPS.len());
        assert!(!loader.advance());
    }

    #[test]
    fn kv_strips_quotes() {
        assert_eq!(
            kv("NAME=\"Arch Linux\"\nID=arch\n", "NAME").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(kv("NAME=x\n", "ID"), None);
    }
}
