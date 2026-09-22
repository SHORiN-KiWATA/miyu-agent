//! mermaid 围栏的渲染:终端出图,WebUI 出 SVG。
//!
//! 两端共用一个纯 Rust 渲染器(`mermaid-rs-renderer`,MIT)出 SVG。选它而不是在
//! WebUI 里 vendor 一份 mermaid.js,理由有三:
//!
//! 1. 终端本来就没有浏览器,mermaid.js 在那边用不了——总得有个 Rust 渲染器;
//! 2. 既然有了,WebUI 再 vendor 800KB 的 JS 就是同一件事做两遍,两边出图还会漂;
//! 3. 图在服务端渲染完再给前端,前端零 CPU、零第三方脚本。
//!
//! 参考实现:`jswysnemc/sai`(Miyu 的 fork)的 `render/asset_block/mermaid.rs`
//! 走的就是「SVG → 位图 → 终端图片协议」这条路,用户 09-20 点名让照着看。
//!
//! ## 终端这一侧的三条要紧规矩(都是被真机打回之后定的)
//!
//! **一、按终端要显示的像素数直接光栅化。** WebUI 拿到的是矢量,怎么缩都锐利;
//! 终端只能收位图。第一版走 `mermaid_rs_renderer::write_output_png` 出**自然
//! 尺寸**的 PNG,再让 kitty 那层重采样到格子里——于是长图先被缩小、再被重采样,
//! 糊成一团(用户 09-20:「终端打印出来的图片不够清晰,而 webui 的超级锐利」)。
//! 现在先算出占几格、乘出目标像素,拿 resvg 一次渲到那个尺寸:零重采样,文字由
//! 光栅化器在最终尺寸上抗锯齿,能有多清楚就有多清楚。
//!
//! **二、不为了塞进一屏而缩。** 高度上限原来是「视口的三分之二」,一张 60 行的
//! 竖流程图被压到 23 行,宽度跟着等比缩到 9 格——字全糊了(用户:「长图变得超级
//! 小」)。实测缩放比:12 节点 0.41×、24 节点 0.23×。一压就废,压了也还是看不清,
//! 所以只留宽度闸(正文区宽)和一道防病态的行数天花板,高的图就让它占几屏,图在
//! 缓冲里,往回翻得到。
//!
//! **三、缩进归 `indent_body` 管,这儿一格都不加。** 正文出了 markdown 渲染器
//! 之后会过一道 `timeline::indent_body`(`stream/mod.rs`),装订边它加。这儿再自己
//! 加两格就是双重缩进——图比同一屏的表情包多缩一层(用户 09-20 对比截图)。
//!
//! ## 缓存
//!
//! 位图按「源码 + 格子像素 + 宽度上限」哈希落盘到 `<家>/cache/diagrams/`。重开
//! 会话会把最近几轮正文整个重放一遍(`repl_replay_turns` 默认 3),没有缓存的话
//! 每张图都要重新解析、布局、光栅化一次——实测一张 24 节点的图单次 139ms,三轮
//! 就是近半秒的白等(用户:「重开这个会话又要重新加载一次,也很慢」)。
//!
//! **认不出来就退回代码块**:终端不认图片协议、语法有硬错误、图型不支持,统统
//! 返回 `None`,调用方照常打那个带语法高亮的围栏。永不阻断输出。

use crate::render::t;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// 源码上限。渲染是同步的,挡住"把整本书塞进围栏"只能挡在入口:一张正常的图
/// 几 KB,这里留两个数量级。(测过一次"渲染完再看花了多久"的写法——活儿已经干完
/// 了才报超时,什么也没挡住,遂改成卡输入。)
pub const MAX_SOURCE: usize = 64 * 1024;

/// 宽度上限(格)。横向图只能受制于终端宽度,这是没办法的事。
const MAX_COLS: usize = 120;

/// 行数天花板。**不是**为了让图塞进一屏——那样只会把字压糊(见模块头规矩二);
/// 纯粹防病态输入把一张几千行的图铺出来。
const MAX_ROWS: usize = 200;

/// 磁盘缓存留多少张。一张几十到几百 KB,128 张是几十 MB 的量级。
const CACHE_ENTRIES: usize = 128;

/// 缓存键的版本。渲染口径一变(尺寸算法、底色、光栅化参数)就得换,否则读到的是
/// 按老口径画的图。
const CACHE_VERSION: u32 = 1;

/// 这个终端能怎么出图。**在渲染之前就得问清楚**:两条路都走不通时直接退回代码
/// 块,别白渲染一张再扔掉(管道、日志、非 kitty 又没装 chafa 的场合全是这样)。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Graphics {
    /// kitty 家族的图形协议:纯编码,同步算得完。
    Kitty,
    /// 其余终端交给 chafa,它认得 Konsole / WezTerm / foot / iTerm2 之流。
    Chafa,
}

/// 这段围栏是不是 mermaid。语言标记大小写不敏感。
pub(crate) fn is_mermaid_lang(lang: &str) -> bool {
    lang.trim().eq_ignore_ascii_case("mermaid")
}

