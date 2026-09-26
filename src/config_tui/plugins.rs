//! 一件功能的「怎么配」：密钥、尺寸、账号。
//!
//! 「开不开」不在这里——那是人格的事，归功能表（`features.rs`）。这里只管
//! 字段：`plugin_fields` 按 id 生成表单描述，`apply_plugin_fields` 把填的值写回
//! 配置。加一件带设置页的功能 = 登记表里标 `settings: true` + 这两处各加一个
//! 分支。
//!
//! 2026-09-20 之前这里还有一张手写的 10 项表（插件总表界面按它的下标分发），
//! 和 `builtin_plugins.rs` 的登记表口径对不上：那张表里有 web / vision / memory、
//! 没有闹钟 / 汇率 / 记账。现在表只有一张，这里按 id 取用。

use crate::config_tui::*;

/// 一件功能的「怎么配」。`id` 就是登记表里的 id（功能表那一行的 id）。
///
/// 2026-09-20 起按 id 分发而不是按菜单下标：以前那张手写表的顺序就是语义，
/// 插一行进去后面全错位。
pub(in crate::config_tui) fn edit_plugin_detail(
    ui: &mut Ui,
    config: &mut AppConfig,
    id: &str,
    display_name: &str,
) -> Result<()> {
    let mut fields = plugin_fields(config, id);
    if fields.is_empty() {
        return Ok(());
    }
    let title = format!(" {display_name} ");
    // 编辑已有插件的设置：不挂「保存 / 返回」（用户 09-26），Esc 退出时改过才收。
    let mut before = field_values(&fields);
    let mut selected = 0;
    loop {
        match run_edit_form_linked(ui, &title, &mut fields, selected)? {
            FormOutcome::Link(index) => {
                follow_plugin_link(ui, config, id, &mut fields[index])?;
                // 跳转行打开的菜单自己改自己的，回来刷新的只是这一行的显示值，
                // 不算这张表单的改动。
                before[index] = fields[index].value.clone();
                selected = index;
            }
            _ if fields_changed(&fields, &before) => {
                return apply_plugin_fields(config, id, &fields)
            }
            _ => return Ok(()),
        }
    }
}

/// 跳转行指向的菜单。真相在那个菜单背后的配置里，这里只负责打开它、回来后把
/// 这一行的显示值刷新成新的真相。
fn follow_plugin_link(
    ui: &mut Ui,
    config: &mut AppConfig,
    id: &str,
    field: &mut Field,
) -> Result<()> {
    if id == "knowledge_base" {
        edit_embedding_model(ui, config)?;
        field.value = embedding_model_label(config);
    }
    Ok(())
}

