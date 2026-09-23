//! 「夜阑」色板与终端色深降级。
//!
//! 引导与空会话 banner 是新用户见到的第一屏，不能赌终端支持什么：设计稿里
//! 一个颜色只写一次 RGB，落地时按能力分四档——真彩 / 256 / 16 / 不上色。
//! 降级不是「颜色变少」，是换一种手段表达同一件事（16 色以下选中态改反显）。
//!
//! 手动压：`MIYU_COLOR=truecolor|256|16|none`、`MIYU_ASCII=1`；`NO_COLOR`
//! （跨工具的无参数约定）优先级最高。

use ratatui::style::{Color, Modifier, Style};

/// 终端色深。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    /// 24 位真彩。渐变才有意义。
    True,
    /// xterm-256。渐变仍然能看，靠 6×6×6 色立方近似。
    X256,
    /// 只有 16 个基础色。渐变塌成单色，底色高亮换成反显。
    Ansi16,
    /// 完全不上色。只剩粗体 / 暗淡 / 反显。
    Mono,
}

impl Depth {
    pub fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return Depth::Mono;
        }
        if let Ok(value) = std::env::var("MIYU_COLOR") {
            return match value.trim().to_ascii_lowercase().as_str() {
                "truecolor" | "24bit" | "true" => Depth::True,
                "256" | "xterm256" => Depth::X256,
                "16" | "ansi" => Depth::Ansi16,
                "none" | "mono" | "0" => Depth::Mono,
                _ => Depth::X256,
            };
        }
        let colorterm = std::env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Depth::True;
        }
        let term = std::env::var("TERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if term.is_empty() || term == "dumb" {
            return Depth::Mono;
        }
        // kitty / wezterm / 新 alacritty 即便没设 COLORTERM 也是真彩。
        if term.contains("kitty") || term.contains("wezterm") || term.contains("alacritty") {
            return Depth::True;
        }
        if term.contains("256color") || term.contains("direct") {
            return Depth::X256;
        }
        Depth::Ansi16
    }

    /// 渐变值不值得画。16 色以下画出来是一串跳变的色块，不如单色干净。
    pub fn gradient_ok(self) -> bool {
        matches!(self, Depth::True | Depth::X256)
    }
}

/// 设计稿里的颜色一律写成 RGB，落地时才降级。
pub type Rgb = (u8, u8, u8);

/// 种子色取自 web/styles.css（miyu-logo 抽的）。
pub const BLUE: Rgb = (0xae, 0xbd, 0xe8); // primary  瞳色雾蓝
pub const CORAL: Rgb = (0xe3, 0x8c, 0x9a); // tertiary 丝带酒红
pub const GOLD: Rgb = (0xe4, 0xbf, 0x79); // secondary 发色暖金
pub const GREEN: Rgb = (0x9c, 0xcf, 0xa0);
pub const DIM: Rgb = (0x99, 0x93, 0xa5);
pub const FAINT: Rgb = (0x6b, 0x66, 0x77);
pub const SEL_BG: Rgb = (0x24, 0x27, 0x33);
/// 淡入的起点色：贴近底色，等于「从无到有」。
pub const INK: Rgb = (0x14, 0x15, 0x1c);

/// xterm-256 的 16 个基础色近似值，用来找最近色。
const ANSI16: [Rgb; 16] = [
    (0, 0, 0),
    (170, 0, 0),
    (0, 170, 0),
    (170, 85, 0),
    (0, 0, 170),
    (170, 0, 170),
    (0, 170, 170),
    (170, 170, 170),
    (85, 85, 85),
    (255, 85, 85),
    (85, 255, 85),
    (255, 255, 85),
    (85, 85, 255),
    (255, 85, 255),
    (85, 255, 255),
    (255, 255, 255),
];

/// RGB → xterm-256 索引。6×6×6 色立方 + 24 级灰阶，标准近似法。
fn to_256(color: Rgb) -> u8 {
    let (r, g, b) = color;
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((u16::from(r) - 8) * 24 / 247) as u8;
    }
    let quantize = |value: u8| -> u16 {
        if value < 48 {
            0
        } else if value < 115 {
            1
        } else {
            (u16::from(value).saturating_sub(35)) / 40
        }
    };
    (16 + 36 * quantize(r) + 6 * quantize(g) + quantize(b)).min(255) as u8
}

/// RGB → 16 色里最近的一个。欧氏距离足够，不必上 CIELAB。
fn to_16(color: Rgb) -> u8 {
    let (r, g, b) = (i32::from(color.0), i32::from(color.1), i32::from(color.2));
    let mut best = 7u8;
    let mut best_distance = i32::MAX;
    for (index, candidate) in ANSI16.iter().enumerate() {
        let distance = (r - i32::from(candidate.0)).pow(2)
            + (g - i32::from(candidate.1)).pow(2)
            + (b - i32::from(candidate.2)).pow(2);
        if distance < best_distance {
            best_distance = distance;
            best = index as u8;
        }
    }
    best
}

/// 色深 + 字符集，两根正交的轴：有的终端色彩很好但字体缺 Unicode，反过来也有。
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub depth: Depth,
    pub ascii: bool,
}

