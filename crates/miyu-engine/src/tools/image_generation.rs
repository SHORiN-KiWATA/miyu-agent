use super::vision::ReferenceResolver;
use super::{vision, ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use base64::Engine;
use chrono::Local;
use miyu_base::config::{AppConfig, ImageGenerationPluginConfig};
use miyu_base::paths::MiyuPaths;
use reqwest::Client;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const DESCRIPTION: &str = "Generate an image from a text prompt using the configured OpenAI or RightCode image API. Pass reference_images to work from existing pictures (image-to-image); each entry takes the same kind of reference vision_analyze accepts — a local image path, an http(s) image URL, or a context_image_N id from the chat history. Emits an image event and returns a local path; the host displays emitted images automatically. Do not call print_image unless the user explicitly asks to display it.";

// 平台面的契约:图不自动投递,生成后由模型用 send_message_to_user 发出。
const PLATFORM_DESCRIPTION: &str = "Generate an image from a text prompt using the configured OpenAI or RightCode image API. Pass reference_images to work from existing pictures (image-to-image); each entry takes the same kind of reference vision_analyze accepts — a local image path, an http(s) image URL, or a context_image_N id from the chat history. Returns the local path of the generated image; it is not delivered automatically — after generating, send it to the user with send_message_to_user, passing images=[{\"path\": ..., \"alt\": ...}].";

fn parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": { "type": "string", "description": "Image generation prompt." },
            "reference_images": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional reference images for image-to-image. Same references vision_analyze takes: local image paths, http(s) image URLs, or context_image_N ids."
            },
            "aspect_ratio": { "type": "string", "enum": ["自动", "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9"], "description": "Optional aspect ratio override." },
            "resolution": { "type": "string", "enum": ["1K", "2K", "4K"], "description": "Optional resolution hint for RightCode." }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

pub fn register(registry: &mut ToolRegistry, config: AppConfig, paths: MiyuPaths) {
    let resolver = ReferenceResolver::unscoped(config.clone(), paths);
    register_with_resolver(registry, config, resolver, false);
}

/// 作用域注册:参考图沿用 `vision_analyze` 的白名单作用域,由 vision 侧在
/// 每回合构造好同一份 state 后调进来。`platform` 为真=消息平台回合:图不再
/// 自动投递,由模型显式 send_message_to_user 发送(08-20 用户裁定——自动投
/// 递静默失灵过,显式发送链路对模型透明可自救)。
pub(crate) fn register_scoped(
    registry: &mut ToolRegistry,
    config: AppConfig,
    resolver: ReferenceResolver,
    platform: bool,
) {
    register_with_resolver(registry, config, resolver, platform);
}

fn register_with_resolver(
    registry: &mut ToolRegistry,
    config: AppConfig,
    resolver: ReferenceResolver,
    platform: bool,
) {
    let resolver = Arc::new(resolver);
    registry.register(
        ToolSpec::new_with_progress(
            "generate_image",
            if platform {
                PLATFORM_DESCRIPTION
            } else {
                DESCRIPTION
            },
            parameters(),
            move |args, progress| {
                let config = config.clone();
                let resolver = resolver.clone();
                async move { generate_image(args, config, resolver, progress, platform).await }
            },
        )
        .writes(),
    );
}

/// 生图插件的设置按**调用这一刻**磁盘上的为准,不按回合开始时的快照。
///
/// 09-23 真机:她在回合里替用户把 API key 写进配置、跑了 `miyu reload`,再调
/// 生图仍然是旧的(没 key 的)那份——在跑的回合拿着开工时的配置快照,reload
/// 只管后面的回合。密钥、端点、模型这一段是「改了就该当场生效」的,每次调用
/// 重读一次(生图本身要几秒到几十秒,读一次配置不值一提)。
///
/// 只换这一段:快照里别的字段可能带着成员 / 回合级的覆盖,整份换掉就丢了。
/// 读不出来(文件正写到一半、校验不过)就退回快照,不让一次坏读拖垮生图。
fn current_settings(snapshot: &AppConfig, paths: &MiyuPaths) -> ImageGenerationPluginConfig {
    match AppConfig::load_or_default(paths) {
        Ok(on_disk) => on_disk.plugins.image_generation,
        Err(error) => {
            tracing::warn!(error = %error, "image generation: config reread failed, using the turn snapshot");
            snapshot.plugins.image_generation.clone()
        }
    }
}