/// 源码 → SVG。WebUI 走这条:图在服务端渲染好,前端只管把 SVG 塞进卡片。
///
/// **带进程内缓存。** 同一张图会被反复要:WebUI 那个 `mermaidCache` 是个 JS
/// `Map`,一刷新页面就没了,于是每次刷新每张卡片都重新 POST 一遍。没有缓存时实测
/// (debug 档)一张 6 节点的图每次 ~90ms、14 节点的 ~580ms,**次次如此**——一页两
/// 三张图,刷新一下就是一秒多的「正在画图…」(用户 09-20)。
///
/// 只放内存不落盘:SVG 才几 KB、重算也就百毫秒量级,daemon 重启后头一次重算是可
/// 以接受的;而终端那条路本来就有按「源码+终端几何」落盘的位图缓存,轮不到这儿
/// 操心。
pub fn render_svg(source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("empty mermaid source".to_string());
    }
    if source.len() > MAX_SOURCE {
        return Err(format!("mermaid source exceeds {MAX_SOURCE} bytes"));
    }
    let key = blake3::hash(source.as_bytes());
    if let Some(svg) = svg_cache_get(key.as_bytes()) {
        return Ok(svg);
    }
    let svg = mermaid_rs_renderer::render(source).map_err(|error| error.to_string())?;
    svg_cache_put(*key.as_bytes(), &svg);
    Ok(svg)
}

/// 缓存住多少张 SVG。一张几 KB,64 张不到 1MB。
const SVG_CACHE_ENTRIES: usize = 64;

type SvgCache = (
    std::collections::HashMap<[u8; 32], String>,
    std::collections::VecDeque<[u8; 32]>,
);

fn svg_cache() -> &'static std::sync::Mutex<SvgCache> {
    static CACHE: OnceLock<std::sync::Mutex<SvgCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new((
            std::collections::HashMap::new(),
            std::collections::VecDeque::new(),
        ))
    })
}

fn svg_cache_get(key: &[u8; 32]) -> Option<String> {
    // 锁中毒不该让出图整个失败:当作没缓存,重算一遍就是了。
    let guard = svg_cache().lock().ok()?;
    guard.0.get(key).cloned()
}

/// 满了就丢最早进来的那张(FIFO)。真 LRU 要在读的时候也改动结构、得拿写锁,
/// 对这个量级不值当——同一页上的图反正都在窗口里。
fn svg_cache_put(key: [u8; 32], svg: &str) {
    let Ok(mut guard) = svg_cache().lock() else {
        return;
    };
    let (map, order) = &mut *guard;
    if map.insert(key, svg.to_string()).is_none() {
        order.push_back(key);
        while order.len() > SVG_CACHE_ENTRIES {
            if let Some(oldest) = order.pop_front() {
                map.remove(&oldest);
            }
        }
    }
}

/// 源码 → 终端里那一段(上下各留一个空行,可直接进正文;**不带缩进**)。
///
/// 返回 `None` = 这次不出图,调用方退回代码块。
pub(crate) fn render_terminal(source: &str) -> Option<String> {
    let graphics = graphics_kind()?;
    let source = source.trim();
    if source.is_empty() || source.len() > MAX_SOURCE {
        return None;
    }
    let (cell_w, cell_h) = cell_pixels();
    let png = diagram_png(source, cell_w, cell_h, max_cols())?;
    // 格数从位图尺寸倒推:它就是按 `格数 × 格子像素` 渲出来的,不必另存一份。
    // 尺寸只读 PNG 头,不解整张图——kitty 那条路要的就是 PNG 字节本身。
    let (width, height) = png_size(&png)?;
    let cols = (width as usize / cell_w).max(1);
    let rows = (height as usize / cell_h).max(1);
    let body = match graphics {
        Graphics::Kitty => miyu_base::terminal::kitty::kitty_png_sequence_with_grid(
            &png,
            u16::try_from(cols).ok()?,
            u16::try_from(rows).ok()?,
        )
        .ok()?,
        Graphics::Chafa => chafa_lines(&png, cols, rows)?.join("\n"),
    };
    let mut block = body;
    if !block.ends_with('\n') {
        block.push('\n');
    }
    if let Some(link) = zoom_link(source, cell_w, cell_h, max_cols()) {
        block.push_str(&link);
    }
    Some(surround(&block))
}

/// 上下各留一个空行,和正文拉开呼吸感。缩进不在这儿加(见模块头规矩三)。
fn surround(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 4);
    out.push('\n');
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out
}

/// 这个终端能不能出图、走哪条路。
///
/// 单元测试里一律说「不能」:这里读的是**开发者终端**的 `TERM`,在 kitty 里跑
/// `cargo test` 会让这条分支跟着环境变脸。只挡 `cfg(test)` 不挡 `testkit` 特性
/// ——走查用的二进制常带 testkit,跟着关掉的话真机走查就永远看不到图了。
fn graphics_kind() -> Option<Graphics> {
    if cfg!(test) {
        return None;
    }
    if miyu_base::terminal::kitty::is_native_kitty_terminal()
        || std::env::var("TERM").is_ok_and(|term| term.contains("ghostty"))
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
    {
        return Some(Graphics::Kitty);
    }
    // chafa 那条要起子进程,所以先确认输出真是终端:管道、重定向里起它毫无意义。
    // `capabilities()` 自己带 OnceLock,一个进程只探一次。
    if std::io::IsTerminal::is_terminal(&std::io::stdout())
        && miyu_base::terminal::chafa::capabilities().present()
    {
        return Some(Graphics::Chafa);
    }
    None
}

/// 只读 PNG 头拿尺寸,不解整张图。一张 198×2400 的图解开要 5~9ms,而我们只想
/// 知道它占几格——传给 kitty 的是 PNG 字节本身,像素一个都用不上。
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(png))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn cell_pixels() -> (usize, usize) {
    let (width, height) = miyu_base::terminal::kitty::cell_pixel_size();
    (usize::from(width.max(1)), usize::from(height.max(1)))
}

