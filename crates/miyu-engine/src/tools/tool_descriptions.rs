use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadPolicy {
    Summary,
    Group,
    Hidden,
}

impl Default for LoadPolicy {
    fn default() -> Self {
        Self::Summary
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolDescription {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub parameters: Value,
    pub always_loaded: bool,
    #[serde(default)]
    pub load_policy: LoadPolicy,
    #[serde(default)]
    pub groups: Vec<String>,
    /// 按工具超时（秒）。缺省=吃 registry 默认兜底；0=豁免（自管超时或
    /// 天生长跑的工具，如 run_command/subagent）。
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// 场所信任位:`"trust": "external"` 的工具也给不可信入口(QQ 群、远端
    /// WebUI 成员)。缺省只给属主。受限注册表就是按这个位从全量面上筛出来的。
    #[serde(default)]
    pub trust: crate::tools::ToolTrust,
}

static TOOL_DESCRIPTIONS: OnceLock<HashMap<String, ToolDescription>> = OnceLock::new();

macro_rules! tool_description_files {
    () => {
        [
            include_str!("../../../../src/tools/descriptions/alarm.json"),
            include_str!("../../../../src/tools/descriptions/archlinux_news.json"),
            include_str!(
                "../../../../src/tools/descriptions/archlinux_official_package_query.json"
            ),
            include_str!("../../../../src/tools/descriptions/archwiki_query.json"),
            include_str!("../../../../src/tools/descriptions/artifact.json"),
            include_str!("../../../../src/tools/descriptions/ask_question.json"),
            include_str!("../../../../src/tools/descriptions/goal.json"),
            include_str!("../../../../src/tools/descriptions/aur.json"),
            include_str!("../../../../src/tools/descriptions/edit.json"),
            include_str!("../../../../src/tools/descriptions/generate_image.json"),
            include_str!("../../../../src/tools/descriptions/get_exchange_rate.json"),
            include_str!("../../../../src/tools/descriptions/glob.json"),
            include_str!("../../../../src/tools/descriptions/grep.json"),
            include_str!("../../../../src/tools/descriptions/install_aur_package.json"),
            include_str!("../../../../src/tools/descriptions/kb.json"),
            include_str!("../../../../src/tools/descriptions/load_skill.json"),
            include_str!("../../../../src/tools/descriptions/ledger.json"),
            include_str!("../../../../src/tools/descriptions/manage_ledger.json"),
            include_str!("../../../../src/tools/descriptions/manage_script.json"),
            include_str!("../../../../src/tools/descriptions/manage_meme.json"),
            include_str!("../../../../src/tools/descriptions/manage_skill.json"),
            include_str!("../../../../src/tools/descriptions/use_meme.json"),
            include_str!("../../../../src/tools/descriptions/present_artifact.json"),
            include_str!("../../../../src/tools/descriptions/print_image.json"),
            include_str!("../../../../src/tools/descriptions/read.json"),
            include_str!("../../../../src/tools/descriptions/recall_memories.json"),
            include_str!("../../../../src/tools/descriptions/remember_fact.json"),
            include_str!("../../../../src/tools/descriptions/review_aur_package.json"),
            include_str!("../../../../src/tools/descriptions/run_command.json"),
            include_str!("../../../../src/tools/descriptions/search_evicted_context.json"),
            include_str!("../../../../src/tools/descriptions/search_knowledge_base.json"),
            include_str!("../../../../src/tools/descriptions/search_web_images.json"),
            include_str!("../../../../src/tools/descriptions/send_to_other_running_session.json"),
            include_str!("../../../../src/tools/descriptions/share_file.json"),
            include_str!("../../../../src/tools/descriptions/subagent.json"),
            include_str!("../../../../src/tools/descriptions/todowrite.json"),
            include_str!("../../../../src/tools/descriptions/trash_path.json"),
            include_str!("../../../../src/tools/descriptions/vision_analyze.json"),
            include_str!("../../../../src/tools/descriptions/web_fetch.json"),
            include_str!("../../../../src/tools/descriptions/web_search.json"),
        ]
    };
}

pub fn all() -> &'static HashMap<String, ToolDescription> {
    TOOL_DESCRIPTIONS.get_or_init(|| {
        let mut map = HashMap::new();
        for raw in tool_description_files!() {
            let desc: ToolDescription =
                serde_json::from_str(raw).expect("built-in tool description JSON must be valid");
            map.insert(desc.name.clone(), desc);
        }
        map
    })
}

pub fn get(name: &str) -> Option<&'static ToolDescription> {
    all().get(name)
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;

/// 事件名 → 工具底名的判定已下沉到 `miyu_base::tool_names`(基础层,和 `is_command_tool`
/// 同住)。老路径 `crate::tools::tool_event_base_name` 靠这条再导出保持不变(09-16)。
pub use miyu_base::tool_names::tool_event_base_name;