async fn generate_image(
    args: Value,
    config: AppConfig,
    resolver: Arc<ReferenceResolver>,
    progress: crate::tools::ToolProgress,
    platform: bool,
) -> Result<String> {
    let plugin = current_settings(&config, &resolver.paths);
    let plugin = &plugin;
    if !plugin.enabled {
        bail!("image generation plugin is disabled")
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if prompt.is_empty() {
        bail!("prompt is required")
    }
    let aspect_ratio = args
        .get("aspect_ratio")
        .and_then(Value::as_str)
        .unwrap_or(&plugin.default_aspect_ratio)
        .trim();
    let resolution = args
        .get("resolution")
        .and_then(Value::as_str)
        .unwrap_or(&plugin.default_resolution)
        .trim();
    // 平台回合的配额由代码承担,不写进 prompt 求模型自觉;本地会话无 task-local,天然放行。
    if !miyu_base::workspace::try_allow_image() {
        bail!("image generation limit reached: only one image per user request in messaging-platform conversations. Wait for the next user message before generating another.")
    }
    let mut references = Vec::new();
    if let Some(value) = args.get("reference_images") {
        let wanted = reference_list(value)?;
        for item in &wanted {
            references.push(resolver.resolve(item).await?);
        }
    }
    let bytes = match request_image(plugin, prompt, aspect_ratio, resolution, &references).await {
        Ok(bytes) => bytes,
        Err(error) => {
            // 失败退还配额:同一请求内允许重试,只有成功的生成才占额度。
            miyu_base::workspace::refund_image_gen_allowance();
            return Err(error);
        }
    };
    let path = save_image(plugin, prompt, &bytes)?;
    // 平台回合不发图片事件:自动投递撤销,模型按 PLATFORM_INSTRUCTION 用
    // send_message_to_user 显式发送(生成目录的图有附件门槛豁免)。
    if !platform {
        progress.report_image(path.clone(), prompt.to_string());
    }
    let should_render_terminal = !platform
        && plugin.auto_print
        && config.plugins.print_image.enabled
        && progress.prepare_for_external_output().await;
    let print_error = if should_render_terminal {
        vision::print_image_file(
            &path,
            vision::configured_print_size(&config.plugins.print_image),
        )
        .await
        .err()
        .map(|err| err.to_string())
    } else {
        None
    };
    let printed = should_render_terminal && print_error.is_none();
    let path_text = path.display().to_string();
    // The reply used to be told to quote the absolute path back. On a chat
    // platform that put the host's home directory in front of strangers — and
    // bought nothing, because the image itself is delivered as its own
    // attachment, independent of the text.
    Ok(json!({
        "status": "ok",
        "path": path_text,
        "bytes": bytes.len(),
        "reference_images": references.len(),
        "printed": printed,
        "print_error": print_error,
        "assistant_instruction": if platform {
            PLATFORM_INSTRUCTION
        } else if printed {
            PRINTED_INSTRUCTION
        } else {
            EMITTED_INSTRUCTION
        }
    })
    .to_string())
}

const PRINTED_INSTRUCTION: &str = "The generated image has already been emitted to the host and printed in the terminal. Do not call send_message_to_user or print_image for the same image unless the user explicitly asks to resend or redisplay it. Do not put the local file path in your reply.";

const EMITTED_INSTRUCTION: &str = "The generated image was saved to disk and emitted as an image event; the host displays emitted images automatically. Do not call print_image unless the user explicitly asks to display it. Do not put the local file path in your reply.";

// 平台回合的图不自动投递(08-20 裁定):模型显式发送,链路对它透明,失败
// 时它能看见错误并重试——自动投递静默失灵过三连。
const PLATFORM_INSTRUCTION: &str = "The image was saved to disk at the returned path. It is not delivered automatically: send it to the user exactly once with send_message_to_user, images=[{\"path\": <the returned path>, \"alt\": <short description>}]. If a send_message_to_user result in this conversation already shows this image delivered, the send is complete — never send the same image twice. Do not put the local file path in your reply text.";

/// 把 `reference_images` 收成一串引用。
///
/// 在 stub 加载模式下模型看到的只有一句摘要和宽松参数壳，没取契约就调用时
/// 很容易把数组写成"数组的 JSON 字符串"——实测 mimo-v2.5 传的就是
/// `"[\"/home/shorin/Pictures/3.png\"]"`。此前这里只认真数组，其余一律
/// `as_array()` 拿到 None 后**静默**退回纯文生图：用户连传了三次才从返回体
/// 的 `reference_images: 0` 看出没生效。
///
/// 现在把常见的几种写法都收下（真数组、单个字符串、字符串里装的 JSON 数组），
/// 收不出任何一条时明确报错——传了参考图却当文生图跑，比直接失败更难查。
fn reference_list(value: &Value) -> Result<Vec<String>> {
    let mut out = Vec::new();
    // 空数组/空串是"明确不给",不报错;给了内容却一条都取不出才报错。
    let had_content = match value {
        Value::Null => false,
        Value::Array(items) => {
            for item in items {
                if !matches!(item, Value::String(_)) {
                    bail!("reference_images entries must be strings, got {item}");
                }
            }
            out = super::string_list(Some(value));
            !items.is_empty()
        }
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(out);
            }
            if text.starts_with('[') && serde_json::from_str::<Value>(text).is_err() {
                bail!("reference_images is not valid JSON: {text}");
            }
            out = super::string_list(Some(value));
            true
        }
        other => bail!("reference_images must be a string or an array of strings, got {other}"),
    };
    if had_content && out.is_empty() {
        bail!("reference_images was given but contained no usable image reference")
    }
    Ok(out)
}