/// 宽度上限:按正文区收口,再留出装订边。
fn max_cols() -> usize {
    crate::render::content_cols(100)
        .saturating_sub(4)
        .clamp(20, MAX_COLS)
}

/// 拿这张图的位图:先查缓存,没有再渲一张存下来。
///
/// 同一个键还会落一份 **SVG**,给「点开看大图」那行链接用。盘上这张 PNG 是按
/// 终端格子渲的(比如 207×460),在看图器里放到 100% 以上就开始糊;SVG 是矢量,
/// 放多大都锐利,而且才几 KB。渲染时 SVG 本来就在手里,白落的。
///
/// 缓存不可用(拿不到家目录、写不进去)不影响出图,只是每次都得重渲、且没有那行
/// 链接。
fn diagram_png(source: &str, cell_w: usize, cell_h: usize, max_cols: usize) -> Option<Vec<u8>> {
    let cached = cache_path(source, cell_w, cell_h, max_cols);
    if let Some(path) = cached.as_deref() {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    let svg = render_svg(source).ok()?;
    let png = rasterize(&svg, cell_w, cell_h, max_cols)?;
    if let Some(path) = cached.as_deref() {
        if let Some(dir) = path.parent() {
            if std::fs::create_dir_all(dir).is_ok() && std::fs::write(path, &png).is_ok() {
                // SVG 写失败不算失败:图照出,只是少一行链接。
                let _ = std::fs::write(path.with_extension("svg"), &svg);
                prune(dir);
            }
        }
    }
    Some(png)
}

/// 图下面那行「点开看大图」,OSC 8 指向盘上那张 PNG。
///
/// **inline 下点击归终端管**(kitty 在 Linux 上转给 `xdg-open`、macOS 上转给
/// `open`);**全屏下归 Miyu 自己管**——全屏把鼠标捕获走了,终端那套失效,见
/// `cli::repl::tail::screen::select::{url_at, open_url}`。
///
/// 指向 **SVG 而不是那张 PNG**:放大这件事只有矢量做得好——PNG 是按终端格子渲
/// 死的,看图器里超过 100% 就糊。
///
/// 代价是 `image/svg+xml` 的默认程序不像 PNG 那么靠谱:很多机器压根没给它指定
/// 过,于是落到系统级默认(用户 09-20 实测点开来的是 Curtail,一个图片**压缩**
/// 工具)。一条 `xdg-mime default <看图器>.desktop image/svg+xml` 就能定好,只
/// 需一次。
///
/// 文件不在(缓存拿不到、或者被 `prune` 清掉了)就不给链接——给一条点开是空的
/// 链接比没有更糟。
fn zoom_link(source: &str, cell_w: usize, cell_h: usize, max_cols: usize) -> Option<String> {
    let svg = cache_path(source, cell_w, cell_h, max_cols)?.with_extension("svg");
    if !svg.is_file() {
        return None;
    }
    let label = format!("\x1b[2m{}\x1b[0m", t("open full size", "点开看大图"));
    Some(format!(
        "{}\n",
        crate::render::link::hyperlink(&file_url(&svg), &label)
    ))
}

/// 本地路径 → `file://` URL。逐段百分号编码:家目录里有空格或中文时,不编码的
/// URL 终端会截断。
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy();
    let encoded: Vec<String> = text
        .split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect();
    format!("file://{}", encoded.join("/"))
}

fn cache_path(source: &str, cell_w: usize, cell_h: usize, max_cols: usize) -> Option<PathBuf> {
    let dir = cache_dir()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&CACHE_VERSION.to_le_bytes());
    hasher.update(format!("{cell_w}x{cell_h}|{max_cols}|").as_bytes());
    hasher.update(source.as_bytes());
    Some(dir.join(format!("{}.png", hasher.finalize().to_hex())))
}

fn cache_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        miyu_base::paths::MiyuPaths::new()
            .ok()
            .map(|paths| paths.cache_dir.join("diagrams"))
    })
    .clone()
}

/// 超出上限就按写入时间删最老的。只在**刚写进一张**之后跑,平时不扫目录。
///
/// 按 **PNG 的张数**算(一张图落两个文件:`.png` 给终端画、`.svg` 给链接点开),
/// 清的时候两个一起清——只清 PNG 会留下一地永远点不开的 SVG。
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut diagrams: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    if diagrams.len() <= CACHE_ENTRIES {
        return;
    }
    diagrams.sort_by_key(|(modified, _)| *modified);
    for (_, png) in diagrams.iter().take(diagrams.len() - CACHE_ENTRIES) {
        let _ = std::fs::remove_file(png);
        let _ = std::fs::remove_file(png.with_extension("svg"));
    }
}

