//! 脚本索引的扫描与落盘。
//!
//! 真相源顺序(09-05):`index.json` 条目里显式写了的字段 > 脚本头部
//! (header.rs)> 默认值。index 退为覆盖层——描述/参数/超时/分组都能写在脚本
//! 开头的注释里,一个文件就是一个完整的工具;index 只放手写覆盖和 disabled
//! 名单。内置 8 条 index 条目原样保留,它们仍然压在头部之上,行为零变化。
//!
//! 脚本 ID 会变成工具名，所以 `is_valid_registered_script_id` 与
//! `is_reserved_script_id` 挡的是「注册出一个和内建工具重名的工具」。
//!
//! `ensure_path_within_root` 是路径边界：索引里的路径可能被手工编辑过，指到库
//! 外就等于任意文件执行。

use crate::tools::scripts::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ScriptIndex {
    #[serde(default)]
    pub(crate) scripts: Vec<ScriptEntry>,
    #[serde(default)]
    pub(crate) disabled: Vec<DisabledScript>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct DisabledScript {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) path: String,
}

/// 脚本的来源层(09-23)。层级由「从哪个扫描根扫到」决定,不由路径前缀反推:
/// 新布局把内置件从 `<前缀>/scripts/personas/<人格>/` 搬到
/// `<前缀>/personas/<人格>/`,再拿前缀猜谁是内置就会漏掉整层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ScriptLayerKind {
    /// 用户自己的扩展层:`<data>/scripts/` 顶层。
    #[default]
    Global,
    /// 用户自己的扩展层:`<data>/scripts/personas/<人格>/`。
    Persona,
    /// 内置层顶层(老布局 `<系统前缀>/scripts/`,当前为空)。
    Builtin,
    /// 内置层的人格目录:老布局 `<系统前缀>/scripts/personas/<人格>/`,
    /// 新布局 `<资源根>/personas/<人格>/scripts/`。
    BuiltinPersona,
    /// 内置层的技能带路脚本:
    /// `<资源根>/personas/<人格>/skills/<技能名>/scripts/`。
    BuiltinSkill,
}

impl ScriptLayerKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Persona => "persona",
            Self::Builtin => "builtin",
            Self::BuiltinPersona => "builtin-persona",
            Self::BuiltinSkill => "builtin-skill",
        }
    }

    pub(crate) fn is_builtin(self) -> bool {
        matches!(
            self,
            Self::Builtin | Self::BuiltinPersona | Self::BuiltinSkill
        )
    }
}

/// 一条脚本的来源:层 + 带路技能。扫描时标出,不进 index.json。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ScriptOrigin {
    pub(crate) layer: ScriptLayerKind,
    /// 技能带路:`skills/<技能名>/scripts/` 下的脚本记下技能名,第二批据此
    /// 「不单独列行 + 开关连动」。
    pub(crate) skill: Option<String>,
}

impl ScriptOrigin {
    pub(crate) fn is_builtin(&self) -> bool {
        self.layer.is_builtin()
    }

    /// 技能带路的脚本(`skills/<技能名>/scripts/`):不进模型的常驻 tools 数组,
    /// 由技能正文带路。09-23 起这是唯一的判据——不再看脚本头部声明。
    pub(crate) fn is_skill_carried(&self) -> bool {
        self.layer == ScriptLayerKind::BuiltinSkill
    }

    pub(crate) fn label(&self) -> &'static str {
        self.layer.as_str()
    }
}

