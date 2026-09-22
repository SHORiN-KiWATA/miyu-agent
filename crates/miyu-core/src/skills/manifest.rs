//! 技能文档的解析与校验。
//!
//! 技能是带 YAML frontmatter 的 Markdown。YAML 解析限了 token 数
//! （`MAX_YAML_TOKENS`）：frontmatter 来自用户或模型，一个精心构造的锚点展开能
//! 把内存吃光。
//!
//! `validate_skill_name` 是路径边界——名字会变成目录名。

use crate::skills::*;

pub(crate) const MAX_SKILL_FILE_BYTES: u64 = 256 * 1024;

pub(crate) const MAX_YAML_TOKENS: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    /// 界面上的名字。`description` 是**模型**读的触发词（进 load_skill 的
    /// 常驻目录、按英文短句写），拿它当界面文案会让设置页中英混杂——名字与
    /// 说明各自双槽，模型槽恒英文、人槽跟界面语言（AGENTS §1.5.1）。
    /// 缺省回退到 `name`。这两个字段不进模型面，不占 token。
    pub display_name: Option<String>,
    /// 界面上的一句话说明；缺省回退到 `description`。
    pub summary: Option<String>,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub allowed_tools: Option<String>,
}

/// `travel-planner` → `Travel planner`。规则与脚本那边的 `humanize_script_id`
/// 一致：连字符/下划线换空格，只把第一个词首字母大写（技能 id 里常有
/// `bilibili`、`linux` 这种专名，全词大写反而怪）。
fn humanize_skill_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut capitalize = true;
    for ch in id.chars() {
        if ch == '-' || ch == '_' {
            if !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        if capitalize {
            out.extend(ch.to_uppercase());
            capitalize = false;
        } else {
            out.push(ch);
        }
    }
    if out.trim().is_empty() {
        id.to_string()
    } else {
        out
    }
}

impl SkillMetadata {
    /// 界面上的名字:人槽优先,没写就回退到技能 id。
    ///
    /// **英文界面直接走模型槽**——那本来就是英文触发词。人槽是中文,英文界面
    /// 照搬会让设置页中英混杂：隔壁脚本那一栏按 locale 变英文，技能这栏却是
    /// 中文（用户 09-23 截图）。不用给技能格式加英文槽,现成的模型槽就够。
    pub fn ui_name(&self) -> String {
        if miyu_base::i18n::locale() == miyu_base::i18n::Locale::En {
            // 模型槽就是技能 id（`travel-planner`），直接摆进设置页是个裸 id。
            // 按脚本那边同一套规则拼成人话（`humanize_script_id`）：连字符换空格、
            // 首字母大写。
            return humanize_skill_id(&self.name);
        }
        self.display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.name)
            .to_string()
    }

    /// 界面上的一句话说明:人槽优先,没写就回退到模型槽。
    pub fn ui_summary(&self) -> &str {
        self.summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.description)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillSource {
    Persona,
    Global,
    BuiltIn,
}