/// SVG → 正好铺满 `格数 × 格子像素` 的 PNG。
///
/// 自己调 resvg 而不用 `mermaid_rs_renderer::write_output_png`,为的是两件事:
/// 一是那个函数只会按自然尺寸出图(于是必然要再重采样一次,字就糊了);二是它
/// **每次调用都 `load_system_fonts()`**,而字体库在这儿只加载一次。
/// 把 mermaid 源码渲成 PNG，等比缩放塞进给定的框里。
///
/// 终端那条(`rasterize`)按格子定尺；成图渲染器按像素，而且**高度也要有上限**：
/// 一张不能分页的图比整页还高的话，分页器只能把它整块丢到下一页，无限循环。
/// 等比只缩不放：放大不会凭空长出细节。
///
/// 把 SVG 那块**整幅底色矩形**改成给定的颜色。
///
/// mermaid 渲染器输出的第一个元素永远是
/// `<rect x="0" y="0" width=… height=… fill="#FFFFFF"/>`。光靠 `pixmap.fill`
/// 盖不住它——实测一张三节点的小图渲完仍有 94 万个纯白像素,全是这块。
///
/// **只动这一块**:节点自己的浅色填充(`#F8FAFC` 一类)是图的一部分,改了图就不是
/// 原来那张了。判据是「紧跟在 `<svg …>` 后面、且 x/y 都是 0」——不满足就原样返回,
/// 宁可保持白底也不要乱改别人的图元。
pub(crate) fn recolour_backdrop(svg: &str, replacement: &str) -> String {
    let Some(start) = svg.find("<rect") else {
        return svg.to_string();
    };
    // `<svg …>` 与它之间只允许有空白:再往后的 rect 就是图元了。
    let Some(head_end) = svg.find('>') else {
        return svg.to_string();
    };
    if !svg[head_end + 1..start].trim().is_empty() {
        return svg.to_string();
    }
    let Some(len) = svg[start..].find("/>") else {
        return svg.to_string();
    };
    let end = start + len + 2;
    let rect = &svg[start..end];
    if !rect.contains("x=\"0\"") || !rect.contains("y=\"0\"") {
        return svg.to_string();
    }
    let Some(fill_at) = rect.find("fill=\"") else {
        return svg.to_string();
    };
    let value_at = fill_at + "fill=\"".len();
    let Some(value_len) = rect[value_at..].find('"') else {
        return svg.to_string();
    };
    let mut out = String::with_capacity(svg.len() + replacement.len());
    out.push_str(&svg[..start + value_at]);
    out.push_str(replacement);
    out.push_str(&svg[start + value_at + value_len..]);
    out
}

/// 渲染器用的那几个色，按用途列出来。
///
/// 实测整张图只有 5 个色（一张五节点的流程图数出来的）：底 `#FFFFFF`、节点填充
/// `#F8FAFC`、节点描边 `#94A3B8`、连线与标签 `#64748B`、节点内文字 `#0F172A`。
/// 底那一个由 [`recolour_backdrop`] 单独处理（只动整幅那块），这里是其余四个。
pub(crate) const NODE_FILL: &str = "#F8FAFC";
pub(crate) const NODE_STROKE: &str = "#94A3B8";
pub(crate) const CONNECTOR: &str = "#64748B";
pub(crate) const NODE_TEXT: &str = "#0F172A";

/// 按一张「源色 → 目标」的表换色。
///
/// 只换列在表里的那几个：渲染器以后加了新色，不会被我们改花。目标值可以是任何
/// SVG 认的颜色写法——WebUI 那边填的是 `var(--md-sys-color-…)`，靠 CSS 变量
/// 穿透进内联 SVG，这样切主题零成本、也不用把主题塞进前端那份缓存的键。
pub(crate) fn repaint(svg: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = svg.to_string();
    for (from, to) in pairs {
        out = out.replace(&format!("\"{from}\""), &format!("\"{to}\""));
    }
    out
}