/// 一个扫描根:目录 + 它代表的来源层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScriptScanRoot {
    pub(crate) path: PathBuf,
    pub(crate) origin: ScriptOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScriptEntry {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) description: String,
    /// 界面上的说明（人槽）。`description` 是**模型**读的那一份，恒英文
    /// （`select_script_description`）；拿它当界面文案会让设置页里中文名配
    /// 英文说明（AGENTS §1.5.1）。脚本头的 `# 描述：` 落在这儿，缺省回退到
    /// `description`。不进模型面，不占 token。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) ui_description: String,
    #[serde(default)]
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) parameters: Value,
    #[serde(default)]
    pub(crate) timeout_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) always_loaded: Option<bool>,
    #[serde(default)]
    pub(crate) load_policy: LoadPolicy,
    #[serde(default)]
    pub(crate) groups: Vec<String>,
    #[serde(default, skip_serializing_if = "ArgvMode::is_off")]
    pub(crate) argv: ArgvMode,
    /// 场所信任位;缺省 Owner。见 ToolSpec::trust。
    #[serde(default, skip_serializing_if = "is_owner_trust")]
    pub(crate) trust: ToolTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) permission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stub_example: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) hints: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) requires: Vec<String>,
    /// 头部 `Capabilities:`,见 `ScriptMetadata::capabilities`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) capabilities: Vec<String>,
    /// 扫描时标出的来源层与带路技能(09-23)。不进 `index.json`(`serde(skip)`):
    /// 它是扫描的结论,不是用户可编辑的字段,每次扫描按根重算。
    #[serde(skip)]
    pub(crate) origin: ScriptOrigin,
}

fn is_owner_trust(trust: &ToolTrust) -> bool {
    *trust == ToolTrust::Owner
}