/// 参考图的两条传法。两条都不支持时静默忽略参考图,退回纯文生图——多传了
/// 一个参数不该让整次调用失败(与 AstrBot 生图插件同款取舍)。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReferenceMode {
    /// RightCode 兼容接口:/v1/images/generations 的 JSON 里带 image:[base64…]。
    GenerationsJson,
    /// OpenAI GPT Image:/v1/images/edits 走 multipart,字段名 image[]。
    EditsMultipart,
    Unsupported,
}

fn reference_mode(plugin: &ImageGenerationPluginConfig) -> ReferenceMode {
    if plugin.provider_type == "rightcode" {
        ReferenceMode::GenerationsJson
    } else if looks_gpt_image_model(&plugin.model) {
        ReferenceMode::EditsMultipart
    } else {
        ReferenceMode::Unsupported
    }
}

async fn request_image(
    plugin: &ImageGenerationPluginConfig,
    prompt: &str,
    aspect_ratio: &str,
    resolution: &str,
    references: &[(Vec<u8>, String)],
) -> Result<Vec<u8>> {
    let api_key = plugin
        .api_keys
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .find(|key| !key.is_empty())
        .context("plugins.image_generation.api_keys is empty")?;
    let base = plugin.base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        bail!("plugins.image_generation.base_url is empty")
    }
    let mode = if references.is_empty() {
        ReferenceMode::Unsupported
    } else {
        reference_mode(plugin)
    };
    if !references.is_empty() && mode == ReferenceMode::Unsupported {
        tracing::warn!(
            provider_type = %plugin.provider_type,
            model = %plugin.model,
            count = references.len(),
            "image generation backend does not accept reference images; falling back to text-to-image"
        );
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(plugin.timeout_seconds))
        .build()?;
    let request = match mode {
        ReferenceMode::EditsMultipart => {
            let mut form = reqwest::multipart::Form::new()
                .text("model", plugin.model.clone())
                .text("prompt", prompt.to_string())
                .text("n", "1");
            if let Some(size) = resolve_size(plugin, aspect_ratio, resolution) {
                form = form.text("size", size);
            }
            for (index, (data, mime)) in references.iter().enumerate() {
                let part = reqwest::multipart::Part::bytes(data.clone())
                    .file_name(format!("reference-{}", index + 1))
                    .mime_str(mime)
                    .with_context(|| format!("invalid reference image mime: {mime}"))?;
                form = form.part("image[]", part);
            }
            client
                .post(format!("{base}/v1/images/edits"))
                .bearer_auth(api_key)
                .multipart(form)
        }
        ReferenceMode::GenerationsJson | ReferenceMode::Unsupported => {
            let include = mode == ReferenceMode::GenerationsJson;
            client
                .post(format!("{base}/v1/images/generations"))
                .bearer_auth(api_key)
                .json(&payload(
                    plugin,
                    prompt,
                    aspect_ratio,
                    resolution,
                    include.then_some(references).unwrap_or(&[]),
                ))
        }
    };
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("image API error ({status}): {}", preview(&text, 500));
    }
    let data: Value = response.json().await?;
    extract_image(&client, data).await
}