/// 底色由调用方给：成图渲染器要跟页面主题一致(用户 09-22:正文是米色纸面,
/// 图却是纯白方块,一眼看出是贴上去的)。透明底不行——渲染器给的是浅色主题,
/// 线条落在深色纸面上会被吃掉。
pub(crate) fn render_png_in_box(
    source: &str,
    max_width: u32,
    max_height: u32,
    background: [u8; 4],
) -> Option<Vec<u8>> {
    use resvg::tiny_skia;
    use resvg::usvg;

    if max_width == 0 || max_height == 0 {
        return None;
    }
    let backdrop = format!(
        "#{:02X}{:02X}{:02X}",
        background[0], background[1], background[2]
    );
    let svg = recolour_backdrop(&render_svg(source).ok()?, &backdrop);
    let options = usvg::Options {
        font_family: default_font_family(),
        fontdb: fonts(),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(&svg, &options).ok()?;
    let natural = tree.size();
    if natural.width() <= 0.0 || natural.height() <= 0.0 {
        return None;
    }
    let scale = (max_width as f32 / natural.width()).min(max_height as f32 / natural.height());
    let width = (natural.width() * scale).round().max(1.0) as u32;
    let height = (natural.height() * scale).round().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    pixmap.fill(tiny_skia::Color::from_rgba8(
        background[0],
        background[1],
        background[2],
        background[3],
    ));
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

/// 终端里图的底色。
///
/// 不是纯白:深色终端里一块 `#FFFFFF` 太刺眼（用户 09-22）。但也别太灰——第一版
/// 取 `#E4E4E7`，用户实测「有点太灰了」。现在贴着白往下挪一档：眼睛不扎，又比
/// 节点填充 `#F8FAFC` 低 6 阶，节点仍浮得出来。
const TERMINAL_BACKDROP: [u8; 3] = [0xF0, 0xF0, 0xF2];

fn rasterize(svg: &str, cell_w: usize, cell_h: usize, max_cols: usize) -> Option<Vec<u8>> {
    use resvg::tiny_skia;
    use resvg::usvg;

    let options = usvg::Options {
        font_family: default_font_family(),
        fontdb: fonts(),
        ..usvg::Options::default()
    };
    let backdrop = format!(
        "#{:02X}{:02X}{:02X}",
        TERMINAL_BACKDROP[0], TERMINAL_BACKDROP[1], TERMINAL_BACKDROP[2]
    );
    let svg = recolour_backdrop(svg, &backdrop);
    let tree = usvg::Tree::from_str(&svg, &options).ok()?;
    let natural = tree.size();
    let (cols, rows) = fit_cells(natural.width(), natural.height(), cell_w, cell_h, max_cols);
    let width = u32::try_from(cols * cell_w).ok()?;
    let height = u32::try_from(rows * cell_h).ok()?;
    let scale = (width as f32 / natural.width()).min(height as f32 / natural.height());
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    // 底色不能透:渲染器给的是浅色主题,深色终端会把浅色线条吃掉。但纯白在深色
    // 终端里是一块刺眼的板子(用户 09-22),换成中性灰——比节点填充(`#F8FAFC`)
    // 深一点点,节点因此还能浮出来。等比缩放留下的边也得有底,不然那几列是透明的。
    pixmap.fill(tiny_skia::Color::from_rgba8(
        TERMINAL_BACKDROP[0],
        TERMINAL_BACKDROP[1],
        TERMINAL_BACKDROP[2],
        255,
    ));
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

/// 系统字体库只扫一次。`mermaid_rs_renderer` 那边是每次调用重扫一遍——实测一张
/// 三节点的小图光 PNG 那一段就要 25ms,基本都花在这儿。
fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

/// SVG 里没写 `font-family` 时用哪个。取渲染器主题里的第一个族,和它自己出图时
/// 的口径一致。
fn default_font_family() -> String {
    let theme = mermaid_rs_renderer::Theme::mermaid_default();
    theme
        .font_family
        .split(',')
        .next()
        .map(|family| family.trim().trim_matches(['"', '\'']).to_string())
        .filter(|family| !family.is_empty())
        .unwrap_or_else(|| "sans-serif".to_string())
}

/// 图占多少格:按自然像素尺寸换算,超宽等比缩,再压一道防病态的行数天花板。
///
/// 不放大:mermaid 出的图尺寸跟着内容走,小图就让它小,硬撑到满屏只会糊。
fn fit_cells(
    width: f32,
    height: f32,
    cell_w: usize,
    cell_h: usize,
    max_cols: usize,
) -> (usize, usize) {
    let natural_w = (width.ceil().max(1.0) as usize).max(1);
    let natural_h = (height.ceil().max(1.0) as usize).max(1);
    let mut cols = natural_w.div_ceil(cell_w).max(1);
    let mut rows = natural_h.div_ceil(cell_h).max(1);
    if cols > max_cols {
        rows = (rows * max_cols).div_ceil(cols).max(1);
        cols = max_cols;
    }
    if rows > MAX_ROWS {
        cols = (cols * MAX_ROWS).div_ceil(rows).max(1);
        rows = MAX_ROWS;
    }
    (cols, rows)
}

/// 非 kitty 终端交给 chafa。
///
/// 参数走 `terminal::chafa::captured_args()`——它按 chafa 版本组装探测开关,
/// 公式那条路踩过「写死 `--probe-mode ctty` 在 1.18.1 以下直接退出码 2」的坑。
fn chafa_lines(png: &[u8], cols: usize, rows: usize) -> Option<Vec<String>> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut args: Vec<String> = miyu_base::terminal::chafa::captured_args()
        .into_iter()
        .map(str::to_string)
        .collect();
    args.extend([
        "--size".to_string(),
        format!("{cols}x{rows}"),
        "-".to_string(),
    ]);
    let mut child = Command::new("chafa")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(png).ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    // 图形格式下 chafa 用 IND(`ESC D`)推行而不是换行,渲染层按行记账会少算
    // N-1 行,所以换回换行——两者都是「下移一行」。
    let text = String::from_utf8_lossy(&output.stdout).replace("\u{1b}D", "\n");
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return None;
    }
    // sixel/iterm 这些格式一行都不推或只推一行,补齐到实际占用的行数,否则渲染层
    // 以为图只占一行,后面的正文会写到图上。
    while lines.len() < rows {
        lines.push(String::new());
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOW: &str = "flowchart TD\n    A[开始] --> B{判断}\n    B -->|是| C[做事]\n    B -->|否| D[结束]\n    C --> D";
    const CELL: (usize, usize) = (9, 20);

    /// 常见图型渲染得出 SVG,而且尺寸不是 0。
    #[test]
    fn a_flowchart_renders_to_svg() {
        let svg = render_svg(FLOW).expect("流程图该渲染得出来");
        assert!(svg.starts_with("<svg"), "{}", &svg[..svg.len().min(80)]);
        assert!(
            svg.contains("width=") && svg.contains("height="),
            "要有尺寸"
        );
        // 中文标签要进得去(字体找不到时会整段丢字)。
        assert!(svg.contains("开始"), "中文节点名没进 SVG");
    }

    /// 时序图也认——它和流程图占了实际用量的绝大多数。
    #[test]
    fn a_sequence_diagram_renders_to_svg() {
        let svg = render_svg("sequenceDiagram\n    用户->>Miyu: 在吗\n    Miyu-->>用户: 在")
            .expect("时序图该渲染得出来");
        assert!(svg.starts_with("<svg"), "{}", &svg[..svg.len().min(80)]);
    }

    /// 语法不对给 Err,由调用方退回代码块;绝不 panic。
    #[test]
    fn broken_source_is_an_error_not_a_panic() {
        assert!(render_svg("").is_err(), "空源码该报错");
        // 认不出的图型:要么报错、要么给一张空图,两种都不能炸。
        let _ = render_svg("这不是 mermaid，只是一句话");
    }

    /// 同一张图要第二遍时不再重算——WebUI 每刷新一次页面就会把每张卡片重新
    /// POST 一遍,没缓存的话次次几十上百毫秒(用户 09-20:「每次刷新都在画图」)。
    #[test]
    fn the_same_source_is_only_rendered_once() {
        // 用一段独一无二的源码,免得和别的测试共用进程时互相命中。
        let source = "flowchart TD\n    唯一缓存测试[只渲一次] --> 好[好]";
        let first = std::time::Instant::now();
        let a = render_svg(source).expect("该渲染得出来");
        let cold = first.elapsed();
        let second = std::time::Instant::now();
        let b = render_svg(source).expect("第二遍该命中缓存");
        let warm = second.elapsed();
        assert_eq!(a, b, "两遍出的 SVG 不一样");
        // 命中缓存是一次哈希 + 一次 clone,和解析布局差着数量级;放宽到十分之一,
        // 机器再慢也不会误判。
        assert!(
            warm * 10 < cold.max(std::time::Duration::from_micros(200)),
            "第二遍没走缓存:冷 {cold:?} / 热 {warm:?}"
        );
    }

    /// 缓存满了按先进先出丢,不会无限长。
    #[test]
    fn the_svg_cache_stays_bounded() {
        for index in 0..(SVG_CACHE_ENTRIES + 20) {
            svg_cache_put(
                blake3::hash(format!("满不满 {index}").as_bytes()).into(),
                "<svg/>",
            );
        }
        let guard = svg_cache().lock().unwrap();
        assert!(
            guard.0.len() <= SVG_CACHE_ENTRIES,
            "缓存涨过头:{}",
            guard.0.len()
        );
        assert_eq!(guard.0.len(), guard.1.len(), "表和次序队列对不上");
    }

    /// 源码太大直接拒,不进渲染器。渲染是同步的,这是唯一挡得住的地方。
    #[test]
    fn an_oversized_source_is_refused_before_rendering() {
        let huge = format!("flowchart TD\n{}", "    A --> B\n".repeat(20_000));
        assert!(huge.len() > MAX_SOURCE);
        let error = render_svg(&huge).expect_err("超长源码该被拒");
        assert!(error.contains("exceeds"), "{error}");
    }

    /// 语言标记判定:大小写不敏感,别的语言不认。
    #[test]
    fn only_the_mermaid_fence_is_claimed() {
        assert!(is_mermaid_lang("mermaid"));
        assert!(is_mermaid_lang("Mermaid"));
        assert!(is_mermaid_lang("  MERMAID "));
        assert!(!is_mermaid_lang("rust"));
        assert!(!is_mermaid_lang(""));
    }

    /// 位图**正好**是格数乘格子像素:这就是「不再重采样」的定义,也是靠它把
    /// 格数倒推回来的前提。
    #[test]
    fn the_bitmap_lands_exactly_on_the_cell_grid() {
        let svg = render_svg(FLOW).unwrap();
        let png = rasterize(&svg, CELL.0, CELL.1, 100).expect("该光栅化得出来");
        assert_eq!(&png[1..4], b"PNG", "不是 PNG 头");
        let image = image::load_from_memory(&png).expect("PNG 该解得开");
        assert_eq!(image.width() as usize % CELL.0, 0, "宽度没落在格子上");
        assert_eq!(image.height() as usize % CELL.1, 0, "高度没落在格子上");
        assert!(image.width() > 50 && image.height() > 50, "图太小");
    }

    /// 长图不再被压扁。竖着长的流程图按自然尺寸走,只受行数天花板约束——
    /// 「视口的三分之二」那版把 24 节点的图缩到 0.23×,字全糊了(用户 09-20)。
    #[test]
    fn a_tall_diagram_keeps_its_natural_scale() {
        let mut lines = vec!["flowchart TD".to_string()];
        for index in 0..23 {
            lines.push(format!(
                "    N{index}[步骤{index}] --> N{}[步骤{}]",
                index + 1,
                index + 1
            ));
        }
        let svg = render_svg(&lines.join("\n")).unwrap();
        let png = rasterize(&svg, CELL.0, CELL.1, 100).unwrap();
        let image = image::load_from_memory(&png).unwrap();
        let rows = image.height() as usize / CELL.1;
        assert!(rows > 40, "长图被压扁了,只剩 {rows} 行");
        assert!(rows <= MAX_ROWS, "行数天花板没兜住:{rows}");
    }

    /// kitty 那条路:序列里既要有**传输段**(`ESC _G`)又要有占位格。
    ///
    /// 第一版只留了占位格,屏幕上一片空白——这条就是为了钉死那个回归。
    #[test]
    fn the_kitty_sequence_carries_both_halves() {
        let svg = render_svg(FLOW).unwrap();
        let png = rasterize(&svg, CELL.0, CELL.1, 100).unwrap();
        let (width, height) = png_size(&png).expect("PNG 头该读得出尺寸");
        let cols = width as usize / CELL.0;
        let rows = height as usize / CELL.1;
        let sequence = miyu_base::terminal::kitty::kitty_png_sequence_with_grid(
            &png,
            cols as u16,
            rows as u16,
        )
        .expect("kitty 序列该生成得出来");
        assert!(sequence.contains("\u{1b}_G"), "没有图形传输段");
        // 走 PNG 传输(`f=100`)而不是裸 RGBA(`f=32`):一张大片留白的图差着一个
        // 数量级,而 PNG 字节本来就在手里。
        assert!(sequence.contains("f=100"), "没走 PNG 传输");
        // 只量**传输段**。序列后半截是占位格,它的大小跟着显示网格走(每格一个
        // 四字节的 U+10EEEE 加两个变音符),跟图本身多大没关系。CI 上字体少,同一
        // 张图渲出来只有 4KB,占位格却照旧那么大,整条序列就顶到了 2.14 倍——量错
        // 了对象,不是传输真的胖了(09-23,`--workspace` 第一次把这条带上 CI 才露出来)。
        let transfer_end = sequence.rfind("\u{1b}\\").expect("传输段该有收尾") + 2;
        let transfer = &sequence[..transfer_end];
        assert!(
            transfer.len() < png.len() * 2,
            "传输量不该比 PNG 本身大出一倍以上:{} vs {}",
            transfer.len(),
            png.len()
        );
        // kitty 的占位格是 U+10EEEE（六位，别少写一位写成 U+10EEE）。
        assert!(
            sequence.contains('\u{10eeee}'),
            "没有 Unicode 占位格,图落不到正文里"
        );
    }

    /// 出来的那一段**自己不带缩进**:装订边归 `timeline::indent_body` 加,
    /// 这儿再加一次就是双重缩进,图会比同一屏的表情包多缩一层(用户 09-20)。
    #[test]
    fn the_block_carries_no_indent_of_its_own() {
        let block = surround("第一行\n第二行\n");
        assert_eq!(block, "\n第一行\n第二行\n\n");
        assert!(
            !block.lines().any(|line| line.starts_with(' ')),
            "块自己加了缩进:{block:?}"
        );
    }

    /// 格数换算:不放大、超宽等比缩、超高再收一道。
    #[test]
    fn cells_never_exceed_the_caps() {
        let (cols, rows) = fit_cells(40_000.0, 40_000.0, CELL.0, CELL.1, 100);
        assert!(cols <= 100, "列数没收住:{cols}");
        assert!(rows <= MAX_ROWS, "行数没收住:{rows}");
        let (small_cols, small_rows) = fit_cells(20.0, 20.0, CELL.0, CELL.1, 100);
        assert_eq!((small_cols, small_rows), (3, 1), "小图不该被放大");
    }

    /// 缓存键跟着格子尺寸与宽度上限走:换了终端、改了窗口宽度,不能拿旧图糊弄。
    #[test]
    fn the_cache_key_follows_the_terminal_geometry() {
        let Some(base) = cache_path(FLOW, 9, 20, 100) else {
            return; // 拿不到家目录的环境（CI 沙箱）就跳过
        };
        assert_ne!(base, cache_path(FLOW, 10, 20, 100).unwrap(), "格宽没进键");
        assert_ne!(base, cache_path(FLOW, 9, 21, 100).unwrap(), "格高没进键");
        assert_ne!(base, cache_path(FLOW, 9, 20, 80).unwrap(), "宽度上限没进键");
        assert_ne!(
            base,
            cache_path("flowchart TD\n A-->B", 9, 20, 100).unwrap(),
            "源码没进键"
        );
        assert_eq!(
            base,
            cache_path(FLOW, 9, 20, 100).unwrap(),
            "同样的输入该同一个键"
        );
    }

    /// 各段耗时与传输量。**不是断言,是量尺**——默认不跑,要数时:
    ///
    /// ```text
    /// cargo test --release -p miyu-hosts mermaid::tests::timings -- --ignored --nocapture
    /// ```
    ///
    /// 必须 `--release`:debug 下 resvg 光栅化慢一个数量级,量出来的数没有意义。
    ///
    /// **读数时当心页缓存。** 同进程里并排量过:`write_output_png` 第一次
    /// 1498ms、第二次 28.6ms,裸 `load_system_fonts` 23.2ms——那一秒半是冷页缓存
    /// 下扫 2467 个字体文件的代价,跟谁调的没关系。所以下面把字体库单独列一行:
    /// 它是**每进程一次**(渲染器自带那条是**每张图一次**),而缓存命中时一次都
    /// 不付——`rasterize` 根本不会被调到。
    #[test]
    #[ignore = "量尺,不是断言"]
    fn timings() {
        fn chain(n: usize) -> String {
            let mut lines = vec!["flowchart TD".to_string()];
            for i in 0..n {
                lines.push(format!(
                    "    N{i}[步骤{i} 做一件事] --> N{}[步骤{} 做另一件事]",
                    i + 1,
                    i + 1
                ));
            }
            lines.join("\n")
        }
        let cases: Vec<(String, String)> = vec![
            (
                "3 节点".into(),
                "flowchart TD\n    A[开始] --> B[中间]\n    B --> C[结束]".into(),
            ),
            ("6 节点".into(), FLOW.into()),
            ("12 节点".into(), chain(11)),
            ("24 节点".into(), chain(23)),
        ];

        let started = std::time::Instant::now();
        let db = fonts();
        println!(
            "字体库 {:.0}ms（{} 个 face，每进程一次；冷页缓存下会到一秒半）\n",
            started.elapsed().as_secs_f64() * 1000.0,
            db.len()
        );
        println!(
            "{:<10}{:>9}{:>10}{:>9}{:>10}{:>11}{:>12}",
            "图", "SVG", "光栅化", "编码", "首次", "缓存命中", "传输字节"
        );
        for (name, source) in &cases {
            let started = std::time::Instant::now();
            let svg = render_svg(source).unwrap();
            let svg_ms = started.elapsed().as_secs_f64() * 1000.0;

            let started = std::time::Instant::now();
            let png = rasterize(&svg, CELL.0, CELL.1, 100).unwrap();
            let raster_ms = started.elapsed().as_secs_f64() * 1000.0;

            let encode = |png: &[u8]| {
                let (width, height) = png_size(png).unwrap();
                let cols = width as usize / CELL.0;
                let rows = height as usize / CELL.1;
                miyu_base::terminal::kitty::kitty_png_sequence_with_grid(
                    png,
                    cols as u16,
                    rows as u16,
                )
                .unwrap()
            };
            let started = std::time::Instant::now();
            let sequence = encode(&png);
            let encode_ms = started.elapsed().as_secs_f64() * 1000.0;

            // 缓存命中那条:不解析、不布局、不光栅化,只把盘上那张 PNG 解开再编码。
            let started = std::time::Instant::now();
            let _ = encode(&png);
            let cached_ms = started.elapsed().as_secs_f64() * 1000.0;

            println!(
                "{name:<10}{svg_ms:>8.1}ms{raster_ms:>9.1}ms{encode_ms:>8.1}ms\
{:>9.1}ms{cached_ms:>10.1}ms{:>12}",
                svg_ms + raster_ms + encode_ms,
                sequence.len()
            );
        }
    }
}

#[cfg(test)]
mod backdrop_tests {
    use super::*;

    const PAPER: &str = "#F4EFE5";

    /// 只改整幅底色那一块，节点自己的填充一个都不许动。
    #[test]
    fn only_the_full_size_backdrop_is_recoloured() {
        let svg = render_svg("graph TD; A-->B;").expect("该渲得出 SVG");
        assert!(svg.contains("fill=\"#FFFFFF\""), "样本变了:{}", &svg[..200]);
        let painted = recolour_backdrop(&svg, PAPER);
        assert!(painted.contains("fill=\"#F4EFE5\""), "底色没换上");
        assert_eq!(painted.matches("#F4EFE5").count(), 1, "只该换那一块");
        // 节点的浅色填充照旧。
        assert_eq!(
            svg.matches("#F8FAFC").count(),
            painted.matches("#F8FAFC").count()
        );
    }

    /// 认不出那块底就原样返回——宁可留白底,也不要乱改别人的图元。
    #[test]
    fn anything_unexpected_is_left_alone() {
        // 第一个 rect 不在原点:是图元,不是底。
        let svg = r##"<svg width="10" height="10"><rect x="3" y="4" fill="#FFFFFF"/></svg>"##;
        assert_eq!(recolour_backdrop(svg, PAPER), svg);
        // `<svg>` 和 rect 之间隔着别的元素。
        let svg = r##"<svg width="10" height="10"><g/><rect x="0" y="0" fill="#FFFFFF"/></svg>"##;
        assert_eq!(recolour_backdrop(svg, PAPER), svg);
        // 压根没有 rect。
        let svg = r##"<svg width="10" height="10"><circle r="1"/></svg>"##;
        assert_eq!(recolour_backdrop(svg, PAPER), svg);
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    /// 换成 CSS 变量：WebUI 靠这个跟主题走（用户 09-22）。
    ///
    /// 只换列在表里的那几个色，别的一个不动——渲染器以后加新色不会被改花。
    #[test]
    fn only_listed_colours_are_repainted() {
        let svg = render_svg("graph TD; A-->B;").expect("该渲得出 SVG");
        let painted = repaint(&svg, &[(NODE_FILL, "var(--a)"), (CONNECTOR, "var(--b)")]);
        assert_eq!(
            painted.matches("var(--a)").count(),
            svg.matches(NODE_FILL).count()
        );
        assert_eq!(
            painted.matches("var(--b)").count(),
            svg.matches(CONNECTOR).count()
        );
        assert!(!painted.contains(NODE_FILL) && !painted.contains(CONNECTOR));
        // 没列进表的色原样保留。
        assert_eq!(
            painted.matches(NODE_STROKE).count(),
            svg.matches(NODE_STROKE).count()
        );
        assert_eq!(
            painted.matches(NODE_TEXT).count(),
            svg.matches(NODE_TEXT).count()
        );
    }

    /// 底可以换成任何 SVG 认的写法，包括 `transparent`。
    #[test]
    fn the_backdrop_accepts_a_keyword() {
        let svg = render_svg("graph TD; A-->B;").expect("该渲得出 SVG");
        let painted = recolour_backdrop(&svg, "transparent");
        assert!(painted.contains("fill=\"transparent\""), "底没换成透明");
        assert_eq!(painted.matches("transparent").count(), 1, "只该换那一块");
    }
}

#[cfg(test)]
mod terminal_backdrop_tests {
    use super::*;

    /// 终端底色得是中性的柔和灰（用户 09-22：纯白在深色终端里太刺眼），
    /// 而且要比节点填充深一档——不然节点浮不出来，整张图糊成一块。
    #[test]
    fn the_terminal_backdrop_is_a_soft_neutral_grey() {
        let [r, g, b] = TERMINAL_BACKDROP;
        assert_ne!([r, g, b], [255, 255, 255], "又变回纯白了");
        // 中性:三个分量彼此不差超过一点点,否则会偏色。
        let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
        assert!(hi - lo <= 8, "偏色了:{r:02X}{g:02X}{b:02X}");
        // 柔和:还是浅底(深色文字要看得清),但不到纯白,也别灰到扎眼
        // (用户 09-22 打回过一版 `#E4E4E7`:「有点太灰了」)。
        assert!((0xE8..0xF8).contains(&hi), "太深或太亮:{hi:02X}");

        // 比节点填充深:`#F8FAFC` 的最亮分量是 0xFC。
        let fill = u8::from_str_radix(&NODE_FILL[1..3], 16).unwrap();
        assert!(hi < fill, "底比节点还亮,节点浮不出来");
    }
}