pub(in crate::config_tui) fn plugin_fields(config: &AppConfig, id: &str) -> Vec<Field> {
    match id {
        "web" => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.web.enabled),
            Field::new(
                t("Results per request", "每次返回数量"),
                config.plugins.web.max_results.to_string(),
            ),
            Field::textarea(
                "Tavily API Keys",
                config.plugins.web.tavily_api_keys.join("\n"),
            )
            .sensitive(),
            Field::textarea(
                "Firecrawl API Keys",
                config.plugins.web.firecrawl_api_keys.join("\n"),
            )
            .sensitive(),
            Field::textarea(
                "AnySearch API Keys",
                config.plugins.web.anysearch_api_keys.join("\n"),
            )
            .sensitive(),
            Field::textarea(
                t(
                    "Exa API Keys (optional; keyless free quota)",
                    "Exa API Keys（可留空用免费额度）",
                ),
                config.plugins.web.exa_api_keys.join("\n"),
            )
            .sensitive(),
            Field::new("SearXNG URL", config.plugins.web.searxng_base_url.clone()),
        ],
        "vision" => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.vision.enabled),
            Field::boolean(
                t(
                    "Prefer current chat model for images",
                    "优先使用当前对话模型识图",
                ),
                config.plugins.vision.prefer_current_multimodal_model,
            ),
            Field::new(
                t("Vision provider/model", "识图 Provider/模型"),
                vision_provider_value(config),
            )
            .choices_owned(vision_provider_model_choice_values(config)),
            Field::new(
                t("Response header timeout (seconds)", "响应头超时秒数"),
                config
                    .plugins
                    .vision
                    .response_header_timeout_seconds
                    .to_string(),
            ),
            Field::new(
                t("Stream idle timeout (seconds)", "流空闲超时秒数"),
                config
                    .plugins
                    .vision
                    .stream_idle_timeout_seconds
                    .to_string(),
            ),
            Field::new(
                t("Per-image timeout (seconds)", "单图总超时秒数"),
                config.plugins.vision.image_timeout_seconds.to_string(),
            ),
        ],
        "image_generation" => vec![
            Field::boolean(
                t("Enabled", "启用"),
                config.plugins.image_generation.enabled,
            ),
            Field::new(
                t("Image API type", "生图 API 类型"),
                config.plugins.image_generation.provider_type.clone(),
            )
            .choices(&["openai", "rightcode"]),
            Field::new("Base URL", config.plugins.image_generation.base_url.clone()),
            Field::textarea(
                "API Keys",
                config.plugins.image_generation.api_keys.join("\n"),
            )
            .sensitive(),
            Field::new(
                t("Model", "模型"),
                config.plugins.image_generation.model.clone(),
            ),
            Field::new(
                t("Default aspect ratio", "默认宽高比"),
                config.plugins.image_generation.default_aspect_ratio.clone(),
            )
            .choices(&[
                "自动", "1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9",
            ]),
            Field::new(
                t("Default resolution", "默认分辨率"),
                config.plugins.image_generation.default_resolution.clone(),
            )
            .choices(&["1K", "2K", "4K"]),
            Field::new(
                t("Output directory", "输出目录"),
                config.plugins.image_generation.output_dir.clone(),
            ),
            Field::boolean(
                t("Print when complete", "完成后打印"),
                config.plugins.image_generation.auto_print,
            ),
            Field::new(
                t("Timeout (seconds)", "超时秒数"),
                config.plugins.image_generation.timeout_seconds.to_string(),
            ),
        ],
        "web_images" => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.web_images.enabled),
            Field::new(
                t("Search source mode", "搜索来源模式"),
                config.plugins.web_images.source_mode.clone(),
            )
            .choices(&["auto", "global", "mainland"]),
            Field::new(
                t("Maximum results", "数量上限"),
                config.plugins.web_images.max_results.to_string(),
            ),
            Field::boolean(
                t("Safe search", "安全搜索"),
                config.plugins.web_images.safe_search,
            ),
            Field::new(
                t("Maximum download (MB)", "最大下载 MB"),
                config.plugins.web_images.max_download_mb.to_string(),
            ),
            Field::new(
                t("Timeout (seconds)", "超时秒数"),
                config.plugins.web_images.timeout_seconds.to_string(),
            ),
        ],
        "print_image" => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.print_image.enabled),
            Field::new(
                t("Print width percent", "打印宽度百分比"),
                config.plugins.print_image.width_percent.to_string(),
            ),
            Field::new(
                t("Print height percent", "打印高度百分比"),
                config.plugins.print_image.height_percent.to_string(),
            ),
        ],
        "memes" => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.memes.enabled),
            Field::new(
                t("Send width percent", "发送宽度百分比"),
                config.plugins.memes.width_percent.to_string(),
            ),
            Field::new(
                t("Send height percent", "发送高度百分比"),
                config.plugins.memes.height_percent.to_string(),
            ),
            Field::new(
                t("Maximum image size (MB)", "最大图片 MB"),
                config.plugins.memes.max_image_mb.to_string(),
            ),
            Field::new(
                t("Maximum search results", "搜索最大结果数"),
                config.plugins.memes.search_max_results.to_string(),
            ),
            Field::boolean(
                t("Allow animated GIFs", "允许 GIF 动画"),
                config.plugins.memes.allow_gif_animation,
            ),
            Field::boolean(
                t("Suggest memes automatically", "自动提示发送表情"),
                config.plugins.memes.auto_send_enabled,
            ),
            Field::boolean(
                t(
                    "Suggest memes automatically on platforms",
                    "通讯平台自动提示发送表情",
                ),
                config.plugins.memes.auto_send_platform_enabled,
            ),
            Field::new(
                t(
                    "Automatic meme suggestion probability",
                    "自动提示发送表情概率",
                ),
                config.plugins.memes.auto_send_probability.to_string(),
            ),
        ],
        "knowledge_base" => vec![
            Field::boolean(t("Enabled", "启用"), config.plugins.knowledge_base.enabled),
            Field::new(
                t("Knowledge base directory", "知识库目录"),
                config.plugins.knowledge_base.data_dir.clone(),
            ),
            Field::new(
                t("Maximum search results", "搜索最大结果数"),
                config.plugins.knowledge_base.max_search_results.to_string(),
            ),
            Field::new(
                t("Snippet context characters", "片段上下文字数"),
                config
                    .plugins
                    .knowledge_base
                    .snippet_context_chars
                    .to_string(),
            ),
            Field::new(
                t("Proximity window characters", "同窗匹配范围"),
                config
                    .plugins
                    .knowledge_base
                    .proximity_window_chars
                    .to_string(),
            ),
            Field::new(
                t("Maximum lines to read", "读取最大行数"),
                config.plugins.knowledge_base.max_read_lines.to_string(),
            ),
            Field::new(
                t("Maximum file size (KB)", "最大文件 KB"),
                config.plugins.knowledge_base.max_file_size_kb.to_string(),
            ),
            Field::boolean(
                t("Allow AI uploads", "允许 AI 上传"),
                config.plugins.knowledge_base.upload_tool_enabled,
            ),
            Field::boolean(
                t("Enable embedding", "启用 Embedding"),
                config.plugins.knowledge_base.embedding_enabled,
            ),
            // 知识库用的就是全局那份 embedding（08-10 起，`Embedder::from_config`）。
            // 这一行原先绑着搬家前的 `plugins.knowledge_base.embedding_*`：运行时
            // 不读它，于是用内置 bge 时主页写着「本地 · bge」，这里却说「未配置」
            // （09-23 用户反馈）。改成展示全局值、回车进主页同一个菜单。
            Field::link(
                t("Embedding model (global)", "Embedding 模型（全局）"),
                embedding_model_label(config),
            ),
            Field::new(
                t("Semantic chunk size", "语义块大小"),
                config
                    .plugins
                    .knowledge_base
                    .semantic_chunk_chars
                    .to_string(),
            ),
            Field::new(
                t("Semantic chunk overlap", "语义块重叠"),
                config
                    .plugins
                    .knowledge_base
                    .semantic_chunk_overlap
                    .to_string(),
            ),
            Field::new(
                t("Semantic candidates", "语义候选数"),
                config.plugins.knowledge_base.semantic_top_k.to_string(),
            ),
            // 「语义最低分」「Embedding 超时秒数」不再摆：运行时读的是全局 embedding
            // 高级设置里的同名项，这里改了不生效（用户 09-23 拍板隐藏，字段留着给
            // 老配置迁移读）。
            Field::new(
                t("Strong keyword match threshold", "关键词强命中阈值"),
                config
                    .plugins
                    .knowledge_base
                    .keyword_strong_score_threshold
                    .to_string(),
            ),
        ],
        "archlinux" => vec![Field::boolean(
            t("Enabled", "启用"),
            config.plugins.archlinux.enabled,
        )],
        "memory" => {
            let memory = config.memory_config();
            vec![
                Field::boolean(t("Enabled", "启用"), memory.enabled),
                Field::boolean(
                    t("Evicted context cache", "上下文弹出缓存"),
                    memory.evicted_context_enabled,
                ),
                Field::boolean(
                    t("Enable association", "联想启用"),
                    memory.association_enabled,
                ),
                Field::boolean(t("Automatic diary", "自动日记"), memory.auto_diary_enabled),
                Field::boolean(
                    t("Automatic fact memory", "自动知识记忆"),
                    memory.auto_fact_enabled,
                ),
                Field::new(
                    t("Diary batch size", "日记整理轮数"),
                    memory.diary_batch_size.to_string(),
                ),
                Field::new(
                    t("Short diary retention days", "短期日记保留天数"),
                    memory.short_diary_retention_days.to_string(),
                ),
                Field::new(
                    t("Diary promotion recalls", "日记长期化召回次数"),
                    memory.diary_promotion_recalls.to_string(),
                ),
                Field::new(
                    t("Organizer timeout seconds", "记忆整理超时秒数"),
                    memory.organizer_timeout_seconds.to_string(),
                ),
                Field::new(
                    t("Associated facts", "联想知识条数"),
                    memory.association_facts.to_string(),
                ),
                Field::new(
                    t("Associated events", "联想事件条数"),
                    memory.association_episodes.to_string(),
                ),
                Field::new(
                    t("Association character limit", "联想字符上限"),
                    memory.association_max_chars.to_string(),
                ),
                Field::boolean(
                    t("Enable forgetting", "遗忘启用"),
                    memory.forgetting_enabled,
                ),
                Field::new(
                    t("Forgetting half-life (days)", "遗忘半衰期天"),
                    memory.forgetting_half_life_days.to_string(),
                ),
                Field::new(
                    t("Minimum forgetting strength", "遗忘最低强度"),
                    memory.forgetting_min_strength.to_string(),
                ),
                Field::new(
                    t("Recall boost strength", "回忆增强强度"),
                    memory.forgetting_review_boost.to_string(),
                ),
                Field::boolean(
                    t("Association dedup", "联想跨回合去重"),
                    memory.association_dedup,
                ),
            ]
        }
        // 没有专属设置页的（闹钟、汇率、记账、脚本、MCP…）。功能表上它们那行
        // 不摆齿轮，正常走不到这里。
        _ => Vec::new(),
    }
}