impl ScriptEntry {
    /// 只有 id 与路径的空条目:其余字段留空,扫描时由脚本头部补齐。
    pub(crate) fn overlay(id: String, path: String) -> Self {
        Self {
            id,
            display_name: String::new(),
            description: String::new(),
            ui_description: String::new(),
            path,
            parameters: Value::Null,
            timeout_seconds: None,
            always_loaded: None,
            load_policy: LoadPolicy::Summary,
            groups: Vec::new(),
            argv: ArgvMode::Off,
            trust: ToolTrust::Owner,
            permission: None,
            stub_example: None,
            hints: Vec::new(),
            requires: Vec::new(),
            capabilities: Vec::new(),
            origin: ScriptOrigin::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ScriptScanResult {
    pub(crate) entries: Vec<ScriptEntry>,
    pub(crate) unregistered: Vec<UnregisteredScript>,
}

/// 扫描根连同来源层,覆盖链低→高(`scan_scripts_at` 后者同名覆盖前者):
///
/// ```text
/// 老布局(09-23 前)                新布局(09-23 起)
/// <system>/                        ← 平台级内置(当前为空)
/// <system>/personas/default        <资源根>/personas/default/scripts
/// <system>/personas/<人格>          <资源根>/personas/<人格>/scripts
///                                  <资源根>/personas/<人格>/skills/<技能>/scripts
/// <data>/scripts                   (用户自己的扩展层,不变)
/// <data>/scripts/personas/<人格>
/// ```
///
/// 内置脚本装在 `personas/` 下:自定义人格在资源树里没有自己的目录=天然拿不到,
/// 人格门是**隐式**的(09-01)。但出厂人格 default 那一层照扫(老布局与新布局
/// 都是),好让自定义人格按 `plugins.scripts` 白名单逐个勾回来(09-13)。老布局
/// 保持可扫——升级后老用户的脚本不能凭空消失。
pub(crate) fn script_scan_root_layers(
    config: &miyu_base::config::AppConfig,
    paths: &MiyuPaths,
) -> Vec<ScriptScanRoot> {
    let mut roots = Vec::new();
    // 老布局。
    let builtin = builtin_scripts_dir(paths);
    let persona_system = config.active_persona_system_scripts_dir(paths);
    push_scan_root(
        &mut roots,
        paths.system_scripts_dir.clone(),
        ScriptLayerKind::Builtin,
        None,
    );
    push_scan_root(
        &mut roots,
        builtin.clone(),
        ScriptLayerKind::BuiltinPersona,
        None,
    );
    if persona_system != builtin {
        push_scan_root(
            &mut roots,
            persona_system,
            ScriptLayerKind::BuiltinPersona,
            None,
        );
    }
    // 新布局:候选链倒序入列,让优先级最高的候选最后覆盖(与 resources 的
    // 「头一个候选优先」同向)。人格内部先出厂后当前,当前人格的件压过出厂件。
    let factory = miyu_base::config::persona_scope_name("");
    for root in paths.system_personas_dirs().into_iter().rev() {
        let mut personas = vec![root.join(&factory)];
        let active = root.join(config.active_persona_scope());
        if !personas.contains(&active) {
            personas.push(active);
        }
        for persona in personas {
            push_scan_root(
                &mut roots,
                persona.join("scripts"),
                ScriptLayerKind::BuiltinPersona,
                None,
            );
            for (scripts, skill) in skill_script_roots(&persona.join("skills")) {
                push_scan_root(
                    &mut roots,
                    scripts,
                    ScriptLayerKind::BuiltinSkill,
                    Some(skill),
                );
            }
        }
    }
    // 用户自己的扩展层(最高优先级)。
    push_scan_root(
        &mut roots,
        paths.scripts_dir.clone(),
        ScriptLayerKind::Global,
        None,
    );
    push_scan_root(
        &mut roots,
        config.active_persona_scripts_dir(paths),
        ScriptLayerKind::Persona,
        None,
    );
    roots
}

fn push_scan_root(
    roots: &mut Vec<ScriptScanRoot>,
    path: PathBuf,
    layer: ScriptLayerKind,
    skill: Option<String>,
) {
    roots.push(ScriptScanRoot {
        path,
        origin: ScriptOrigin { layer, skill },
    });
}

/// `<personas>/<人格>/skills/*/scripts`:技能带路的脚本目录,连同技能名。
/// 只认真的有 `scripts/` 子目录的技能;目录名排序保证指纹与覆盖顺序确定。
fn skill_script_roots(skills_dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(read_dir) = std::fs::read_dir(skills_dir) else {
        return Vec::new();
    };
    let mut skills: Vec<PathBuf> = read_dir
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("scripts").is_dir())
        .collect();
    skills.sort();
    skills
        .into_iter()
        .filter_map(|skill| {
            let name = skill.file_name()?.to_str()?.to_string();
            Some((skill.join("scripts"), name))
        })
        .collect()
}

/// 内置脚本的目录:`<system>/personas/default/`。
pub fn builtin_scripts_dir(paths: &MiyuPaths) -> PathBuf {
    paths.system_scripts_dir.join("personas").join("default")
}

/// 这条脚本是不是内置层的(装在 `<system>/` 下)。
pub(crate) fn is_builtin_script(paths: &MiyuPaths, entry: &ScriptEntry) -> bool {
    if entry.origin.is_builtin() {
        return true;
    }
    // 兼容:直接构造 ScriptEntry 的调用方(与老布局)仍按路径前缀判。
    // 空 system_scripts_dir 下 `Path::starts_with("")` 恒真,得挡掉。
    let path = Path::new(&entry.path);
    if !paths.system_scripts_dir.as_os_str().is_empty()
        && path.starts_with(&paths.system_scripts_dir)
    {
        return true;
    }
    paths
        .system_personas_dir()
        .is_some_and(|root| path.starts_with(root))
}

/// 功能表要摆的脚本:内置各层(老布局出厂目录)+ 用户全局层,再加上出厂人格
/// 技能树里带路的脚本。技能带路的那些在功能表上不单独成行(开关是它那份技能
/// 那一行),但要把归属技能带回去,好让技能开关连动脚本白名单。
pub fn list_scripts_for_features(
    paths: &MiyuPaths,
) -> Vec<(String, String, String, bool, Option<String>)> {
    let mut rows = Vec::new();
    let base_dirs = [builtin_scripts_dir(paths), paths.scripts_dir.clone()];
    let base_refs: Vec<&Path> = base_dirs.iter().map(PathBuf::as_path).collect();
    for (id, name, hint, builtin) in list_scripts_with_origin(&base_refs, Some(paths)) {
        rows.push((id, name, hint, builtin, None));
    }
    if let Some(root) = paths.system_personas_dir() {
        let skills_dir = root
            .join(miyu_base::config::persona_scope_name(""))
            .join("skills");
        for (scripts_dir, skill) in skill_script_roots(&skills_dir) {
            let Ok(scan) = scan_scripts(&[scripts_dir.as_path()]) else {
                continue;
            };
            for entry in scan.entries {
                let display = entry_display_name(&entry);
                let hint = if entry.ui_description.trim().is_empty() {
                    entry.description
                } else {
                    entry.ui_description
                };
                rows.push((entry.id, display, hint, true, Some(skill.clone())));
            }
        }
    }
    rows
}

pub(crate) fn script_specs(
    entries: &[ScriptEntry],
    scripts_dir: &Path,
    cache_dir: &Path,
) -> Vec<ToolSpec> {
    entries
        .iter()
        .filter_map(|entry| entry_to_spec(entry, scripts_dir, cache_dir).ok())
        .collect()
}

/// index 条目没写的字段从脚本头部补:显示名、描述、参数 schema、超时、分组、
/// argv 模式。index 写了的一律不动——它是覆盖层。
pub(crate) fn merge_header_defaults(entry: &mut ScriptEntry, metadata: &ScriptMetadata) {
    if entry.display_name.trim().is_empty() {
        if let Some(display_name) = select_script_display_name(&metadata.display_names) {
            entry.display_name = display_name;
        }
    }
    if entry.description.trim().is_empty() {
        if let Some(description) = select_script_description(&metadata.descriptions) {
            entry.description = description;
        }
    }
    if entry.ui_description.trim().is_empty() {
        if let Some(description) = select_script_ui_description(&metadata.descriptions) {
            entry.ui_description = description;
        }
    }
    if entry.parameters.is_null() {
        if let Some(parameters) = &metadata.parameters {
            entry.parameters = parameters.clone();
        }
    }
    if entry.timeout_seconds.is_none() {
        entry.timeout_seconds = metadata.timeout_seconds;
    }
    // 头部给了分组就顺带走 group 目录;index 里自己写的 groups+load_policy
    // 组合原样保留(用户现有条目有 groups 配 summary 的,不替它改语义)。
    if entry.groups.is_empty() && !metadata.groups.is_empty() {
        entry.groups = metadata.groups.clone();
        if matches!(entry.load_policy, LoadPolicy::Summary) {
            entry.load_policy = LoadPolicy::Group;
        }
    }
    if entry.argv.is_off() {
        if let Some(argv) = metadata.argv {
            entry.argv = argv;
        }
    }
    if entry.trust == ToolTrust::Owner {
        if let Some(trust) = metadata.trust {
            entry.trust = trust;
        }
    }
    if entry.permission.is_none() {
        entry.permission = metadata.permission.map(|permission| {
            match permission {
                ToolPermission::ReadOnly => "read-only",
                ToolPermission::Presentation => "presentation",
                ToolPermission::Writes => "writes",
            }
            .to_string()
        });
    }
    if entry.stub_example.is_none() {
        entry.stub_example = metadata.stub_example.clone();
    }
    if entry.hints.is_empty() {
        entry.hints = metadata.hints.clone();
    }
    if entry.requires.is_empty() {
        entry.requires = metadata.requires.clone();
    }
    if entry.capabilities.is_empty() {
        entry.capabilities = metadata.capabilities.clone();
    }
}

/// 只给路径的扫描(测试、以及只看某一层的调用方):来源层按用户层算。
pub(crate) fn scan_scripts(dirs: &[&Path]) -> Result<ScriptScanResult> {
    let roots: Vec<ScriptScanRoot> = dirs
        .iter()
        .map(|dir| ScriptScanRoot {
            path: dir.to_path_buf(),
            origin: ScriptOrigin::default(),
        })
        .collect();
    scan_scripts_at(&roots)
}

/// 按扫描根(带来源层)扫,`scan_scripts` 是它的无层包装。
pub(crate) fn scan_scripts_at(roots: &[ScriptScanRoot]) -> Result<ScriptScanResult> {
    let mut entries = BTreeMap::<String, ScriptEntry>::new();
    let mut unregistered = BTreeMap::<String, UnregisteredScript>::new();
    let mut seen_paths = BTreeSet::new();

    for root in roots {
        let scripts_dir = root.path.as_path();
        if !scripts_dir.is_dir() {
            continue;
        }

        let index_path = scripts_dir.join("index.json");
        let index = read_script_index_for_scan(&index_path)?;

        let mut disabled_ids = BTreeSet::new();
        let mut disabled_paths = BTreeSet::new();
        // 本层 index 已登记的 id:同目录里同名 stem 的其它文件(gpustoggle.bak
        // 之类)不得再以自动检测的身份把它顶掉或拖进未注册清单——用户机器上
        // 一个没有描述头的 .bak 就把正主从工具面上抹掉了(09-05 实查)。
        let mut indexed_ids = BTreeSet::new();
        for disabled in &index.disabled {
            if !disabled.id.trim().is_empty() {
                disabled_ids.insert(disabled.id.clone());
                entries.remove(&disabled.id);
                unregistered.remove(&disabled.id);
            }
            if !disabled.path.trim().is_empty() {
                disabled_paths.insert(canonicalize_key(&resolve_script_path(
                    &disabled.path,
                    scripts_dir,
                )));
            }
        }

        for indexed_entry in index.scripts {
            if !is_valid_registered_script_id(&indexed_entry.id)
                || disabled_ids.contains(&indexed_entry.id)
                || is_reserved_script_id(&indexed_entry.id)
            {
                continue;
            }
            let unresolved_path = resolve_script_path(&indexed_entry.path, scripts_dir);
            if !unresolved_path.is_file() {
                continue;
            }
            let path = match ensure_path_within_root(&unresolved_path, scripts_dir) {
                Ok(path) => path,
                Err(_) => continue,
            };
            let canon = canonicalize_key(&path);
            if disabled_paths.contains(&canon) {
                continue;
            }
            seen_paths.insert(canon);

            let mut entry = indexed_entry;
            indexed_ids.insert(entry.id.clone());
            entry.path = path.to_string_lossy().to_string();
            entry.origin = root.origin.clone();
            merge_header_defaults(&mut entry, &metadata_from_script(&path));
            if entry.description.trim().is_empty() {
                entries.remove(&entry.id);
                unregistered.insert(
                    entry.id.clone(),
                    UnregisteredScript {
                        name: entry.id,
                        path: path.to_string_lossy().to_string(),
                        skill: root.origin.skill.clone(),
                    },
                );
            } else {
                unregistered.remove(&entry.id);
                entries.insert(entry.id.clone(), entry);
            }
        }

        for file_entry in std::fs::read_dir(scripts_dir)? {
            let file_entry = file_entry?;
            let path = file_entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = file_entry.file_name().to_string_lossy().to_string();
            if fname == "index.json" || fname.starts_with('.') || is_backup_file_name(&fname) {
                continue;
            }
            let Some(detected) = inspect_script(&path) else {
                continue;
            };
            if detected
                .id
                .as_deref()
                .is_some_and(|id| indexed_ids.contains(id))
            {
                continue;
            }
            let canon = canonicalize_key(&path);
            let path_string = path.to_string_lossy().to_string();
            // 文件名折不出合法工具名(纯中文文件名):列进未注册清单,模型能
            // 看见它、用 manage_script 给个 id 注册。
            let Some(id) = detected.id.clone() else {
                if disabled_paths.contains(&canon) || !seen_paths.insert(canon) {
                    continue;
                }
                unregistered.insert(
                    detected.stem.clone(),
                    UnregisteredScript {
                        name: detected.stem,
                        path: path_string,
                        skill: root.origin.skill.clone(),
                    },
                );
                continue;
            };
            if is_reserved_script_id(&id) {
                continue;
            }
            if disabled_ids.contains(&id)
                || disabled_paths.contains(&canon)
                || !seen_paths.insert(canon)
            {
                continue;
            }

            let mut entry = entry_from_detected(&detected, id.clone(), path_string.clone());
            entry.origin = root.origin.clone();
            if entry.description.trim().is_empty() {
                entries.remove(&id);
                unregistered.insert(
                    id.clone(),
                    UnregisteredScript {
                        name: id,
                        path: path_string,
                        skill: root.origin.skill.clone(),
                    },
                );
            } else {
                unregistered.remove(&id);
                entries.insert(id, entry);
            }
        }
    }

    Ok(ScriptScanResult {
        entries: entries.into_values().collect(),
        unregistered: unregistered.into_values().collect(),
    })
}

/// 编辑器/手工备份副本不算脚本:`foo.bak` 的 stem 仍是 `foo`,会撞正主的 id。
pub(crate) fn is_backup_file_name(name: &str) -> bool {
    name.ends_with('~')
        || [".bak", ".orig", ".tmp", ".swp", ".old", ".rej"]
            .iter()
            .any(|suffix| name.to_ascii_lowercase().ends_with(suffix))
}

pub(crate) fn canonicalize_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn resolve_script_path(path_str: &str, scripts_dir: &Path) -> PathBuf {
    let p = Path::new(path_str);
    if p.is_absolute() {
        if p.starts_with(scripts_dir) {
            return p.to_path_buf();
        }
        if let Some(root) = scripts_dir
            .parent()
            .filter(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("data"))
            .and_then(Path::parent)
        {
            let legacy = root.join("config/scripts");
            if let Ok(relative) = p.strip_prefix(&legacy) {
                return scripts_dir.join(relative);
            }
        }
        if let Some(base) = directories::BaseDirs::new() {
            let legacy = base.config_dir().join("miyu/scripts");
            if let Ok(relative) = p.strip_prefix(&legacy) {
                return scripts_dir.join(relative);
            }
        }
        p.to_path_buf()
    } else {
        scripts_dir.join(p)
    }
}

pub(crate) fn ensure_path_within_root(path: &Path, scripts_dir: &Path) -> Result<PathBuf> {
    let root = scripts_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve scripts directory {}",
            scripts_dir.display()
        )
    })?;
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to resolve script path {}", path.display()))?;
    if !path.starts_with(&root) {
        bail!(
            "script path must stay within the scripts directory: {}",
            path.display()
        );
    }
    Ok(path)
}

