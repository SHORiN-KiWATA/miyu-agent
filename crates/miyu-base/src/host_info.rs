//! Host facts that never change while the process runs: which OS this is,
//! which kernel it runs, and where Miyu keeps its own files.
//!
//! These ride the system prompt (the stable prefix) rather than the per-turn
//! `<runtime …/>` tail. The tail is fossilized into `turns.context_messages`
//! and replayed byte-for-byte forever, so a constant put there is paid once
//! per turn and accumulates in every later request; in the prefix it is paid
//! once and then served from the provider's cache.
//!
//! Collection is memoized for the same reason the block is static: a running
//! kernel does not change, and `prepare_for_turn` rebuilds the system prompt
//! on every single turn.

use serde_json::{json, Value};
use std::path::Path;
use std::sync::OnceLock;

/// os-release(5) lookup order: `/etc` overrides the vendor copy, and some
/// image-based distros ship only the latter.
const OS_RELEASE_PATHS: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

const MACOS_SYSTEM_VERSION: &str = "/System/Library/CoreServices/SystemVersion.plist";

/// Reads a file that is expected to be small, refusing anything that is not a
/// regular file or that grew past 64 KiB — these paths are host-controlled and
/// this runs on the prompt-building path.
pub fn read_small_file(path: &str) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn os_release_text() -> Option<String> {
    OS_RELEASE_PATHS
        .iter()
        .find_map(|path| read_small_file(path))
}

pub(crate) fn os_release_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key).then(|| value.trim().trim_matches('"').to_string())
    })
}

pub fn macos_system_version_text() -> Option<String> {
    read_small_file(MACOS_SYSTEM_VERSION)
}

pub fn parse_macos_system_version(raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    json!({
        "product_name": plist_value(raw, "ProductName"),
        "product_version": plist_value(raw, "ProductVersion"),
        "product_build_version": plist_value(raw, "ProductBuildVersion"),
    })
}