/// 下载「生成好的图」的字节上限。
///
/// 这条路是**跟着第三方返回的 URL 走**的：API 说图在哪就去哪拿，长度完全由
/// 对面说了算。原先是 `bytes().await?.to_vec()`，一个字节的上限都没有。
///
/// 32 MiB 的依据：本工具能请求到的最大分辨率是 2048×2048（见 `resolution`
/// 那张表），这个尺寸的 PNG 常见在 3-8 MB，32 MiB 是 4 倍以上余量。
const MAX_GENERATED_IMAGE_BYTES: usize = 32 * 1024 * 1024;

fn payload(
    plugin: &ImageGenerationPluginConfig,
    prompt: &str,
    aspect_ratio: &str,
    resolution: &str,
    references: &[(Vec<u8>, String)],
) -> Value {
    let rightcode = plugin.provider_type == "rightcode";
    let mut payload = json!({
        "model": plugin.model,
        "prompt": prompt,
        "n": 1,
    });
    if let Some(size) = resolve_size(plugin, aspect_ratio, resolution) {
        payload["size"] = Value::String(size);
    }
    if rightcode {
        if !aspect_ratio.is_empty() && aspect_ratio != "自动" {
            payload["aspect_ratio"] = Value::String(aspect_ratio.to_string());
        }
    }
    if !references.is_empty() {
        // RightCode 兼容接口收 base64 数组;带参考图时它回 url 而不是 b64。
        payload["image"] = Value::Array(
            references
                .iter()
                .map(|(data, _)| {
                    Value::String(base64::engine::general_purpose::STANDARD.encode(data))
                })
                .collect(),
        );
        payload["response_format"] = Value::String("url".to_string());
    } else if !rightcode && !looks_gpt_image_model(&plugin.model) {
        payload["response_format"] = Value::String("b64_json".to_string());
    }
    payload
}

async fn extract_image(client: &Client, response: Value) -> Result<Vec<u8>> {
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .context("image response missing data array")?;
    let first = data.first().context("image response data is empty")?;
    if let Some(b64) = first.get("b64_json").and_then(Value::as_str) {
        return base64::engine::general_purpose::STANDARD
            .decode(b64)
            .context("failed to decode b64_json image");
    }
    if let Some(url) = first.get("url").and_then(Value::as_str) {
        let response = client.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            bail!("failed to download generated image ({status})")
        }
        return crate::tools::http_response::read_bytes(response, MAX_GENERATED_IMAGE_BYTES)
            .await
            .context("generated image is too large");
    }
    bail!("image response contains neither b64_json nor url")
}

