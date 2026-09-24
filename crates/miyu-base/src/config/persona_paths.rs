//! 人格相关的路径与提示词组装。
//!
//! 每个人格有自己的提示词、记忆库、技能目录。`active_persona_*` 是「当前人格
//! 的那一份」，`dev_scoped` 处理开发模式——它共用目录但要走另一套提示词。
//!
//! `system_prompt_for` 按受众拼提示词：属主能看到主机环境，群里的人不能。

use crate::config::*;

impl AppConfig {
    /// Dev 模式系统提示词:读 `config/dev-prompt.md`,默认为空(09-24 起没有内置
    /// 角色句,用户自己写什么就发什么)。文件缺失、为空,或恰好是老版本自动写进去的
    /// 那行默认([`LEGACY_DEV_SYSTEM_PROMPT`]),都当没写。
    pub fn dev_system_prompt(&self, paths: &MiyuPaths) -> Result<String> {
        let path = paths.config_dir.join(DEV_PROMPT_FILE);
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let content = content.trim();
        if content == LEGACY_DEV_SYSTEM_PROMPT {
            return Ok(String::new());
        }
        Ok(content.to_string())
    }

    pub fn system_prompt(&self, paths: &MiyuPaths) -> Result<String> {
        self.system_prompt_for(paths, PromptAudience::Owner)
    }

    pub fn system_prompt_for(&self, paths: &MiyuPaths, audience: PromptAudience) -> Result<String> {
        self.system_prompt_with(paths, audience, audience.includes_user_identity())
    }

    /// `with_user_profile` 单独给:属主档案(profile.md)只在属主类入口注入——终端
    /// 与 WebUI(本机或远端,External 也算),通讯平台不注入。受众本身只管
    /// style-lock 那类差异,由调用方按「有没有平台上下文」决定档案进不进。
    pub fn system_prompt_with(
        &self,
        paths: &MiyuPaths,
        _audience: PromptAudience,
        with_user_profile: bool,
    ) -> Result<String> {
        let mut prompt = self.base_system_prompt(paths)?;
        if with_user_profile {
            let user_identity = self.user_identity_prompt(paths)?;
            if !user_identity.trim().is_empty() {
                prompt.push_str("\n\n<current-user-profile>\n");
                prompt.push_str(
                    "This profile describes the user currently interacting with you.\n\n",
                );
                prompt.push_str(user_identity.trim());
                prompt.push_str("\n</current-user-profile>");
            }
        }
        Ok(prompt)
    }

    pub fn base_system_prompt(&self, paths: &MiyuPaths) -> Result<String> {
        let persona = self.active_persona_prompt(paths)?;
        if persona.trim().is_empty() {
            Ok(default_system_prompt())
        } else {
            Ok(persona)
        }
    }

    pub fn custom_system_prompt(&self, paths: &MiyuPaths) -> Result<String> {
        if let Some(prompt) = self
            .system_prompt
            .as_deref()
            .filter(|prompt| !prompt.trim().is_empty())
        {
            return Ok(prompt.to_string());
        }
        let prompt_file = self.system_prompt_path(paths);
        if prompt_file.exists() {
            return Ok(std::fs::read_to_string(prompt_file)?);
        }
        Ok(String::new())
    }

    pub fn prompts_dir_path(&self, paths: &MiyuPaths) -> PathBuf {
        migrated_resource_path(paths, &self.prompt.prompts_dir)
            .unwrap_or_else(|| config_relative_path(paths, &self.prompt.prompts_dir))
    }

    pub fn user_identity_path(&self, paths: &MiyuPaths) -> PathBuf {
        if relative_path_equals(&self.prompt.user_identity_file, "user-identity.md") {
            fallback_resource_file(paths, "identities", "user-identity.md")
        } else if let Some(path) = migrated_fallback_file(
            paths,
            &self.prompt.user_identity_file,
            "identities",
            "user-identity.md",
        ) {
            path
        } else if let Some(path) = migrated_resource_path(paths, &self.prompt.user_identity_file) {
            path
        } else {
            config_relative_path(paths, &self.prompt.user_identity_file)
        }
    }

    pub fn identities_dir_path(&self, paths: &MiyuPaths) -> PathBuf {
        migrated_resource_path(paths, &self.prompt.identities_dir)
            .unwrap_or_else(|| config_relative_path(paths, &self.prompt.identities_dir))
    }

    pub fn persona_path(&self, paths: &MiyuPaths, name: &str) -> PathBuf {
        self.prompts_dir_path(paths).join(name)
    }

    // ── 成员私有人格(阶段 8):目录即人格 ──

    /// 回合里被指到成员私有人格时的目录。
    pub fn private_persona_dir(&self) -> Option<PathBuf> {
        self.prompt
            .private_persona_dir
            .as_deref()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
    }

