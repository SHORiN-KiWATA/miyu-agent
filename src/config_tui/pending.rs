//! 不在 `config.jsonc` 里、但要跟着「保存并退出」一起落盘的改动。
//!
//! 人格清单（`persona.toml`）与开发模式提示词（`dev-prompt.md`）是各自独立的
//! 文件，2026-09-20 起攒在这里：改完只进内存，按「保存并退出」（或退出时那句
//! 「保存吗」答是）才一起写盘，答否就整份丢掉——和设置界面其余部分同一个口径
//! （在这之前它们是改完即写，用户指出这跟别处不一样）。
//!
//! 读的那一侧也得走这里：功能表二进二出、人格菜单上的计数，都要看得见这一轮
//! 还没写盘的改动，否则退出去再进来会看到旧的。
//!
//! 09-26 起人格正文、防失忆提示、预设对话、用户身份也攒着（[`PersonaDrafts`]，用户：
//! 编辑表单去掉「保存 / 返回」之后，这几张也要归「保存并退出」管）。它们要在存配置
//! **之前**落（[`PendingWrites::flush_before_config`]），清单和开发提示词在存配置之后。

use crate::config_tui::*;
use miyu_base::config::PersonaManifest;
use std::collections::BTreeMap;

#[derive(Default)]
pub(in crate::config_tui) struct PendingWrites {
    /// 人格 scope → 待写的清单。切过人格就可能攒下不止一份。
    manifests: BTreeMap<String, PersonaManifest>,
    /// 开发模式提示词的正文；空串 = 清空（落盘时删文件，回退内置默认）。
    dev_prompt: Option<String>,
    /// 人格与用户身份攒着的改动。
    pub(in crate::config_tui) drafts: PersonaDrafts,
}

impl PendingWrites {
    pub(in crate::config_tui) fn is_empty(&self) -> bool {
        self.manifests.is_empty() && self.dev_prompt.is_none() && self.drafts.is_empty()
    }

    /// 这一层人格此刻的清单：先看这一轮改过没有，没有再读盘。
    pub(in crate::config_tui) fn manifest(
        &self,
        config: &AppConfig,
        paths: &MiyuPaths,
        scope: &str,
    ) -> PersonaManifest {
        self.manifests
            .get(scope)
            .cloned()
            // 攒着改名的人格，盘上还在老名字的目录里。
            .unwrap_or_else(|| PersonaManifest::load(config, paths, &self.drafts.disk_scope(scope)))
    }

    pub(in crate::config_tui) fn set_manifest(&mut self, scope: &str, manifest: PersonaManifest) {
        self.manifests.insert(scope.to_string(), manifest);
    }

    /// 攒下一次人格编辑（键是盘上的名字）。改了名的话，这一轮攒着的功能清单跟着换到新
    /// scope 名下——落盘时目录先搬过去，清单才写得进新名字那里。
    pub(in crate::config_tui) fn set_persona_draft(&mut self, disk: String, draft: PersonaDraft) {
        let shown = self
            .drafts
            .persona_by_disk(&disk)
            .map(|previous| previous.name.clone())
            .unwrap_or_else(|| disk.clone());
        let (from, to) = (
            miyu_base::config::persona_scope_name(&shown),
            miyu_base::config::persona_scope_name(&draft.name),
        );
        if from != to {
            if let Some(manifest) = self.manifests.remove(&from) {
                self.manifests.insert(to, manifest);
            }
        }
        self.drafts.set_persona(disk, draft);
    }

    /// 删掉界面上这个人格：它攒着的编辑和功能清单都作废，交回盘上的名字。
    pub(in crate::config_tui) fn forget_persona(&mut self, shown: &str) -> String {
        self.manifests
            .remove(&miyu_base::config::persona_scope_name(shown));
        self.drafts.forget_persona(shown)
    }

    /// 人格、用户身份落盘：在存配置之前（见 [`PersonaDrafts::flush`]）。
    pub(in crate::config_tui) fn flush_before_config(
        &mut self,
        config: &mut AppConfig,
        paths: &MiyuPaths,
    ) -> Result<()> {
        self.drafts.flush(config, paths)
    }

    /// 开发模式提示词此刻的正文。
    pub(in crate::config_tui) fn dev_prompt(&self, paths: &MiyuPaths) -> String {
        self.dev_prompt.clone().unwrap_or_else(|| {
            std::fs::read_to_string(paths.config_dir.join(miyu_base::config::DEV_PROMPT_FILE))
                .unwrap_or_default()
        })
    }

    pub(in crate::config_tui) fn set_dev_prompt(&mut self, text: String) {
        self.dev_prompt = Some(text);
    }

    /// 全部落盘。配置本身已经存过了，这里只管那几个独立文件。
    pub(in crate::config_tui) fn flush(
        &mut self,
        config: &AppConfig,
        paths: &MiyuPaths,
    ) -> Result<()> {
        for (scope, manifest) in std::mem::take(&mut self.manifests) {
            let path = PersonaManifest::manifest_path(config, paths, &scope);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, manifest.to_toml())?;
        }
        if let Some(text) = self.dev_prompt.take() {
            let path = paths.config_dir.join(miyu_base::config::DEV_PROMPT_FILE);
            let text = text.trim();
            if text.is_empty() {
                if path.exists() {
                    std::fs::remove_file(&path)?;
                }
            } else {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, format!("{text}\n"))?;
            }
        }
        Ok(())
    }
}
