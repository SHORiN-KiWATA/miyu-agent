//! 人格与用户身份的待写改动（用户 09-26：编辑人格、Miyu 人格附加、编辑用户身份也跟设置界面
//! 其余部分一样，改完只进内存，按「保存并退出」才一起写，选「不保存」就整批丢掉）。
//!
//! 键一律是**盘上**的文件名：改名要到落盘时才真的搬（[`apply_persona_edit`] 连着搬目录、迁
//! 库里的归属、改配置里的引用），这之前盘上还是老名字，界面上显示的却已经是新名字——读的
//! 一侧（列表、正文、附属文件、功能清单）都经这里换算。
//!
//! 新建、删除不在这里攒：新建是带「保存 / 返回」的表单，按了保存当场写；删除当场删。它们和
//! 攒着的改动碰到一起的两处由调用方处理——新建查重要连盘上被改名改走的老名字一起算（落盘前
//! 它还占着那个文件），删除要先把这一项攒着的改动作废（[`PersonaDrafts::forget_persona`]）。

use crate::config_tui::*;
use miyu_base::config::persona_scope_name;
use std::collections::BTreeMap;

/// 编辑人格攒下的样子。`name` 和键不同 = 改了名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::config_tui) struct PersonaDraft {
    pub(in crate::config_tui) name: String,
    pub(in crate::config_tui) content: String,
    pub(in crate::config_tui) hint: String,
    pub(in crate::config_tui) dialogs: String,
}

/// 编辑用户身份攒下的样子。`name` 和键不同 = 改了名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::config_tui) struct IdentityDraft {
    pub(in crate::config_tui) name: String,
    pub(in crate::config_tui) content: String,
}

#[derive(Default)]
pub(in crate::config_tui) struct PersonaDrafts {
    /// 盘上的人格文件名 → 这一轮改成的样子。
    personas: BTreeMap<String, PersonaDraft>,
    /// Miyu（`default`）的防失忆提示与预设对话。
    miyu_extras: Option<(String, String)>,
    /// 盘上的用户身份文件名 → 这一轮改成的样子。
    identities: BTreeMap<String, IdentityDraft>,
}

impl PersonaDrafts {
    pub(in crate::config_tui) fn is_empty(&self) -> bool {
        self.personas.is_empty() && self.miyu_extras.is_none() && self.identities.is_empty()
    }

    // —— 人格 ——

    /// 界面上该列出的人格：盘上的名字按攒着的改名换过来。
    pub(in crate::config_tui) fn persona_names(&self, on_disk: Vec<String>) -> Vec<String> {
        let mut names: Vec<String> = on_disk
            .into_iter()
            .map(|disk| match self.personas.get(&disk) {
                Some(draft) => draft.name.clone(),
                None => disk,
            })
            .collect();
        names.sort();
        names
    }

    /// 界面上这个名字在盘上叫什么。
    pub(in crate::config_tui) fn persona_disk_name(&self, shown: &str) -> String {
        self.personas
            .iter()
            .find(|(_, draft)| draft.name == shown)
            .map(|(disk, _)| disk.clone())
            .unwrap_or_else(|| shown.to_string())
    }

    /// 界面上这个人格攒着的样子（没改过就是 `None`，照盘上读）。
    pub(in crate::config_tui) fn persona(&self, shown: &str) -> Option<&PersonaDraft> {
        self.personas.get(&self.persona_disk_name(shown))
    }

    /// 盘上这个名字攒着的样子。
    pub(in crate::config_tui) fn persona_by_disk(&self, disk: &str) -> Option<&PersonaDraft> {
        self.personas.get(disk)
    }

    /// 盘上这个名字正被改名改走：落盘前它还占着那个文件，新建、改名都不能用它。
    pub(in crate::config_tui) fn persona_renamed_away(&self, disk: &str) -> bool {
        self.personas
            .get(disk)
            .is_some_and(|draft| draft.name != disk)
    }

    pub(in crate::config_tui) fn set_persona(&mut self, disk: String, draft: PersonaDraft) {
        self.personas.insert(disk, draft);
    }

