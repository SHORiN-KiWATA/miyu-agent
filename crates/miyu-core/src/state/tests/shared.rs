//! 状态层测试共用的 fixture。

use crate::state::*;

pub(super) fn test_paths(root: &Path) -> MiyuPaths {
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

/// 沙箱里的会话库。按人分库之后路径由布局推导，测试直接找文件更省事。
pub(super) fn find_db(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_db(&path) {
                return Some(found);
            }
        } else if path
            .file_name()
            .is_some_and(|name| name == "conversation.db")
        {
            return Some(path);
        }
    }
    None
}

pub(super) fn test_store() -> (tempfile::TempDir, StateStore) {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(&test_paths(temp.path())).unwrap();
    (temp, store)
}

pub(super) fn platform_binding_key(
    conversation_id: &str,
    participant_id: Option<&str>,
    persona: &str,
) -> PlatformSessionBindingKey {
    PlatformSessionBindingKey {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        conversation_kind: "group".to_string(),
        conversation_id: conversation_id.to_string(),
        participant_id: participant_id.map(str::to_string),
        persona: persona.to_string(),
    }
}

pub(super) fn platform_meme_ref(
    conversation_id: &str,
    message_id: &str,
    library: &str,
    meme_id: &str,
    direction: &str,
    created_at: &str,
) -> PlatformMemeRefRecord {
    PlatformMemeRefRecord {
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        conversation_kind: "group".to_string(),
        conversation_id: conversation_id.to_string(),
        message_id: message_id.to_string(),
        library: library.to_string(),
        meme_id: meme_id.to_string(),
        direction: direction.to_string(),
        created_at: created_at.to_string(),
    }
}

pub(super) fn plugin_scope(conversation_id: &str) -> PlatformPluginScopeKey {
    PlatformPluginScopeKey {
        plugin_id: "reply_processor".to_string(),
        platform: "onebot".to_string(),
        account_id: "10000".to_string(),
        conversation_kind: "group".to_string(),
        conversation_id: conversation_id.to_string(),
    }
}