pub(crate) fn relative_script_path(path: &Path, scripts_dir: &Path) -> String {
    let root = scripts_dir
        .canonicalize()
        .unwrap_or_else(|_| scripts_dir.to_path_buf());
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string()
}

pub(crate) fn is_reserved_script_id(id: &str) -> bool {
    id == "load_tools" || crate::tools::tool_descriptions::get(id).is_some()
}

pub(crate) fn is_valid_registered_script_id(id: &str) -> bool {
    id.chars()
        .next()
        .map(|character| character.is_ascii_alphabetic())
        .unwrap_or(false)
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[derive(Debug, Clone)]
pub(crate) struct DetectedScript {
    /// 工具名:头部 `Id:`(合法时)> 文件名 stem 归一化;折不出来为 None。
    pub(crate) id: Option<String>,
    pub(crate) stem: String,
    pub(crate) display_name: String,
    pub(crate) metadata: ScriptMetadata,
}

pub(crate) fn inspect_script(path: &Path) -> Option<DetectedScript> {
    let raw = read_header(path)?;
    if !raw.starts_with("#!") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?.to_string();
    let metadata = extract_metadata(&raw);
    let id = metadata
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| is_valid_registered_script_id(id))
        .map(str::to_string)
        .or_else(|| normalize_script_id(&stem));
    let display_name = select_script_display_name(&metadata.display_names)
        .unwrap_or_else(|| humanize_script_id(id.as_deref().unwrap_or(&stem)));
    Some(DetectedScript {
        id,
        stem,
        display_name,
        metadata,
    })
}