pub(crate) fn plist_value(raw: &str, key: &str) -> Option<String> {
    let marker = format!("<key>{key}</key>");
    let after_key = raw.split(&marker).nth(1)?;
    let after_string = after_key.split("<string>").nth(1)?;
    after_string
        .split("</string>")
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn os_release_pretty_name() -> Option<String> {
    let text = os_release_text()?;
    os_release_value(&text, "PRETTY_NAME").filter(|value| !value.trim().is_empty())
}

fn macos_product_name() -> Option<String> {
    let raw = macos_system_version_text()?;
    let product = plist_value(&raw, "ProductName")?;
    Some(match plist_value(&raw, "ProductVersion") {
        Some(version) => format!("{product} {version}"),
        None => product,
    })
}

/// Human-readable OS name. macOS has no os-release and Linux has no
/// SystemVersion.plist, so both probes run and the one matching the build
/// target goes first; `consts::OS` is the never-empty floor.
fn detect_os_name() -> String {
    let mut probes: [fn() -> Option<String>; 2] = [os_release_pretty_name, macos_product_name];
    if cfg!(target_os = "macos") {
        probes.reverse();
    }
    probes
        .iter()
        .find_map(|probe| probe())
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// `uname -r` without the subprocess. `libc` is already an unconditional
/// dependency and `uname(2)` is the same call on Linux and macOS, so this
/// stays portable without forking or reading Linux-only `/proc` entries.
#[cfg(unix)]
fn detect_kernel_release() -> Option<String> {
    // SAFETY: `utsname` is a plain byte-array struct with no invalid bit
    // patterns, `uname` only writes into the buffer we own, and on success it
    // NUL-terminates every field.
    let release = unsafe {
        let mut info: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut info) != 0 {
            return None;
        }
        std::ffi::CStr::from_ptr(info.release.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    let release = release.trim().to_string();
    (!release.is_empty()).then_some(release)
}

#[cfg(not(unix))]
fn detect_kernel_release() -> Option<String> {
    None
}

fn host_os_facts() -> &'static (String, Option<String>) {
    static FACTS: OnceLock<(String, Option<String>)> = OnceLock::new();
    FACTS.get_or_init(|| (detect_os_name(), detect_kernel_release()))
}

/// 沙盒信息不在这里(09-23 起):它会随用户按 Tab 切只读而变,写在系统提示词里
/// 每切一次就掰断整段前缀缓存。改由 [`sandbox_notice`] 走「变了才追加」的尾巴。
pub fn host_environment_block_full(
    root_dir: &Path,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    let (os, kernel) = host_os_facts();
    let mut block = format!("<host-environment os=\"{}\"", xml_attr_escape(os));
    // Omitted rather than reported as "unknown": an absent attribute costs
    // nothing and cannot be mistaken for a fact.
    if let Some(kernel) = kernel {
        block.push_str(&format!(" kernel=\"{}\"", xml_attr_escape(kernel)));
    }
    block.push_str(&format!(
        " miyu_home=\"{}\"",
        xml_attr_escape(&root_dir.display().to_string())
    ));
    block.push_str(&format!(
        " harness=\"Miyu {}\"",
        xml_attr_escape(env!("CARGO_PKG_VERSION"))
    ));
    if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
        block.push_str(&format!(" model=\"{}\"", xml_attr_escape(model)));
    }
    if let Some(effort) = effort.map(str::trim).filter(|value| !value.is_empty()) {
        block.push_str(&format!(" effort=\"{}\"", xml_attr_escape(effort)));
    }
    block.push_str("/>");
    block
}

/// 沙盒关掉时追加的那一条:之前告诉过她「关在哪」,现在得说一声不关了。
pub const SANDBOX_OFF_NOTICE: &str = "<sandbox state=\"off\"/>";

/// 这一轮的沙盒(09-11 成员,09-13 起 `/sandbox` 会话,09-23 起只读模式):模型得
/// 知道自己关在哪、还能碰什么,别去猜为什么写文件会被拒。属性由策略摘要生成,
/// 同一份策略两次生成逐字节相等——「变了才追加」靠逐字节比对上一份。
pub fn sandbox_notice(policy: &crate::sandbox::SandboxPolicy) -> String {
    // 后端与「读收没收」必须照实报。macOS 走 sandbox-exec，而那一版**只收写**
    // （见 `sandbox::macos` 模块头的三条真机探测）：照搬 landlock 的措辞会让
    // 模型以为读也被关住，据此做出错误判断——比如以为读不到的东西就不必回避。
    let (backend, readable) = if cfg!(target_os = "macos") {
        (
            "sandbox-exec",
            "everything (this backend confines writes only)".to_string(),
        )
    } else {
        ("landlock", policy.readable_summary.join(", "))
    };
    let mode = if policy.read_only_mode {
        " mode=\"read-only\""
    } else {
        ""
    };
    format!(
        "<sandbox{mode} backend=\"{}\" root=\"{}\" writable=\"{}\" readable=\"{}\"/>",
        backend,
        xml_attr_escape(&policy.root.display().to_string()),
        xml_attr_escape(&policy.writable_summary.join(", ")),
        xml_attr_escape(&readable),
    )
}

pub fn xml_attr_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn os_release_value_reads_quoted_and_bare_entries() {
        let text = "NAME=\"Arch Linux\"\nPRETTY_NAME=\"Arch Linux\"\nID=arch\nBUILD_ID=rolling";
        assert_eq!(
            os_release_value(text, "PRETTY_NAME").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(os_release_value(text, "ID").as_deref(), Some("arch"));
        assert_eq!(os_release_value(text, "VERSION_ID"), None);
        // A prefix match must not win: `ID` and `BUILD_ID` share a suffix.
        assert_eq!(
            os_release_value(text, "BUILD_ID").as_deref(),
            Some("rolling")
        );
    }

    #[test]
    fn macos_plist_yields_product_name_and_version() {
        let raw = "<key>ProductName</key><string>macOS</string>\
                   <key>ProductVersion</key><string>15.2</string>\
                   <key>ProductBuildVersion</key><string>24C101</string>";
        assert_eq!(plist_value(raw, "ProductName").as_deref(), Some("macOS"));
        assert_eq!(plist_value(raw, "ProductVersion").as_deref(), Some("15.2"));
        assert_eq!(plist_value(raw, "Missing"), None);
        let parsed = parse_macos_system_version(Some(raw));
        assert_eq!(parsed["product_build_version"], json!("24C101"));
        assert_eq!(parse_macos_system_version(None), Value::Null);
    }

    #[test]
    fn detected_os_name_is_never_empty() {
        assert!(!detect_os_name().trim().is_empty());
    }

    #[test]
    fn host_block_is_a_single_self_closing_tag_with_the_real_root() {
        let block = host_environment_block_full(
            &PathBuf::from("/home/tester/.miyu"),
            Some("stub/stub-a"),
            Some("high"),
        );
        assert!(block.contains(" harness=\"Miyu "));
        assert!(block.contains(" model=\"stub/stub-a\""));
        assert!(block.contains(" effort=\"high\""));
        assert!(block.starts_with("<host-environment os=\""));
        assert!(block.ends_with("/>"));
        assert!(block.contains(" miyu_home=\"/home/tester/.miyu\""));
        assert!(!block.contains('\n'));
        // No placeholder values leak in when a probe comes back empty.
        assert!(!block.contains("\"\""));
        assert!(!block.contains("unknown"));
    }

    /// 沙盒说明来自策略摘要:根 + 可写 + 可读,两次生成逐字节相等(「变了才追加」
    /// 靠逐字节比对);只读模式多一个 mode。环境块里不再有沙盒(09-23)。
    #[test]
    fn sandbox_notice_carries_the_summary_byte_stably() {
        let mut policy = crate::sandbox::SandboxPolicy {
            root: PathBuf::from("/home/tester/proj"),
            writable_summary: vec!["root".into(), "/tmp".into(), "~/.cargo".into()],
            readable_summary: vec!["root".into(), "/tmp".into(), "system dirs".into()],
            ..Default::default()
        };
        // 后端与「读收没收」按平台不同——macOS 那一版只收写，照搬 landlock 的
        // 措辞会让模型以为读也关住了（见 `sandbox::macos` 模块头）。
        let expected = if cfg!(target_os = "macos") {
            "<sandbox backend=\"sandbox-exec\" root=\"/home/tester/proj\" writable=\"root, /tmp, ~/.cargo\" readable=\"everything (this backend confines writes only)\"/>"
        } else {
            "<sandbox backend=\"landlock\" root=\"/home/tester/proj\" writable=\"root, /tmp, ~/.cargo\" readable=\"root, /tmp, system dirs\"/>"
        };
        assert_eq!(sandbox_notice(&policy), expected);
        assert_eq!(sandbox_notice(&policy), sandbox_notice(&policy));
        policy.read_only_mode = true;
        assert!(sandbox_notice(&policy).starts_with("<sandbox mode=\"read-only\" backend="));
        let root = PathBuf::from("/home/tester/.miyu");
        assert!(!host_environment_block_full(&root, Some("stub/a"), None).contains("sandbox"));
    }

    #[test]
    fn host_block_escapes_paths_that_would_break_the_attribute() {
        let block = host_environment_block_full(&PathBuf::from("/tmp/a\"b&c"), None, None);
        // miyu_home 后面还有 harness 属性,不再是最后一个
        assert!(block.contains(" miyu_home=\"/tmp/a&quot;b&amp;c\" harness=\"Miyu "));
    }

    #[cfg(unix)]
    #[test]
    fn kernel_release_is_available_on_unix() {
        let release = detect_kernel_release().expect("uname should report a release on unix");
        assert!(!release.contains('\0'));
        assert_eq!(release, release.trim());
    }
}
