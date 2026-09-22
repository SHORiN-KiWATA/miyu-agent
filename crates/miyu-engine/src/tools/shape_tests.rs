use super::*;
use sha2::Digest;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("../../../../src/tools/fixtures/registry-shapes.json");

fn shape(registry: &ToolRegistry) -> BTreeMap<String, String> {
    registry
        .definitions()
        .into_iter()
        .map(|definition| {
            let payload = serde_json::to_string(&definition).unwrap();
            let digest = sha2::Sha256::digest(payload.as_bytes());
            (definition.function.name.clone(), hex::encode(digest))
        })
        .collect()
}

fn current() -> serde_json::Value {
    let temp = tempfile::tempdir().unwrap();
    let paths = tests::test_paths(temp.path());
    let mut config = AppConfig::default();
    // Arch 那套工具 09-23 起默认**跟着宿主走**（不是 Arch 就不注册）。而这份夹具
    // 是「工具面字节稳定」的守卫（AGENTS §1.1），必须平台无关：不钉死这一位的话，
    // 在 Arch 上生成的夹具到 Ubuntu / macOS 的 CI 上必然漂移，报
    // `missing=[archlinux_news, aur, …]`——那是宿主不同，不是工具面变了
    // （2026-09-23 CI 实测撞到）。
    //
    // 钉成 true：夹具覆盖的是「这些工具在时的形状」，真正要防的回归是它们的
    // schema 变了。它们注册与否由 `the_v4_migration…` 那几条单独把守。
    config.plugins.archlinux.enabled = true;
    serde_json::json!({
        "normal": shape(&builtin_registry(&config, &paths)),
        "dev": shape(&dev_registry(&config, &paths)),
        "restricted": shape(&restricted_platform_registry(&config, &paths)),
    })
}

#[test]
fn registry_shapes_match_fixture() {
    let expected: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    let actual = current();
    for face in ["normal", "dev", "restricted"] {
        let want = expected[face].as_object().cloned().unwrap_or_default();
        let got = actual[face].as_object().cloned().unwrap_or_default();
        let missing: Vec<_> = want.keys().filter(|k| !got.contains_key(*k)).collect();
        let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
        let changed: Vec<_> = want
            .iter()
            .filter(|(k, v)| got.get(*k).is_some_and(|g| g != *v))
            .map(|(k, _)| k)
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty() && changed.is_empty(),
            "{face} face drifted: missing={missing:?} extra={extra:?} changed={changed:?} \
                 (run `cargo test write_registry_shape_fixture -- --ignored` if intended)"
        );
    }
}

#[test]
#[ignore]
fn write_registry_shape_fixture() {
    let path = std::path::Path::new(miyu_base::WORKSPACE_ROOT)
        .join("src/tools/fixtures/registry-shapes.json");
    std::fs::write(&path, serde_json::to_string_pretty(&current()).unwrap()).unwrap();
    println!("wrote {}", path.display());
}