pub(crate) fn entry_from_detected(
    detected: &DetectedScript,
    id: String,
    path: String,
) -> ScriptEntry {
    let mut entry = ScriptEntry::overlay(id, path);
    entry.display_name = detected.display_name.clone();
    merge_header_defaults(&mut entry, &detected.metadata);
    entry
}

/// 展示用的名字:头部或 index 给了就用,没给按 id 兜一个(英文界面下只写了
/// 中文名的脚本走的就是这条——[`select_script_display_name`] 不回退中文)。
/// 落盘的 `ScriptEntry.display_name` 保持原样为空:兜底只发生在展示边界,
/// 免得 register 把某次运行时 locale 挑出来的名字固化进 index.json。
pub(crate) fn entry_display_name(entry: &ScriptEntry) -> String {
    if entry.display_name.trim().is_empty() {
        humanize_script_id(&entry.id)
    } else {
        entry.display_name.clone()
    }
}

pub(crate) fn entry_to_spec(
    entry: &ScriptEntry,
    scripts_dir: &Path,
    cache_dir: &Path,
) -> Result<ToolSpec> {
    let id = entry.id.clone();
    if id.is_empty() {
        bail!("script id is empty");
    }
    let display_name = entry_display_name(entry);
    if entry.description.trim().is_empty() {
        bail!("registered script is missing a description: {id}");
    }
    let description = entry.description.clone();
    // 默认懒加载(09-05)。此前「没写参数就常驻」:自动检测出来的脚本全都带着
    // 泛化 schema 永久占 tools 数组;stub 模式下常驻工具发的还是完整定义
    // (registry::stub_definitions),白花字节。index 里显式 always_loaded:true
    // 仍然放行。
    let always_loaded = entry.always_loaded.unwrap_or(false);
    let load_policy = entry.load_policy;
    let parameters = if entry.parameters.is_null() {
        json!({
            "type": "object",
            "properties": {
                "stdin": {
                    "type": "string",
                    "description": "Optional raw stdin input. If omitted, all arguments are sent as JSON via stdin."
                }
            },
            "additionalProperties": true
        })
    } else {
        entry.parameters.clone()
    };
    let timeout = entry
        .timeout_seconds
        .unwrap_or(SCRIPT_TIMEOUT_SECS)
        .min(300);
    let argv = entry.argv;
    let path_str = entry.path.clone();
    let scripts_dir = scripts_dir.to_path_buf();
    let cache_dir = cache_dir.to_path_buf();
    let host_capabilities = host_capabilities_for(entry);

    // 缺省 writes:脚本会跑命令。头部/index 明确写了 read-only 的才降。
    let permission = entry
        .permission
        .as_deref()
        .and_then(ToolPermission::parse)
        .unwrap_or(ToolPermission::Writes);
    let mut spec =
        ToolSpec::new_with_progress(id, description, parameters, move |args, progress| {
            let path_str = path_str.clone();
            let scripts_dir = scripts_dir.clone();
            let cache_dir = cache_dir.clone();
            let host_capabilities = host_capabilities.clone();
            async move {
                run_script(
                    &path_str,
                    &scripts_dir,
                    &cache_dir,
                    &args,
                    timeout,
                    argv,
                    &host_capabilities,
                    &progress,
                )
                .await
            }
        })
        .with_permission(permission)
        .with_display_name(display_name)
        .with_always_loaded(always_loaded)
        .with_load_policy(load_policy)
        .with_groups(entry.groups.clone())
        .with_trust(entry.trust)
        .with_cross_hints(entry.hints.clone())
        .with_requires_prior(entry.requires.clone())
        // 09-23:进不进常驻工具面只看脚本住在哪——`skills/<技能名>/scripts/`
        // 里的由技能带路(不进数组,可经工具桥调用),其余照旧进面。
        .with_exposed(!entry.origin.is_skill_carried())
        .script();
    if let Some(example) = entry
        .stub_example
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        spec = spec.with_stub_example(example);
    }
    Ok(spec)
}