    /// 私有人格的 scope 名:`home-<用户>-<slug>`,由目录最后两级算出;会话表、
    /// 记忆状态目录都用它,与共享人格的 scope 天然不撞(共享的没有 `home-` 前缀
    /// 也可能撞——用户自己起名 `home-xxx` 的概率忽略)。
    pub fn private_persona_scope(dir: &std::path::Path) -> Option<String> {
        let slug = dir.file_name()?.to_str()?;
        let username = dir.parent()?.parent()?.file_name()?.to_str()?;
        Some(persona_scope_name(&format!("home-{username}-{slug}")))
    }

    fn private_scope_matches(&self, persona: &str) -> bool {
        let Some(dir) = self.private_persona_dir() else {
            return false;
        };
        let persona = persona.trim();
        persona == self.prompt.active_persona.trim()
            || Self::private_persona_scope(&dir).as_deref() == Some(persona)
    }

    pub fn validate_persona_files(&self, paths: &MiyuPaths) -> Result<()> {
        if self
            .prompt
            .active_persona
            .trim()
            .eq_ignore_ascii_case("system-prompt.md")
        {
            bail!("system-prompt.md is reserved and cannot be used as a persona");
        }
        let directory = self.prompts_dir_path(paths);
        if !directory.exists() {
            return Ok(());
        }
        let mut scopes = HashMap::<String, String>::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") {
                continue;
            }
            if name.eq_ignore_ascii_case("system-prompt.md") {
                continue;
            }
            let scope = persona_scope_name(&name);
            if let Some(existing) = scopes.insert(scope.clone(), name.clone()) {
                bail!(
                    "persona names map to the same persistent scope: {existing} and {name} ({scope})"
                );
            }
        }
        Ok(())
    }

    pub fn identity_path(&self, paths: &MiyuPaths, name: &str) -> PathBuf {
        self.identities_dir_path(paths).join(name)
    }

    pub fn persona_memory_data_dir(&self, paths: &MiyuPaths, persona: &str) -> PathBuf {
        if self.private_scope_matches(persona) {
            if let Some(dir) = self.private_persona_dir() {
                return dir;
            }
        }
        paths.personas_dir().join(persona_scope_name(persona))
    }

    /// 纯中文人格名以前的 scope 是 `md`(只剩扩展名),09-13 起按名字哈希。
    /// 当前人格正好是这种名字、老目录还在、新目录还没有,就把老目录搬过去,
    /// 记忆与状态不丢。只搬当前人格:老 scope 只能容下一个人格,不会有第二个。
    pub(crate) fn migrate_degenerate_persona_scope(&self, paths: &MiyuPaths) {
        let name = self.prompt.active_persona.trim();
        if name.is_empty() || self.private_persona_dir().is_some() {
            return;
        }
        let scope = persona_scope_name(name);
        let ascii_only: String = name
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if !scope.starts_with("persona-") || ascii_only != "md" {
            return;
        }
        for (old, new) in [
            (
                paths.personas_dir().join("md"),
                paths.personas_dir().join(&scope),
            ),
            (
                paths.state_dir.join("personas").join("md"),
                paths.state_dir.join("personas").join(&scope),
            ),
        ] {
            if old.is_dir() && !new.exists() {
                if let Err(error) = std::fs::rename(&old, &new) {
                    tracing::warn!(error = %error, from = %old.display(), to = %new.display(), "persona scope migration failed");
                }
            }
        }
    }

    pub fn persona_memory_state_dir(&self, paths: &MiyuPaths, persona: &str) -> PathBuf {
        paths
            .state_dir
            .join("personas")
            .join(persona_scope_name(persona))
    }

    pub fn persona_skills_dir(&self, paths: &MiyuPaths, persona: &str) -> PathBuf {
        if self.private_scope_matches(persona) {
            if let Some(dir) = self.private_persona_dir() {
                return dir.join("skills");
            }
        }
        paths
            .skills_dir
            .join("personas")
            .join(persona_scope_name(persona))
    }

    pub fn persona_scripts_dir(&self, paths: &MiyuPaths, persona: &str) -> PathBuf {
        if self.private_scope_matches(persona) {
            if let Some(dir) = self.private_persona_dir() {
                return dir.join("scripts");
            }
        }
        paths
            .scripts_dir
            .join("personas")
            .join(persona_scope_name(persona))
    }

    pub fn active_persona_scripts_dir(&self, paths: &MiyuPaths) -> PathBuf {
        self.persona_scripts_dir(paths, self.prompt.active_persona.trim())
    }

    /// 内置(system)层的当前人格脚本目录。内置脚本装在
    /// `<system>/personas/<人格>/` 下,自定义人格的子目录不存在=天然拿不到
    /// 内置——人格门是**隐式**的,与 data 层的 personas/ 约定完全一致。
    pub fn active_persona_system_scripts_dir(&self, paths: &MiyuPaths) -> PathBuf {
        paths
            .system_scripts_dir
            .join("personas")
            .join(self.active_persona_scope())
    }

    /// 新布局(09-23):资源树里这一人格的根 `<资源根>/personas/<人格>`。
    /// 技能与脚本一个家——脚本住 `scripts/`,技能住 `skills/<技能名>/`。
    pub fn persona_system_dir(&self, paths: &MiyuPaths, persona: &str) -> Option<PathBuf> {
        Some(
            paths
                .system_personas_dir()?
                .join(persona_scope_name(persona)),
        )
    }

    /// 新布局:资源树候选里这一人格的全部根目录,优先级从高到低
    /// (安装前缀 > 系统前缀 > debug 源码树,见 [`MiyuPaths::system_personas_dirs`])。
    pub fn persona_system_dirs(&self, paths: &MiyuPaths, persona: &str) -> Vec<PathBuf> {
        let scope = persona_scope_name(persona);
        paths
            .system_personas_dirs()
            .into_iter()
            .map(|root| root.join(&scope))
            .collect()
    }

    /// 出厂人格(Miyu 本人 = scope `default`)在资源树里的根。
    pub fn default_persona_system_dir(&self, paths: &MiyuPaths) -> Option<PathBuf> {
        self.persona_system_dir(paths, "")
    }

    /// 新布局:当前人格在资源树(最高优先级候选)里的根。
    pub fn active_persona_system_dir(&self, paths: &MiyuPaths) -> Option<PathBuf> {
        self.persona_system_dir(paths, &self.active_persona_scope())
    }

    /// Sanitized scope name of the active persona; also the namespace key for
    /// sessions and per-persona state directories.
    pub fn active_persona_scope(&self) -> String {
        if let Some(dir) = self.private_persona_dir() {
            if let Some(scope) = Self::private_persona_scope(&dir) {
                return scope;
            }
        }
        persona_scope_name(self.prompt.active_persona.trim())
    }

    /// Dev 模式的作用域配置:人格指针换成保留人格 "dev",记忆/技能目录
    /// 随之落入独立命名空间。键是常量人格名而非提示词内容——编辑
    /// dev-prompt.md 只改提示词,永远不会切库丢记忆。
    pub fn dev_scoped(&self) -> AppConfig {
        let mut config = self.clone();
        config.prompt.active_persona = crate::config::DEV_PERSONA.to_string();
        // dev 不带记忆(09-09 用户裁定)。关的是整套:记忆工具不注册、联想
        // 不注入、自动日记不写、`<associative-memory>` 前言也随之退场。
        // 实测里 dev 会话被回灌过另一个 dev 会话的闲聊日记——编码回合既用
        // 不上它,又把闲聊语域带回上下文。
        //
        // 关在配置层而不是各处加 `mode != Dev`:`memory_config()` 是全链
        // 唯一判据,MemoryStore 的读写、联想、前言、工具注册都看它。
        // 连带:`miyu pop` 弹出的回合不再进逐出库,也就找不回来了。
        let uses_top_level = config.memory != MemoryConfig::default();
        let memory = if uses_top_level {
            &mut config.memory
        } else {
            &mut config.plugins.memory
        };
        memory.enabled = false;
        config
    }

    pub fn active_persona_memory_data_dir(&self, paths: &MiyuPaths) -> PathBuf {
        self.persona_memory_data_dir(paths, self.prompt.active_persona.trim())
    }

    pub fn active_persona_memory_state_dir(&self, paths: &MiyuPaths) -> PathBuf {
        self.persona_memory_state_dir(paths, self.prompt.active_persona.trim())
    }

    pub fn active_persona_skills_dir(&self, paths: &MiyuPaths) -> PathBuf {
        self.persona_skills_dir(paths, self.prompt.active_persona.trim())
    }

    pub fn active_persona_prompt(&self, paths: &MiyuPaths) -> Result<String> {
        if let Some(dir) = self.private_persona_dir() {
            let path = dir.join("persona.md");
            return std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()));
        }
        if !self.prompt.active_persona.trim().is_empty() {
            let path = self.persona_path(paths, self.prompt.active_persona.trim());
            if path.exists() {
                return std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()));
            }
        }
        if let Some(prompt) = self
            .system_prompt
            .as_deref()
            .filter(|prompt| !prompt.trim().is_empty())
        {
            return Ok(prompt.to_string());
        }
        let legacy = self.custom_system_prompt(paths)?;
        if legacy.trim().is_empty() {
            Ok(String::new())
        } else {
            Ok(legacy)
        }
    }

    pub fn user_identity_prompt(&self, paths: &MiyuPaths) -> Result<String> {
        if !self.prompt.active_identity.trim().is_empty() {
            let path = self.identity_path(paths, self.prompt.active_identity.trim());
            if path.exists() {
                return std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()));
            }
        }
        let path = self.user_identity_path(paths);
        if path.exists() {
            return std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()));
        }
        Ok(String::new())
    }

    pub fn system_prompt_path(&self, paths: &MiyuPaths) -> PathBuf {
        let value = self
            .system_prompt_file
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("system-prompt.md");
        if relative_path_equals(value, "system-prompt.md") {
            fallback_resource_file(paths, "prompts", "system-prompt.md")
        } else if let Some(path) =
            migrated_fallback_file(paths, value, "prompts", "system-prompt.md")
        {
            path
        } else if let Some(path) = migrated_resource_path(paths, value) {
            path
        } else {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                paths.config_dir.join(path)
            }
        }
    }
}