impl SkillSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Global => "global",
            Self::BuiltIn => "built_in",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SkillEntry {
    pub metadata: SkillMetadata,
    pub source: SkillSource,
    pub directory: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct LoadedSkill {
    pub metadata: SkillMetadata,
    pub body: String,
    pub source: SkillSource,
    pub base_dir: Option<PathBuf>,
    pub files: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillScope {
    Global,
    Persona,
}

impl SkillScope {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("global").trim() {
            "" | "global" => Ok(Self::Global),
            "persona" => Ok(Self::Persona),
            other => bail!("invalid skill scope: {other}; expected global or persona"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Persona => "persona",
        }
    }
}

pub fn parse_skill_metadata(raw: &str, expected_name: Option<&str>) -> Result<SkillMetadata> {
    parse_skill_document(raw, expected_name).map(|(metadata, _)| metadata)
}

pub(crate) fn parse_skill_document(
    raw: &str,
    expected_name: Option<&str>,
) -> Result<(SkillMetadata, String)> {
    let (frontmatter, body) = split_frontmatter(raw)?;
    validate_frontmatter_tokens(&frontmatter)?;
    let documents =
        YamlLoader::load_from_str(&frontmatter).context("parsing skill YAML frontmatter")?;
    if documents.len() != 1 {
        bail!("skill frontmatter must contain exactly one YAML document");
    }
    let mapping = documents[0]
        .as_hash()
        .context("skill frontmatter root must be a mapping")?;
    let name = required_yaml_string(mapping, "name")?;
    validate_skill_name(&name)?;
    if let Some(expected) = expected_name {
        if name != expected {
            bail!("skill name '{name}' does not match directory '{expected}'");
        }
    }
    let description = required_yaml_string(mapping, "description")?;
    validate_description(&description)?;
    let license = optional_yaml_string(mapping, "license")?;
    let compatibility = optional_yaml_string(mapping, "compatibility")?;
    if compatibility
        .as_ref()
        .is_some_and(|value| !(1..=500).contains(&value.chars().count()))
    {
        bail!("skill compatibility must be 1-500 characters");
    }
    let display_name = optional_yaml_string(mapping, "display_name")?;
    let summary = optional_yaml_string(mapping, "summary")?;
    for (field, value) in [("display_name", &display_name), ("summary", &summary)] {
        if value
            .as_ref()
            .is_some_and(|value| !(1..=200).contains(&value.chars().count()))
        {
            bail!("skill {field} must be 1-200 characters");
        }
    }
    let allowed_tools = optional_yaml_string(mapping, "allowed-tools")?;
    let metadata = yaml_string_map(mapping, "metadata")?;
    Ok((
        SkillMetadata {
            name,
            description,
            display_name,
            summary,
            license,
            compatibility,
            metadata,
            allowed_tools,
        },
        body,
    ))
}

pub(crate) fn validate_frontmatter_tokens(frontmatter: &str) -> Result<()> {
    for (index, Token(_, token)) in Scanner::new(frontmatter.chars()).enumerate() {
        if index >= MAX_YAML_TOKENS {
            bail!("skill frontmatter exceeds the YAML token limit");
        }
        if matches!(token, TokenType::Alias(_) | TokenType::Anchor(_)) {
            bail!("skill frontmatter may not use YAML anchors or aliases");
        }
    }
    Ok(())
}

pub(crate) fn split_frontmatter(raw: &str) -> Result<(String, String)> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines();
    if lines.next() != Some("---") {
        bail!("SKILL.md must begin with YAML frontmatter");
    }
    let mut frontmatter = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line);
    }
    if !closed {
        bail!("SKILL.md frontmatter is missing its closing ---");
    }
    Ok((frontmatter.join("\n"), lines.collect::<Vec<_>>().join("\n")))
}

pub(crate) fn required_yaml_string(mapping: &yaml_rust2::yaml::Hash, key: &str) -> Result<String> {
    optional_yaml_string(mapping, key)?.ok_or_else(|| anyhow::anyhow!("skill {key} is required"))
}

pub(crate) fn optional_yaml_string(
    mapping: &yaml_rust2::yaml::Hash,
    key: &str,
) -> Result<Option<String>> {
    let Some(value) = mapping.get(&Yaml::String(key.to_string())) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("skill {key} must be a string"))?
        .trim()
        .to_string();
    if value.is_empty() {
        bail!("skill {key} must not be empty");
    }
    Ok(Some(value))
}

pub(crate) fn yaml_string_map(
    mapping: &yaml_rust2::yaml::Hash,
    key: &str,
) -> Result<BTreeMap<String, String>> {
    let Some(value) = mapping.get(&Yaml::String(key.to_string())) else {
        return Ok(BTreeMap::new());
    };
    let values = value
        .as_hash()
        .ok_or_else(|| anyhow::anyhow!("skill {key} must be a string-to-string mapping"))?;
    let mut result = BTreeMap::new();
    for (name, value) in values {
        let name = name
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("skill {key} keys must be strings"))?;
        let value = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("skill {key} values must be strings"))?;
        result.insert(name.to_string(), value.to_string());
    }
    Ok(result)
}