fn save_image(plugin: &ImageGenerationPluginConfig, prompt: &str, bytes: &[u8]) -> Result<PathBuf> {
    let output_dir = expand_path(&plugin.output_dir);
    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create output dir {}", output_dir.display()))?;
    let filename = format!(
        "{}-{}.png",
        slug(prompt),
        Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = output_dir.join(filename);
    std::fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn resolve_size(
    plugin: &ImageGenerationPluginConfig,
    aspect_ratio: &str,
    resolution: &str,
) -> Option<String> {
    if plugin.provider_type == "rightcode" {
        return match (aspect_ratio, resolution) {
            ("1:1", "2K" | "4K") => Some("2048x2048".to_string()),
            ("1:1", _) => Some("1024x1024".to_string()),
            ("3:2", "2K" | "4K") => Some("2048x1365".to_string()),
            ("16:9", "2K" | "4K") => Some("2048x1152".to_string()),
            ("4:3", "2K" | "4K") => Some("2048x1536".to_string()),
            ("5:4", "2K" | "4K") => Some("2048x1638".to_string()),
            ("21:9", "2K" | "4K") => Some("2048x878".to_string()),
            ("2:3", "2K" | "4K") => Some("1365x2048".to_string()),
            ("3:4", "2K" | "4K") => Some("1536x2048".to_string()),
            ("9:16", "2K" | "4K") => Some("1152x2048".to_string()),
            ("4:5", "2K" | "4K") => Some("1638x2048".to_string()),
            ("3:2", _) => Some("1536x1024".to_string()),
            ("16:9", _) => Some("1536x864".to_string()),
            ("4:3", _) => Some("1365x1024".to_string()),
            ("5:4", _) => Some("1280x1024".to_string()),
            ("21:9", _) => Some("1536x658".to_string()),
            ("2:3", _) => Some("1024x1536".to_string()),
            ("3:4", _) => Some("1024x1365".to_string()),
            ("9:16", _) => Some("864x1536".to_string()),
            ("4:5", _) => Some("1024x1280".to_string()),
            _ => None,
        };
    }
    if looks_gpt_image_model(&plugin.model) {
        return Some(
            match aspect_ratio {
                "1:1" => "1024x1024",
                "2:3" | "3:4" | "9:16" | "4:5" => "1024x1536",
                "3:2" | "16:9" | "4:3" | "5:4" | "21:9" => "1536x1024",
                _ => "auto",
            }
            .to_string(),
        );
    }
    Some(
        match aspect_ratio {
            "2:3" | "3:4" | "9:16" | "4:5" => "1024x1792",
            "3:2" | "16:9" | "4:3" | "5:4" | "21:9" => "1792x1024",
            _ => "1024x1024",
        }
        .to_string(),
    )
}

fn looks_gpt_image_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("gpt-image")
}

fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.trim().strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    let path = Path::new(value.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        miyu_base::workspace::effective_workdir().join(path)
    }
}

fn slug(value: &str) -> String {
    let mut out = value
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_ascii_whitespace() || matches!(ch, '-' | '_') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "image".to_string()
    } else {
        out.chars().take(48).collect()
    }
}