/// 脚本运行时能向宿主要的能力:只认 `host_ports::HOST_CAPABILITIES` 里的 id,且只给
/// `Trust: owner`(缺省)的脚本——`Trust: external` 的脚本也会在不可信场所跑,
/// 宿主信息不能经它流出去。不认识的 id 记 warn 并丢弃,脚本照常注册。
pub(super) fn host_capabilities_for(entry: &ScriptEntry) -> Vec<String> {
    if entry.capabilities.is_empty() {
        return Vec::new();
    }
    if entry.trust != ToolTrust::Owner {
        tracing::warn!(
            script = %entry.id,
            "script declares Capabilities but is Trust: external; host access is not granted"
        );
        return Vec::new();
    }
    entry
        .capabilities
        .iter()
        .filter(|id| {
            let known = miyu_base::host_ports::is_known_capability(id);
            if !known {
                tracing::warn!(script = %entry.id, capability = %id, "unknown host capability ignored");
            }
            known
        })
        .cloned()
        .collect()
}

pub(crate) fn read_script_index_value(index_path: &Path) -> Result<Value> {
    if !index_path.is_file() {
        return Ok(json!({"scripts": [], "disabled": []}));
    }
    let raw = std::fs::read_to_string(index_path)?;
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", index_path.display()))?;
    if !value.is_object() {
        bail!(
            "script index root must be an object: {}",
            index_path.display()
        );
    }
    Ok(value)
}

