//! 人格与用户身份的编辑攒到「保存并退出」才写（用户 09-26）。

use crate::config_tui::*;
use miyu_base::config::{persona_scope_name, PersonaManifest};

fn test_paths(root: &std::path::Path) -> MiyuPaths {
    MiyuPaths {
        root_dir: root.to_path_buf(),
        config_dir: root.join("config"),
        config_file: root.join("config/config.jsonc"),
        skills_dir: root.join("config/skills"),
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        state_dir: root.join("state"),
        pictures_dir: root.join("pictures"),
        fish_hook_file: root.join("fish/miyu.fish"),
        bash_hook_file: root.join("shell/bash-hook.sh"),
        zsh_hook_file: root.join("shell/zsh-hook.zsh"),
        scripts_dir: root.join("config/scripts"),
        system_scripts_dir: PathBuf::new(),
    }
}

fn draft(name: &str, content: &str) -> PersonaDraft {
    PersonaDraft {
        name: name.to_string(),
        content: content.to_string(),
        hint: String::new(),
        dialogs: String::new(),
    }
}

/// 改了名、还没保存：界面上列新名字，盘上的老名字还占着，功能清单在盘上老 scope 里。
#[test]
fn a_renamed_persona_shows_its_new_name_until_it_is_saved() {
    let mut drafts = PersonaDrafts::default();
    drafts.set_persona("甲.md".to_string(), draft("乙.md", "新正文"));
    let names = drafts.persona_names(vec!["甲.md".to_string(), "丙.md".to_string()]);
    assert!(names.contains(&"乙.md".to_string()) && names.contains(&"丙.md".to_string()));
    assert!(!names.contains(&"甲.md".to_string()), "{names:?}");
    assert_eq!(drafts.persona_disk_name("乙.md"), "甲.md");
    assert_eq!(drafts.persona_disk_name("丙.md"), "丙.md");
    assert_eq!(drafts.persona("乙.md").unwrap().content, "新正文");
    assert!(drafts.persona_renamed_away("甲.md"));
    assert!(!drafts.persona_renamed_away("丙.md"));
    assert_eq!(
        drafts.disk_scope(&persona_scope_name("乙.md")),
        persona_scope_name("甲.md")
    );

    assert_eq!(drafts.forget_persona("乙.md"), "甲.md");
    assert!(drafts.is_empty());
}

/// 攒着的功能清单跟着改名走：落盘时目录先搬过去，清单才写得进新名字那里。删掉就一起作废。
#[test]
fn staged_features_follow_a_rename_and_die_with_a_delete() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    let mut marked = PersonaManifest::all();
    marked.subsystems.voice = !marked.subsystems.voice;

    let mut pending = PendingWrites::default();
    pending.set_manifest(&persona_scope_name("甲.md"), marked.clone());
    pending.set_persona_draft("甲.md".to_string(), draft("乙.md", "正文"));
    assert_eq!(
        pending.manifest(&config, &paths, &persona_scope_name("乙.md")),
        marked
    );
    assert_ne!(
        pending.manifest(&config, &paths, &persona_scope_name("甲.md")),
        marked,
        "老 scope 下不该还挂着那份"
    );

    assert_eq!(pending.forget_persona("乙.md"), "甲.md");
    assert_ne!(
        pending.manifest(&config, &paths, &persona_scope_name("乙.md")),
        marked
    );
    assert!(pending.is_empty());
}

/// 没保存的改名：新名字被占了，盘上那个老名字也还占着（落盘前文件还在）。
#[test]
fn names_being_renamed_away_are_still_taken() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    write_persona(&paths, &config, "甲.md", "正文").unwrap();
    write_persona(&paths, &config, "丙.md", "正文").unwrap();
    let mut drafts = PersonaDrafts::default();
    drafts.set_persona("甲.md".to_string(), draft("乙.md", "正文"));

    assert!(ensure_draft_name_available(&paths, &config, &drafts, "乙.md", None).is_err());
    assert!(ensure_draft_name_available(&paths, &config, &drafts, "甲.md", None).is_err());
    assert!(ensure_draft_name_available(&paths, &config, &drafts, "丙.md", None).is_err());
    assert!(ensure_draft_name_available(&paths, &config, &drafts, "丁.md", None).is_ok());
    // 改回自己原来的名字不算撞。
    assert!(ensure_draft_name_available(&paths, &config, &drafts, "甲.md", Some("乙.md")).is_ok());
}

/// 攒着的时候盘上一个字不动；「保存并退出」时改名、正文、附属文件、Miyu 附加、用户身份一起落。
#[test]
fn nothing_is_written_until_the_drafts_are_flushed() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    paths.create_dirs().unwrap();
    let mut config = AppConfig::default();
    write_persona(&paths, &config, "甲.md", "旧正文").unwrap();
    write_identity(&paths, &config, "我.md", "旧身份").unwrap();
    config.prompt.active_persona = "乙.md".to_string();

    let mut pending = PendingWrites::default();
    pending.set_persona_draft(
        "甲.md".to_string(),
        PersonaDraft {
            name: "乙.md".to_string(),
            content: "新正文".to_string(),
            hint: "新提示".to_string(),
            dialogs: String::new(),
        },
    );
    pending
        .drafts
        .set_miyu_extras("Miyu 新提示".to_string(), String::new());
    pending.drafts.set_identity(
        "我.md".to_string(),
        IdentityDraft {
            name: "新我.md".to_string(),
            content: "新身份".to_string(),
        },
    );

    assert_eq!(
        read_persona(&paths, &config, "甲.md").unwrap().trim(),
        "旧正文"
    );
    assert!(!config.persona_path(&paths, "乙.md").exists());
    assert_eq!(
        read_identity(&paths, &config, "我.md").unwrap().trim(),
        "旧身份"
    );

    pending.flush_before_config(&mut config, &paths).unwrap();

    assert!(pending.is_empty());
    assert!(!config.persona_path(&paths, "甲.md").exists());
    assert_eq!(
        read_persona(&paths, &config, "乙.md").unwrap().trim(),
        "新正文"
    );
    let (hint, _) = persona_aux_values(&paths, &config, &persona_scope_name("乙.md"));
    assert_eq!(hint, "新提示");
    let miyu_hint = std::fs::read_to_string(miyu_core::persona_hint::manual_hint_path(
        &config, &paths, "default",
    ))
    .unwrap();
    assert_eq!(miyu_hint.trim(), "Miyu 新提示");
    assert!(!config.identity_path(&paths, "我.md").exists());
    assert_eq!(
        read_identity(&paths, &config, "新我.md").unwrap().trim(),
        "新身份"
    );
}