fn preview(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= limit {
        normalized
    } else {
        format!("{}...", normalized.chars().take(limit).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一件事钉在调用这一层:开工时插件关着、回合中途在配置里打开了,这一回合
    /// 的生图就该认新的。空 prompt 让它在发请求之前停下——认新配置报「缺
    /// prompt」,还拿着快照就报「插件关着」。
    #[tokio::test]
    async fn a_call_reads_the_plugin_settings_on_disk_not_the_turn_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut snapshot = AppConfig::default();
        snapshot.plugins.image_generation.enabled = false;
        let mut written = AppConfig::default();
        written.plugins.image_generation.enabled = true;
        written.save(&paths).unwrap();
        let resolver = Arc::new(ReferenceResolver::unscoped(snapshot.clone(), paths));
        let error = generate_image(
            json!({ "prompt": "" }),
            snapshot,
            resolver,
            crate::tools::ToolProgress::default(),
            false,
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(error, "prompt is required");
    }

    /// 回合开始后才写进配置的 key,这一回合里的生图就得用上(09-23 真机)。
    #[test]
    fn settings_written_mid_turn_are_used_by_the_next_call() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        // 回合开工时的快照:没有 key。
        let snapshot = AppConfig::default();
        assert!(snapshot.plugins.image_generation.api_keys.is_empty());
        // 回合中途,配置文件里写进了 key。
        let mut written = AppConfig::default();
        written.plugins.image_generation.api_keys = vec!["sk-written-mid-turn".to_string()];
        written.save(&paths).unwrap();
        assert_eq!(
            current_settings(&snapshot, &paths).api_keys,
            vec!["sk-written-mid-turn".to_string()]
        );
        // 读不出来就退回快照,不让生图跟着坏掉。
        std::fs::write(&paths.config_file, "{ not json").unwrap();
        assert!(current_settings(&snapshot, &paths).api_keys.is_empty());
    }

    fn plugin(provider_type: &str, model: &str) -> ImageGenerationPluginConfig {
        let mut plugin = ImageGenerationPluginConfig::default();
        plugin.provider_type = provider_type.to_string();
        plugin.model = model.to_string();
        plugin
    }

    /// 模型在 stub 模式下没取契约就调用，会把数组写成各种形状。实测
    /// mimo-v2.5 传的是 `"[\"/path.png\"]"`，此前被静默丢弃。
    #[test]
    fn reference_images_accepts_the_shapes_models_actually_send() {
        let one = vec!["/home/shorin/Pictures/3.png".to_string()];
        // 真数组。
        assert_eq!(
            reference_list(&json!(["/home/shorin/Pictures/3.png"])).unwrap(),
            one
        );
        // 单个字符串。
        assert_eq!(
            reference_list(&json!("/home/shorin/Pictures/3.png")).unwrap(),
            one
        );
        // 字符串里装着 JSON 数组——线上实际踩到的那种。
        assert_eq!(
            reference_list(&json!(r#"["/home/shorin/Pictures/3.png"]"#)).unwrap(),
            one
        );
        assert_eq!(
            reference_list(&json!(["/a.png", "/b.png"])).unwrap(),
            vec!["/a.png".to_string(), "/b.png".to_string()]
        );
        // 没给就是没给,不报错。
        assert!(reference_list(&Value::Null).unwrap().is_empty());
        assert!(reference_list(&json!([])).unwrap().is_empty());
        assert!(reference_list(&json!("")).unwrap().is_empty());

        // 给了却一条都取不出 ⇒ 明确报错,不静默退回文生图。
        assert!(reference_list(&json!(["", "  "])).is_err());
        assert!(reference_list(&json!("[not json")).is_err());
        assert!(reference_list(&json!([1, 2])).is_err());
        assert!(reference_list(&json!(42)).is_err());
    }

    /// 参考图的两条线路各自认哪个后端;都不认时退回纯文生图而不是报错。
    #[test]
    fn reference_mode_follows_the_backend() {
        assert_eq!(
            reference_mode(&plugin("rightcode", "gpt-image-2")),
            ReferenceMode::GenerationsJson
        );
        assert_eq!(
            reference_mode(&plugin("openai", "gpt-image-1")),
            ReferenceMode::EditsMultipart
        );
        assert_eq!(
            reference_mode(&plugin("openai", "dall-e-3")),
            ReferenceMode::Unsupported
        );
    }

    /// 带参考图时 JSON 里要出现 base64 数组,并且改回 url 响应;
    /// 不带参考图时载荷与从前逐字节一致(不能因为加了这个功能就掰缓存)。
    #[test]
    fn generations_payload_carries_reference_images_only_when_given() {
        let plugin = plugin("rightcode", "gpt-image-2");
        let bare = payload(&plugin, "a cat", "1:1", "1K", &[]);
        assert!(bare.get("image").is_none());

        let with_reference = payload(
            &plugin,
            "a cat",
            "1:1",
            "1K",
            &[(vec![1, 2, 3], "image/png".to_string())],
        );
        let images = with_reference["image"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].as_str().unwrap(), "AQID");
        assert_eq!(with_reference["response_format"], "url");
    }

    #[test]
    fn the_reply_is_never_asked_to_quote_the_local_path() {
        // It used to be: the result carried `final_response_must_include_path`
        // and both instructions ordered the model to repeat it. On a chat
        // platform that published the host's home directory, and it bought
        // nothing — the image rides as its own attachment, not as text.
        for instruction in [PRINTED_INSTRUCTION, EMITTED_INSTRUCTION] {
            assert!(
                instruction.contains("Do not put the local file path in your reply"),
                "{instruction}"
            );
            assert!(
                !instruction.contains("include the exact local image path"),
                "{instruction}"
            );
            assert!(
                !instruction.contains("final_response_must_include_path"),
                "{instruction}"
            );
        }
    }
}