    /// 删掉界面上这个人格：攒着的改动作废，交回它在盘上的名字（删的是盘上那份）。
    pub(in crate::config_tui) fn forget_persona(&mut self, shown: &str) -> String {
        let disk = self.persona_disk_name(shown);
        self.personas.remove(&disk);
        disk
    }

    /// 功能清单按 scope 存在人格目录里：界面上的新 scope 在落盘前还是盘上老名字那个。
    pub(in crate::config_tui) fn disk_scope(&self, scope: &str) -> String {
        self.personas
            .iter()
            .find(|(disk, draft)| persona_scope_name(&draft.name) == scope && **disk != draft.name)
            .map(|(disk, _)| persona_scope_name(disk))
            .unwrap_or_else(|| scope.to_string())
    }

    // —— Miyu 附加 ——

    pub(in crate::config_tui) fn miyu_extras(&self) -> Option<&(String, String)> {
        self.miyu_extras.as_ref()
    }

    pub(in crate::config_tui) fn set_miyu_extras(&mut self, hint: String, dialogs: String) {
        self.miyu_extras = Some((hint, dialogs));
    }

    // —— 用户身份 ——

    pub(in crate::config_tui) fn identity_names(&self, on_disk: Vec<String>) -> Vec<String> {
        let mut names: Vec<String> = on_disk
            .into_iter()
            .map(|disk| match self.identities.get(&disk) {
                Some(draft) => draft.name.clone(),
                None => disk,
            })
            .collect();
        names.sort();
        names
    }

    pub(in crate::config_tui) fn identity_disk_name(&self, shown: &str) -> String {
        self.identities
            .iter()
            .find(|(_, draft)| draft.name == shown)
            .map(|(disk, _)| disk.clone())
            .unwrap_or_else(|| shown.to_string())
    }

    pub(in crate::config_tui) fn identity(&self, shown: &str) -> Option<&IdentityDraft> {
        self.identities.get(&self.identity_disk_name(shown))
    }

    pub(in crate::config_tui) fn identity_renamed_away(&self, disk: &str) -> bool {
        self.identities
            .get(disk)
            .is_some_and(|draft| draft.name != disk)
    }

    pub(in crate::config_tui) fn set_identity(&mut self, disk: String, draft: IdentityDraft) {
        self.identities.insert(disk, draft);
    }

    pub(in crate::config_tui) fn forget_identity(&mut self, shown: &str) -> String {
        let disk = self.identity_disk_name(shown);
        self.identities.remove(&disk);
        disk
    }

    /// 全部落盘。要排在存配置**之前**：人格改名会把配置里的引用一并改好存下
    /// （[`apply_persona_edit`]），配置在后面存，才不会先写下一个盘上还不存在的名字。
    /// 一件落下就划掉一件——中途失败，再按一次保存不会把已经落下的重做一遍。
    pub(in crate::config_tui) fn flush(
        &mut self,
        config: &mut AppConfig,
        paths: &MiyuPaths,
    ) -> Result<()> {
        while let Some((disk, draft)) = self
            .personas
            .iter()
            .next()
            .map(|(disk, draft)| (disk.clone(), draft.clone()))
        {
            apply_persona_edit(paths, config, &disk, &draft.name, &draft.content)?;
            write_persona_aux(
                paths,
                config,
                &persona_scope_name(&draft.name),
                &draft.hint,
                &draft.dialogs,
            )?;
            self.personas.remove(&disk);
        }
        if let Some((hint, dialogs)) = &self.miyu_extras {
            write_persona_aux(paths, config, "default", hint, dialogs)?;
            self.miyu_extras = None;
        }
        while let Some((disk, draft)) = self
            .identities
            .iter()
            .next()
            .map(|(disk, draft)| (disk.clone(), draft.clone()))
        {
            write_identity(paths, config, &draft.name, &draft.content)?;
            if draft.name != disk {
                let old = config.identity_path(paths, &disk);
                if old.exists() {
                    std::fs::remove_file(old)?;
                }
            }
            self.identities.remove(&disk);
        }
        Ok(())
    }
}