pub(in crate::config_tui) fn apply_plugin_fields(
    config: &mut AppConfig,
    id: &str,
    fields: &[Field],
) -> Result<()> {
    match id {
        "web" => {
            config.plugins.web.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.web.max_results = fields[1].value.trim().parse::<usize>()?.clamp(1, 10);
            config.plugins.web.tavily_api_keys = parse_key_list(&fields[2].value);
            config.plugins.web.firecrawl_api_keys = parse_key_list(&fields[3].value);
            config.plugins.web.anysearch_api_keys = parse_key_list(&fields[4].value);
            config.plugins.web.exa_api_keys = parse_key_list(&fields[5].value);
            config.plugins.web.searxng_base_url =
                fields[6].value.trim().trim_end_matches('/').to_string();
        }
        "vision" => {
            config.plugins.vision.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.vision.prefer_current_multimodal_model =
                parse_bool_field(&fields[1].value)?;
            let (provider_id, model) = parse_provider_model_choice(&fields[2].value);
            config.plugins.vision.vision_provider_id = provider_id;
            config.plugins.vision.vision_model = model;
            config.plugins.vision.response_header_timeout_seconds =
                fields[3].value.trim().parse::<u64>()?.max(1);
            config.plugins.vision.stream_idle_timeout_seconds =
                fields[4].value.trim().parse::<u64>()?.max(1);
            config.plugins.vision.image_timeout_seconds =
                fields[5].value.trim().parse::<u64>()?.max(1);
        }
        "image_generation" => {
            config.plugins.image_generation.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.image_generation.provider_type = fields[1].value.trim().to_string();
            config.plugins.image_generation.base_url =
                fields[2].value.trim().trim_end_matches('/').to_string();
            config.plugins.image_generation.api_keys = parse_key_list(&fields[3].value);
            config.plugins.image_generation.model = fields[4].value.trim().to_string();
            config.plugins.image_generation.default_aspect_ratio =
                fields[5].value.trim().to_string();
            config.plugins.image_generation.default_resolution = fields[6].value.trim().to_string();
            config.plugins.image_generation.output_dir = fields[7].value.trim().to_string();
            config.plugins.image_generation.auto_print = parse_bool_field(&fields[8].value)?;
            config.plugins.image_generation.timeout_seconds = fields[9].value.trim().parse()?;
        }
        "web_images" => {
            config.plugins.web_images.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.web_images.source_mode = match fields[1].value.trim() {
                "auto" | "global" | "mainland" => fields[1].value.trim().to_string(),
                other => {
                    if is_zh() {
                        anyhow::bail!("未知搜图来源模式: {other}")
                    } else {
                        anyhow::bail!("Unknown image search source mode: {other}")
                    }
                }
            };
            // 下标两次整体前移:09-22 删「自动预览 / 默认预览数量」(搜图不再自己
            // 显示),09-23 删「视觉模型审核」(审核整条撤掉)。
            config.plugins.web_images.max_results =
                fields[2].value.trim().parse::<usize>()?.clamp(1, 10);
            config.plugins.web_images.safe_search = parse_bool_field(&fields[3].value)?;
            config.plugins.web_images.max_download_mb =
                fields[4].value.trim().parse::<f64>()?.clamp(0.1, 50.0);
            config.plugins.web_images.timeout_seconds =
                fields[5].value.trim().parse::<u64>()?.clamp(5, 120);
        }
        "print_image" => {
            config.plugins.print_image.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.print_image.width_percent = fields[1].value.trim().parse::<u8>()?;
            config.plugins.print_image.height_percent = fields[2].value.trim().parse::<u8>()?;
        }
        "memes" => {
            config.plugins.memes.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.memes.width_percent =
                fields[1].value.trim().parse::<u8>()?.clamp(1, 100);
            config.plugins.memes.height_percent =
                fields[2].value.trim().parse::<u8>()?.clamp(1, 100);
            config.plugins.memes.max_image_mb =
                fields[3].value.trim().parse::<u64>()?.clamp(1, 100);
            config.plugins.memes.search_max_results =
                fields[4].value.trim().parse::<usize>()?.clamp(1, 10);
            config.plugins.memes.allow_gif_animation = parse_bool_field(&fields[5].value)?;
            config.plugins.memes.auto_send_enabled = parse_bool_field(&fields[6].value)?;
            config.plugins.memes.auto_send_platform_enabled = parse_bool_field(&fields[7].value)?;
            config.plugins.memes.auto_send_probability =
                fields[8].value.trim().parse::<f32>()?.clamp(0.0, 1.0);
        }
        "knowledge_base" => {
            config.plugins.knowledge_base.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.knowledge_base.data_dir = fields[1].value.trim().to_string();
            config.plugins.knowledge_base.max_search_results = fields[2].value.trim().parse()?;
            config.plugins.knowledge_base.snippet_context_chars = fields[3].value.trim().parse()?;
            config.plugins.knowledge_base.proximity_window_chars =
                fields[4].value.trim().parse()?;
            config.plugins.knowledge_base.max_read_lines = fields[5].value.trim().parse()?;
            config.plugins.knowledge_base.max_file_size_kb = fields[6].value.trim().parse()?;
            config.plugins.knowledge_base.upload_tool_enabled = parse_bool_field(&fields[7].value)?;
            config.plugins.knowledge_base.embedding_enabled = parse_bool_field(&fields[8].value)?;
            // fields[9] 是跳转行，只展示全局 embedding，不写回任何东西。
            config.plugins.knowledge_base.semantic_chunk_chars = fields[10].value.trim().parse()?;
            config.plugins.knowledge_base.semantic_chunk_overlap =
                fields[11].value.trim().parse()?;
            config.plugins.knowledge_base.semantic_top_k = fields[12].value.trim().parse()?;
            config.plugins.knowledge_base.keyword_strong_score_threshold =
                fields[13].value.trim().parse()?;
        }
        "archlinux" => {
            config.plugins.archlinux.enabled = parse_bool_field(&fields[0].value)?;
        }
        "memory" => {
            config.memory = miyu_base::config::MemoryConfig::default();
            config.plugins.memory.enabled = parse_bool_field(&fields[0].value)?;
            config.plugins.memory.evicted_context_enabled = parse_bool_field(&fields[1].value)?;
            config.plugins.memory.association_enabled = parse_bool_field(&fields[2].value)?;
            config.plugins.memory.auto_diary_enabled = parse_bool_field(&fields[3].value)?;
            config.plugins.memory.auto_fact_enabled = parse_bool_field(&fields[4].value)?;
            config.plugins.memory.auto_skill_enabled = false;
            config.plugins.memory.diary_batch_size =
                fields[5].value.trim().parse::<usize>()?.clamp(2, 100);
            config.plugins.memory.short_diary_retention_days =
                fields[6].value.trim().parse::<u64>()?.clamp(1, 3650);
            config.plugins.memory.diary_promotion_recalls =
                fields[7].value.trim().parse::<u64>()?.clamp(1, 100);
            config.plugins.memory.organizer_timeout_seconds =
                fields[8].value.trim().parse::<u64>()?.clamp(5, 600);
            config.plugins.memory.association_facts = fields[9].value.trim().parse::<usize>()?;
            config.plugins.memory.association_episodes =
                fields[10].value.trim().parse::<usize>()?;
            config.plugins.memory.association_max_chars =
                fields[11].value.trim().parse::<usize>()?;
            config.plugins.memory.forgetting_enabled = parse_bool_field(&fields[12].value)?;
            config.plugins.memory.forgetting_half_life_days =
                fields[13].value.trim().parse::<f64>()?;
            config.plugins.memory.forgetting_min_strength =
                fields[14].value.trim().parse::<f64>()?;
            config.plugins.memory.forgetting_review_boost =
                fields[15].value.trim().parse::<f64>()?;
            config.plugins.memory.association_dedup = parse_bool_field(&fields[16].value)?;
        }
        _ => {}
    }
    Ok(())
}
