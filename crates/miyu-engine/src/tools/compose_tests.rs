//! `compose_registry` 按子系统启用快照裁工具面:清单关掉的子系统一件工具都不注册,
//! 快照说开的才在。

use super::tests::test_paths;
use super::*;
use miyu_base::config::PersonaManifest;

fn names(manifest: &PersonaManifest, config: &AppConfig, paths: &MiyuPaths) -> Vec<String> {
    compose_registry(config, paths, manifest, Surface::owner(true)).tool_names()
}

#[test]
fn persona_switches_drop_whole_subsystems_from_the_tool_face() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let mut config = AppConfig::default();
    config.voice.enabled = true;
    let all = names(&PersonaManifest::all(), &config, &paths);
    for expected in ["end_voice_chat", "load_skill", "recall_memories"] {
        assert!(all.contains(&expected.to_string()), "全开面少了 {expected}");
    }
    // 老格式的 `subsystems.skills = false` 故意留着(09-24 技能并入插件):读入时
    // 折成「技能插件不在名单里」,工具面上技能那几件照样不出现。
    let off =
        PersonaManifest::parse("[subsystems]\nvoice = false\nskills = false\nmemory = false\n")
            .unwrap();
    let snapshot = off.enabled_subsystems(&config);
    assert!(!snapshot.voice && !snapshot.memory && !off.skills_enabled(&config));
    let trimmed = names(&off, &config, &paths);
    for gone in [
        "end_voice_chat",
        "speak",
        "load_skill",
        "manage_skill",
        "recall_memories",
        "remember_fact",
        "search_evicted_context",
    ] {
        assert!(
            !trimmed.contains(&gone.to_string()),
            "清单关了还注册 {gone}"
        );
    }
    // 核心件不受清单影响。
    for kept in ["run_command", "edit", "subagent", "todowrite"] {
        assert!(trimmed.contains(&kept.to_string()), "裁掉了核心件 {kept}");
    }
}