pub(crate) fn read_script_index_for_scan(index_path: &Path) -> Result<ScriptIndex> {
    if !index_path.is_file() {
        return Ok(ScriptIndex::default());
    }
    let raw = std::fs::read_to_string(index_path)?;
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", index_path.display()))?;
    let scripts = value
        .get("scripts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .collect();
    let disabled = value
        .get("disabled")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .collect();
    Ok(ScriptIndex { scripts, disabled })
}

pub(crate) fn index_array_mut<'a>(index: &'a mut Value, key: &str) -> Result<&'a mut Vec<Value>> {
    let object = index
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("script index root must be an object"))?;
    let value = object.entry(key.to_string()).or_insert_with(|| json!([]));
    if !value.is_array() {
        *value = json!([]);
    }
    Ok(value.as_array_mut().expect("array was just initialized"))
}

pub(crate) fn raw_entry_field<'a>(entry: &'a Value, field: &str) -> Option<&'a str> {
    entry.get(field).and_then(Value::as_str)
}

pub(crate) fn write_script_index_value(index_path: &Path, index: &Value) -> Result<()> {
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file_name = index_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("index.json");
    let temp_path = index_path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&temp_path, serde_json::to_string_pretty(index)?)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    if let Err(error) = std::fs::rename(&temp_path, index_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("failed to replace {}", index_path.display()));
    }
    Ok(())
}

pub(crate) fn find_auto_detected_path(scripts_dir: &Path, id: &str) -> Result<Option<String>> {
    if !scripts_dir.is_dir() {
        return Ok(None);
    }
    for file_entry in std::fs::read_dir(scripts_dir)? {
        let file_entry = file_entry?;
        let path = file_entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(detected) = inspect_script(&path) else {
            continue;
        };
        if detected.id.as_deref() == Some(id) {
            return Ok(Some(relative_script_path(&path, scripts_dir)));
        }
    }
    Ok(None)
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