pub(crate) fn validate_skill_name(name: &str) -> Result<()> {
    let len = name.chars().count();
    if !(1..=64).contains(&len) || name.starts_with('-') || name.ends_with('-') {
        bail!("skill name must be 1-64 lowercase ASCII letters, digits, or single hyphens");
    }
    let mut previous_hyphen = false;
    for character in name.chars() {
        let valid =
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-';
        if !valid || character == '-' && previous_hyphen {
            bail!("skill name must be 1-64 lowercase ASCII letters, digits, or single hyphens");
        }
        previous_hyphen = character == '-';
    }
    Ok(())
}

pub(crate) fn validate_description(description: &str) -> Result<()> {
    if !(1..=1024).contains(&description.trim().chars().count()) {
        bail!("skill description must be 1-1024 characters");
    }
    Ok(())
}

pub(crate) fn read_skill_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("SKILL.md must be a regular file: {}", path.display());
    }
    if metadata.len() > MAX_SKILL_FILE_BYTES {
        bail!("SKILL.md exceeds the {MAX_SKILL_FILE_BYTES} byte limit");
    }
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

pub fn hash_metadata(hasher: &mut blake3::Hasher, path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            hasher.update(&[1]);
            hasher.update(&metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified().and_then(|time| {
                time.duration_since(UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
                hasher.update(&modified.as_secs().to_le_bytes());
                hasher.update(&modified.subsec_nanos().to_le_bytes());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                hasher.update(&metadata.ino().to_le_bytes());
                hasher.update(&metadata.ctime().to_le_bytes());
                hasher.update(&metadata.ctime_nsec().to_le_bytes());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hasher.update(&[0]);
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn persona_scope(config: &AppConfig, scope: SkillScope) -> Option<String> {
    (scope == SkillScope::Persona).then(|| config.active_persona_scope())
}

pub(crate) fn target_path(
    paths: &MiyuPaths,
    name: &str,
    scope: SkillScope,
    persona: Option<&str>,
) -> Result<PathBuf> {
    validate_skill_name(name)?;
    match scope {
        SkillScope::Global => {
            if persona.is_some() {
                bail!("global skill drafts may not contain a persona scope");
            }
            if name == "personas" {
                bail!("personas is reserved for persona-scoped skills");
            }
            Ok(paths.skills_dir.join(name))
        }
        SkillScope::Persona => {
            let persona = persona.context("persona skill draft is missing its persona scope")?;
            validate_persona_scope(persona)?;
            Ok(paths.skills_dir.join("personas").join(persona).join(name))
        }
    }
}

pub(crate) fn validate_persona_scope(scope: &str) -> Result<()> {
    if scope.is_empty()
        || scope.len() > 64
        || scope == "."
        || scope == ".."
        || scope != persona_scope_name(scope)
        || !scope
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("invalid persona skill scope");
    }
    Ok(())
}

#[cfg(test)]
mod humanize_tests {
    use super::*;

    /// 英文界面下技能名不能是裸 id。09-23 用户截图：设置页上脚本那栏按 locale
    /// 变英文、技能那栏留中文；改成走模型槽之后，模型槽本身就是 id
    /// （`travel-planner`），直接摆上去只是把一种难看换成另一种。
    #[test]
    fn a_skill_id_becomes_a_readable_english_name() {
        assert_eq!(humanize_skill_id("travel-planner"), "Travel planner");
        assert_eq!(
            humanize_skill_id("linux-game-compatibility"),
            "Linux game compatibility"
        );
        assert_eq!(humanize_skill_id("script_creator"), "Script creator");
    }

    /// 拼不出东西就退回原样,别给个空字符串。
    #[test]
    fn a_degenerate_id_falls_back_to_itself() {
        assert_eq!(humanize_skill_id("---"), "---");
        assert_eq!(humanize_skill_id(""), "");
    }
}