impl Theme {
    pub fn detect() -> Self {
        let ascii = std::env::var("MIYU_ASCII")
            .map(|value| value != "0" && !value.is_empty())
            .unwrap_or(false);
        Self {
            depth: Depth::detect(),
            ascii,
        }
    }

    /// 前景色。`Mono` 档一律不上色，交给 modifier 拉开层次。
    pub fn fg(self, color: Rgb) -> Style {
        match self.depth {
            Depth::True => Style::new().fg(Color::Rgb(color.0, color.1, color.2)),
            Depth::X256 => Style::new().fg(Color::Indexed(to_256(color))),
            Depth::Ansi16 => Style::new().fg(Color::Indexed(to_16(color))),
            Depth::Mono => Style::new(),
        }
    }

    /// 同 [`Theme::fg`]，给直接写 ANSI 的地方用（REPL 状态行这类不走 ratatui 的
    /// 输出）：两边按同一个色深降级，同一个颜色才画得一样（09-23 只读的金色）。
    pub fn fg_ansi(self, color: Rgb) -> String {
        match self.depth {
            Depth::True => format!("\x1b[38;2;{};{};{}m", color.0, color.1, color.2),
            Depth::X256 => format!("\x1b[38;5;{}m", to_256(color)),
            Depth::Ansi16 => format!("\x1b[38;5;{}m", to_16(color)),
            Depth::Mono => String::new(),
        }
    }

    /// 暗淡。低色深下没有「更暗的灰」可用，退回 DIM modifier。
    pub fn dim(self, color: Rgb) -> Style {
        match self.depth {
            Depth::True | Depth::X256 => self.fg(color),
            _ => Style::new().add_modifier(Modifier::DIM),
        }
    }

    /// 选中行的底色。16 色以下没有像样的深底可用，**换成反显**——
    /// 这是终端里最稳的「选中」表达，一路退到 vt100 都有。
    pub fn select(self, style: Style) -> Style {
        match self.depth {
            Depth::True => style.bg(Color::Rgb(SEL_BG.0, SEL_BG.1, SEL_BG.2)),
            Depth::X256 => style.bg(Color::Indexed(to_256(SEL_BG))),
            _ => style.add_modifier(Modifier::REVERSED),
        }
    }

    /// 两色之间取插值；不支持渐变的档位直接返回起点色。
    pub fn lerp(self, from: Rgb, to: Rgb, t: f32) -> Style {
        if !self.depth.gradient_ok() {
            return self.fg(from);
        }
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        self.fg((mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2)))
    }

    /// 往白里提亮，用来做扫光。
    pub fn lift(self, color: Rgb, t: f32) -> Style {
        self.lerp(color, (255, 255, 255), t)
    }

    // ── 字符集 ──
    pub fn dot_done(self) -> &'static str {
        if self.ascii {
            "*"
        } else {
            "●"
        }
    }
    pub fn dot_here(self) -> &'static str {
        if self.ascii {
            "@"
        } else {
            "◉"
        }
    }
    pub fn dot_todo(self) -> &'static str {
        if self.ascii {
            "-"
        } else {
            "○"
        }
    }
    pub fn cursor(self) -> &'static str {
        if self.ascii {
            "> "
        } else {
            "▸ "
        }
    }
    pub fn radio_on(self) -> &'static str {
        if self.ascii {
            "(o)"
        } else {
            "●"
        }
    }
    pub fn radio_off(self) -> &'static str {
        if self.ascii {
            "( )"
        } else {
            "○"
        }
    }
    pub fn hline(self) -> &'static str {
        if self.ascii {
            "-"
        } else {
            "─"
        }
    }
    pub fn arrow(self) -> &'static str {
        if self.ascii {
            ">"
        } else {
            "›"
        }
    }
    pub fn check(self) -> &'static str {
        if self.ascii {
            "+"
        } else {
            "✓"
        }
    }
    pub fn spinner(self, frame: usize) -> &'static str {
        const UNICODE: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];
        const ASCII: [&str; 4] = ["|", "/", "-", "\\"];
        if self.ascii {
            ASCII[frame % 4]
        } else {
            UNICODE[frame % 4]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantization_hits_expected_cube_cells() {
        // 纯白落在色立方的顶角，纯黑落在 16。
        assert_eq!(to_256((255, 255, 255)), 231);
        assert_eq!(to_256((0, 0, 0)), 16);
        // 灰阶走 232..=255 那条梯子。
        assert!((232..=255).contains(&to_256((128, 128, 128))));
        // 雾蓝在 16 色里最近的是亮白/亮蓝一带，绝不会掉到黑。
        assert_ne!(to_16(BLUE), 0);
    }

    #[test]
    fn low_depth_never_emits_rgb() {
        let mono = Theme {
            depth: Depth::Mono,
            ascii: false,
        };
        assert_eq!(mono.fg(BLUE), Style::new());
        assert_eq!(mono.lerp(BLUE, CORAL, 0.5), Style::new());
        let sixteen = Theme {
            depth: Depth::Ansi16,
            ascii: false,
        };
        assert_eq!(
            sixteen.select(Style::new()),
            Style::new().add_modifier(Modifier::REVERSED)
        );
    }
}
