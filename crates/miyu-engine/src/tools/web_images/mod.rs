mod download;
#[cfg(test)]
use crate::tools::net_guard::is_safe_remote_url;
use crate::tools::net_guard::resolve_public_remote_target;
mod providers;
mod ranking;
use download::*;
use providers::*;
use ranking::*;

use super::{ToolProgress, ToolRegistry, ToolSpec};
use anyhow::{bail, Context, Result};
use futures_util::{future::join_all, StreamExt};
use image::GenericImageView;
use miyu_base::config::AppConfig;
use miyu_base::i18n::text as t;
use miyu_base::paths::MiyuPaths;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

static PROVIDER_COOLDOWNS: LazyLock<Mutex<HashMap<&'static str, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static IMAGE_DECODE_PERMITS: LazyLock<std::sync::Arc<Semaphore>> =
    LazyLock::new(|| std::sync::Arc::new(Semaphore::new(4)));
static CACHE_PUBLISH_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

/// 平台回合的收尾指令:图不自动投递,由模型 `send_message_to_user` 发,而且
/// 发完再写正文就是**第二条消息**(用户 09-21 实录:搜完图她发了图,又补了一条
/// 「图片发出来了，就长这样」)。原来两套指令都是照本地终端写的,QQ 里她收到的
/// 是「In your final response, include useful local_path…」——既让她贴一条对面
/// 打不开的本地路径,又硬性要求她再写一段。与 `generate_image` 的平台版同源。
const PLATFORM_INSTRUCTION: &str = "The images are on this host, not delivered. Send the ones worth showing in one send_message_to_user call: images takes the whole list, and the caption belongs in that same call's text so it arrives with them. The reader cannot open a local_path, so keep paths out of message text. Your final reply is delivered as a separate message after that send, so leave it empty.";

const LOCAL_INSTRUCTION: &str = "The images are downloaded at local_path; nothing is shown yet. Print the ones worth looking at with print_image, which takes a list of paths. Keep raw paths out of your prose unless the user wants the files.";

/// 收尾指令按宿主选:两边都是「图在本地,要给人看得自己发/自己打」,区别只是
/// 用哪件工具。
fn assistant_instruction(platform: bool) -> &'static str {
    if platform {
        PLATFORM_INSTRUCTION
    } else {
        LOCAL_INSTRUCTION
    }
}

pub fn register(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    allow_download: bool,
) {
    register_for_host(registry, config, paths, allow_download, false);
}

/// 平台回合用这个覆盖上面那份:工具面本身一个字节不变(描述与参数同一份),
/// 只换结果里的收尾指令,所以不掰缓存前缀(§1.1)。
pub fn register_platform(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    allow_download: bool,
) {
    register_for_host(registry, config, paths, allow_download, true);
}

fn register_for_host(
    registry: &mut ToolRegistry,
    config: AppConfig,
    paths: MiyuPaths,
    allow_download: bool,
    platform: bool,
) {
    registry.register(ToolSpec::new_with_progress(
        "search_web_images",
        "Search web images with parallel multi-source retrieval, ranking, and deduplication. Sources adapt to global or mainland connectivity and can include SearXNG, DuckDuckGo, Bing CN, Baidu, and 360.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Image search query." },
                "count": { "type": "integer", "description": "Required. Exact number of images to return. Match the user's requested quantity: one/a/an/一张/一幅 means 1; a few/几张 means 3; several/多张 means 5 unless the user gives another number. Do not use the configured maximum as the default." },
                "safe_search": { "type": "boolean", "description": "Enable safe image search. Defaults to plugin config." }
            },
            "required": ["query", "count"],
            "additionalProperties": false
        }),
        move |args, progress| {
            let config = config.clone();
            let paths = paths.clone();
            async move {
                search_web_images(args, config, paths, allow_download, platform, progress).await
            }
        },
    ));
}

async fn search_web_images(
    args: Value,
    config: AppConfig,
    paths: MiyuPaths,
    allow_download: bool,
    platform: bool,
    progress: ToolProgress,
) -> Result<String> {
    let plugin = &config.plugins.web_images;
    if !plugin.enabled {
        bail!("web image search plugin is disabled")
    }
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if query.is_empty() {
        bail!("query is required")
    }
    let Some(count) = args.get("count").and_then(Value::as_u64) else {
        bail!("count is required; choose the number of images from the user's request")
    };
    let count = count.clamp(1, plugin.max_results.clamp(1, 10) as u64) as usize;
    let safe_search = args
        .get("safe_search")
        .and_then(Value::as_bool)
        .unwrap_or(plugin.safe_search)
        || plugin.safe_search;
    let client = Client::builder()
        .timeout(Duration::from_secs(plugin.timeout_seconds.max(5)))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()?;
    progress.report(t("searching image candidates", "正在搜索图片候选"));
    let search = search_images(&client, &config, query, count, safe_search).await?;
    let candidates = search.candidates;
    if !allow_download {
        return Ok(json!({
            "success": !candidates.is_empty(),
            "query": query,
            "count": candidates.len().min(count),
            "mode": "metadata_only",
            "providers": search.diagnostics,
            "images": candidates.into_iter().take(count).map(candidate_json).collect::<Vec<_>>(),
        })
        .to_string());
    }
    // 成员跑时存到成员自己家(home/<user>/pictures),而不是管理员的 data/pictures
    // ——后者成员被 Landlock 挡在外面,下载既落错地方又读不回(用户报)。
    let pictures_base = match config.member_home_dir() {
        Some(home) => home.join("pictures"),
        None => paths.pictures_dir.clone(),
    };
    let cache_dir = pictures_base.join("web-images");
    let stored = download_and_store_images(
        &config,
        &cache_dir,
        query,
        candidates,
        count,
        configured_max_download_bytes(plugin.max_download_mb),
        progress.clone(),
    )
    .await?;
    // 这件工具只负责搜到、下到本地、把路径交回去(用户 09-22 拍板)。
    //
    // 原来它自己还兼两条显示通道:一条无条件的 `report_image`(平台上被当成
    // 本轮产图**自动投递**,终端上自动贴图),一条工具内的 chafa 打印。前者
    // 绕过了 preview / preview_count / auto_preview 全部控制项——用户那一轮
    // 模型明明传了 `preview: false`,搜来的 3 张照样发进了 QQ,连她自己在正文
    // 里判定"一堆假透明"弃用的那几张也发了。
    //
    // 现在两条都撤:平台上由她挑了用 send_message_to_user 发,终端上由她调
    // print_image(已支持批量)。与生图 08-20 的裁定同一口径。
    Ok(json!({
        "success": !stored.is_empty(),
        "query": query,
        "count": stored.len(),
        "result_role": "downloaded_image_candidates",
        "description_policy": "search_description is search-engine metadata, not a look at the image. Check the picture before claiming it matches the request.",
        "providers": search.diagnostics,
        "cache_dir": cache_dir,
        "images": stored.into_iter().map(stored_json).collect::<Vec<_>>(),
        "assistant_instruction": assistant_instruction(platform)
    })
    .to_string())
}

async fn search_images(
    client: &Client,
    config: &AppConfig,
    query: &str,
    count: usize,
    safe_search: bool,
) -> Result<ImageSearchResult> {
    let limit = image_candidate_pool_limit(count);
    let all_providers = image_search_providers(config, query, safe_search);
    let mut diagnostics = Vec::new();
    let mut providers = all_providers
        .iter()
        .copied()
        .filter(provider_ready)
        .collect::<Vec<_>>();
    if providers.is_empty() {
        if let Some(provider) = provider_probe_candidate(&all_providers) {
            providers.push(provider);
        }
    } else {
        for provider in all_providers
            .iter()
            .copied()
            .filter(|provider| !providers.iter().any(|ready| ready.id() == provider.id()))
        {
            diagnostics.push(json!({
                "provider": provider.id(),
                "success": false,
                "skipped": "cooldown",
            }));
        }
    }
    let provider_timeout = Duration::from_secs(config.plugins.web_images.timeout_seconds.max(5));
    let searches = providers.into_iter().map(|provider| {
        let client = client.clone();
        let searxng_base_url = config.plugins.web.searxng_base_url.clone();
        let query = query.to_string();
        async move {
            let started = Instant::now();
            let result = tokio::time::timeout(
                provider_timeout,
                search_with_provider(
                    &client,
                    provider,
                    &searxng_base_url,
                    &query,
                    limit,
                    safe_search,
                ),
            )
            .await;
            let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            (provider, elapsed_ms, result)
        }
    });
    let mut candidates = Vec::new();
    for (provider, elapsed_ms, result) in join_all(searches).await {
        match result {
            Ok(Ok(mut items)) => {
                for (index, item) in items.iter_mut().enumerate() {
                    item.provider_rank = index + 1;
                }
                mark_provider_success(provider);
                diagnostics.push(json!({
                    "provider": provider.id(),
                    "success": true,
                    "elapsed_ms": elapsed_ms,
                    "candidates": items.len(),
                }));
                candidates.extend(items);
            }
            Ok(Err(err)) => {
                let message = err.to_string();
                mark_provider_failure(provider, &message);
                diagnostics.push(json!({
                    "provider": provider.id(),
                    "success": false,
                    "elapsed_ms": elapsed_ms,
                    "error": clean_text(&message, 240),
                }));
            }
            Err(_) => {
                mark_provider_failure(provider, "timeout");
                diagnostics.push(json!({
                    "provider": provider.id(),
                    "success": false,
                    "elapsed_ms": elapsed_ms,
                    "error": "provider timeout",
                }));
            }
        }
    }
    rank_candidates(query, &mut candidates);
    let candidates = dedupe_candidates(candidates);
    if candidates.is_empty() {
        bail!("image search returned no results")
    }
    Ok(ImageSearchResult {
        candidates: candidates.into_iter().take(limit).collect(),
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(title: &str, rank: usize, width: u32, height: u32) -> ImageCandidate {
        ImageCandidate {
            title: title.to_string(),
            page_url: "https://example.com/page".to_string(),
            image_url: format!("https://example.com/{rank}.jpg"),
            thumbnail_url: String::new(),
            source: "test".to_string(),
            width,
            height,
            search_description: String::new(),
            provider_rank: rank,
        }
    }

    fn stored(path: PathBuf, rank: usize) -> StoredImage {
        StoredImage {
            candidate: candidate("test image", rank, 2, 2),
            local_path: path,
            mime_type: "image/png".to_string(),
            size_bytes: 16,
            sha256: format!("hash-{rank}"),
            used_thumbnail: false,
        }
    }

    #[test]
    fn extracts_ddg_vqd() {
        assert_eq!(
            extract_ddg_vqd("foo vqd=\"123-456\" bar"),
            Some("123-456".to_string())
        );
        assert_eq!(extract_ddg_vqd("foo"), None);
    }

    #[test]
    fn detects_png_dimensions() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(&16u32.to_be_bytes());
        assert_eq!(detect_image_dimensions(&bytes, "image/png"), (32, 16));
        assert_eq!(
            detect_image_mime(b"<html>not an image</html>", "image/png", "photo.png"),
            None
        );
    }

    #[test]
    fn exact_model_number_outranks_wrong_high_resolution_model() {
        let query = "华为 Mate 70 Pro 绿色 背面";
        let correct = candidate("华为 Mate 70 Pro 云杉绿 背面", 3, 1000, 800);
        let wrong = candidate("华为 Mate 30 Pro 5G 绿色背面", 1, 3000, 2000);
        assert!(score_candidate(query, &correct) > score_candidate(query, &wrong));
    }

    #[test]
    fn requested_product_outranks_accessory() {
        let query = "华为 Mate 70 Pro 绿色 背面";
        let product = candidate("华为 Mate 70 Pro 云杉绿手机背面", 3, 1000, 800);
        let case = candidate("华为 Mate 70 Pro 绿色手机壳保护套", 1, 3000, 3000);
        assert!(score_candidate(query, &product) > score_candidate(query, &case));
    }

    #[test]
    fn cjk_query_adds_subterms_without_spaces() {
        let terms = image_query_terms("杭州西湖断桥残雪实景");
        assert!(terms.contains(&"断桥".to_string()));
        assert!(terms.contains(&"残雪".to_string()));
    }

    #[test]
    fn blocks_local_and_private_image_urls() {
        for url in [
            "http://localhost/image.png",
            "http://127.0.0.1/image.png",
            "http://10.0.0.1/image.png",
            "http://[::1]/image.png",
            "http://[::ffff:127.0.0.1]/image.png",
        ] {
            assert!(!is_safe_remote_url(&Url::parse(url).unwrap()), "{url}");
        }
        assert!(is_safe_remote_url(
            &Url::parse("https://images.example.com/photo.jpg").unwrap()
        ));
    }

    #[test]
    fn parses_provider_result_shapes() {
        let ddg = parse_ddg_results(
            r#"{"results":[{"title":"cat","url":"https://example.com/page","image":"https://example.com/cat.jpg","thumbnail":"https://example.com/cat-small.jpg","width":800,"height":600}]}"#,
            5,
        )
        .unwrap();
        assert_eq!(ddg.len(), 1);
        let bing = parse_bing_results(
            r#"<a class="iusc" m="{&quot;t&quot;:&quot;cat&quot;,&quot;purl&quot;:&quot;https://example.com/page&quot;,&quot;murl&quot;:&quot;https://example.com/cat.jpg&quot;,&quot;turl&quot;:&quot;https://example.com/cat-small.jpg&quot;}"></a>"#,
            5,
        );
        assert_eq!(bing.len(), 1);
    }

    #[test]
    fn provider_mode_selects_mainland_sources() {
        let mut config = AppConfig::default();
        config.plugins.web_images.source_mode = "mainland".to_string();
        config.plugins.web.searxng_base_url.clear();
        let unsafe_ids = image_search_providers(&config, "猫", false)
            .into_iter()
            .map(ImageSearchProvider::id)
            .collect::<Vec<_>>();
        assert_eq!(unsafe_ids, vec!["bing_cn", "baidu", "so360"]);

        // 百度和 360 不支持安全搜索参数。原来靠下载后的视觉审核兜底,审核 09-23
        // 撤了,安全搜索开着时就不再用它们。
        let safe_ids = image_search_providers(&config, "猫", true)
            .into_iter()
            .map(ImageSearchProvider::id)
            .collect::<Vec<_>>();
        assert_eq!(safe_ids, vec!["bing_cn"]);
    }

    #[test]
    fn legacy_web_images_config_defaults_source_mode() {
        let config: miyu_base::config::WebImagesPluginConfig =
            serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(config.source_mode, "auto");
    }

    #[test]
    fn rejects_images_over_pixel_limit_before_decode() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&4_001u32.to_be_bytes());
        bytes.extend_from_slice(&4_000u32.to_be_bytes());
        assert!(validate_downloaded_image(
            bytes,
            "image/png".to_string(),
            "https://example.com/large.png".to_string(),
        )
        .is_none());
    }

    #[test]
    fn image_pixel_limit_is_inclusive() {
        assert!(image_dimensions_allowed(4_000, 4_000));
        assert!(!image_dimensions_allowed(4_001, 4_000));
        assert!(!image_dimensions_allowed(0, 4_000));
        assert_eq!(IMAGE_DECODER_MAX_ALLOC, 64 * 1024 * 1024);
    }

    #[test]
    fn configured_download_size_is_capped_at_fifty_mib() {
        assert_eq!(configured_max_download_bytes(500.0), 50 * 1024 * 1024);
        assert_eq!(configured_max_download_bytes(f64::NAN), 1024 * 1024 / 10);
    }

    #[test]
    fn duplicate_hashes_keep_candidate_order() {
        let mut later = stored(PathBuf::from("later"), 2);
        later.sha256 = "same".to_string();
        let mut earlier = stored(PathBuf::from("earlier"), 1);
        earlier.sha256 = "same".to_string();

        let deduped = dedupe_downloaded(vec![(1, later), (0, earlier)]);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].local_path, PathBuf::from("earlier"));
    }

    #[tokio::test]
    async fn publish_preserves_preexisting_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let call_dir = CallTempDir::new(dir.path()).unwrap();
        let staged = call_dir.path().join("candidate.png");
        write_temp_file(&staged, b"existing").await.unwrap();
        let mut item = stored(staged, 1);
        item.size_bytes = b"existing".len();
        item.sha256 = hex::encode(Sha256::digest(b"existing"));
        let final_path = dir.path().join(format!("webimg-{}.png", item.sha256));
        tokio::fs::write(&final_path, b"existing").await.unwrap();

        publish_image(dir.path(), &mut item).await.unwrap();

        assert_eq!(item.local_path, final_path);
        assert_eq!(tokio::fs::read(final_path).await.unwrap(), b"existing");
    }

    #[tokio::test]
    async fn concurrent_same_hash_publishes_one_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let first_dir = CallTempDir::new(dir.path()).unwrap();
        let second_dir = CallTempDir::new(dir.path()).unwrap();
        let first_path = first_dir.path().join("first.png");
        let second_path = second_dir.path().join("second.png");
        write_temp_file(&first_path, b"complete").await.unwrap();
        write_temp_file(&second_path, b"complete").await.unwrap();
        let mut first = stored(first_path, 1);
        let mut second = stored(second_path, 2);
        first.size_bytes = b"complete".len();
        second.size_bytes = b"complete".len();
        first.sha256 = hex::encode(Sha256::digest(b"complete"));
        second.sha256 = first.sha256.clone();

        let (first_result, second_result) = tokio::join!(
            publish_image(dir.path(), &mut first),
            publish_image(dir.path(), &mut second)
        );
        first_result.unwrap();
        second_result.unwrap();

        assert_eq!(first.local_path, second.local_path);
        assert_eq!(
            tokio::fs::read(&first.local_path).await.unwrap(),
            b"complete"
        );
        drop(first_dir);
        drop(second_dir);
        assert_eq!(
            tokio::fs::read(&first.local_path).await.unwrap(),
            b"complete"
        );
    }

    #[tokio::test]
    async fn publish_repairs_truncated_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let call_dir = CallTempDir::new(dir.path()).unwrap();
        let staged = call_dir.path().join("candidate.png");
        write_temp_file(&staged, b"complete").await.unwrap();
        let mut item = stored(staged, 1);
        item.size_bytes = b"complete".len();
        item.sha256 = hex::encode(Sha256::digest(b"complete"));
        let final_path = dir.path().join(format!("webimg-{}.png", item.sha256));
        tokio::fs::write(&final_path, b"cut").await.unwrap();

        publish_image(dir.path(), &mut item).await.unwrap();

        assert_eq!(tokio::fs::read(final_path).await.unwrap(), b"complete");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn publish_replaces_invalid_symlink_but_not_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let call_dir = CallTempDir::new(dir.path()).unwrap();
        let staged = call_dir.path().join("candidate.png");
        write_temp_file(&staged, b"complete").await.unwrap();
        let mut item = stored(staged, 1);
        item.size_bytes = b"complete".len();
        item.sha256 = hex::encode(Sha256::digest(b"complete"));
        let final_path = dir.path().join(format!("webimg-{}.png", item.sha256));
        let target = dir.path().join("outside");
        tokio::fs::write(&target, b"outside").await.unwrap();
        symlink(&target, &final_path).unwrap();

        publish_image(dir.path(), &mut item).await.unwrap();
        assert_eq!(tokio::fs::read(&final_path).await.unwrap(), b"complete");

        let directory_hash = hex::encode(Sha256::digest(b"directory"));
        let directory_path = dir.path().join(format!("webimg-{directory_hash}.png"));
        tokio::fs::create_dir(&directory_path).await.unwrap();
        let directory_staged = call_dir.path().join("directory.png");
        write_temp_file(&directory_staged, b"complete")
            .await
            .unwrap();
        let mut directory_item = stored(directory_staged, 2);
        directory_item.size_bytes = b"complete".len();
        directory_item.sha256 = directory_hash;
        let error = publish_image(dir.path(), &mut directory_item)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cache path is a directory"));
        assert!(directory_path.is_dir());
    }

    #[tokio::test]
    async fn abort_cleans_call_temp_directory() {
        let cache = tempfile::tempdir().unwrap();
        let cache_path = cache.path().to_path_buf();
        let (path_sender, path_receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let call_dir = CallTempDir::new(&cache_path).unwrap();
            let staged = call_dir.path().join("candidate.png");
            write_temp_file(&staged, b"temporary").await.unwrap();
            path_sender.send(call_dir.path().to_path_buf()).unwrap();
            futures_util::future::pending::<()>().await;
            drop(call_dir);
        });
        let call_path = path_receiver.await.unwrap();
        assert!(call_path.exists());

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        assert!(!call_path.exists());
    }

    #[tokio::test]
    #[ignore = "live network smoke test"]
    async fn live_provider_smoke_test() {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let mut successes = 0;
        for provider in [
            ImageSearchProvider::DuckDuckGo,
            ImageSearchProvider::BingCn,
            ImageSearchProvider::Baidu,
            ImageSearchProvider::So360,
        ] {
            let result =
                search_with_provider(&client, provider, "", "杭州西湖 断桥残雪 实景", 8, true)
                    .await;
            if result.as_ref().is_ok_and(|items| !items.is_empty()) {
                successes += 1;
            } else {
                eprintln!("{}: {result:?}", provider.id());
            }
        }
        assert!(successes >= 3, "only {successes} providers succeeded");
    }

    #[tokio::test]
    #[ignore = "live network smoke test"]
    async fn live_pinned_download_smoke_test() {
        let (bytes, _, mime) = download_image_bytes(
            "https://www.rust-lang.org/logos/rust-logo-512x512.png",
            "https://www.rust-lang.org/",
            2 * 1024 * 1024,
            Instant::now() + Duration::from_secs(20),
        )
        .await
        .unwrap();
        assert_eq!(
            detect_image_mime(&bytes, &mime, ""),
            Some("image/png".to_string())
        );
    }
}

#[cfg(test)]
mod host_instruction_tests {
    use super::*;

    /// 两边的收尾指令都不许要求她再写一段最终回复,也不许让她把本地路径塞进
    /// 正文。用户 09-21 实录:QQ 里搜完图她先发了图文,又补一条「图片发出来了，
    /// 就长这样」——那一句就是本地版指令里「In your final response, include
    /// useful local_path and page_url values」催出来的。
    #[test]
    fn neither_instruction_demands_a_final_response() {
        for (label, text) in [
            ("平台", assistant_instruction(true)),
            ("终端", assistant_instruction(false)),
        ] {
            assert!(
                !text.contains("In your final response"),
                "{label}版还在要求她再写一段最终回复:{text}"
            );
            assert!(
                !text.contains("include useful local_path"),
                "{label}版还在让她把本地路径写进回复:{text}"
            );
        }
    }

    /// 两边各自指向正确的那件工具:平台发消息、终端打印。
    #[test]
    fn each_host_points_at_its_own_delivery_tool() {
        let platform = assistant_instruction(true);
        assert!(platform.contains("send_message_to_user"), "{platform}");
        assert!(
            platform.contains("separate message"),
            "平台版没说清最终回复会是另一条消息:{platform}"
        );
        assert!(
            !platform.contains("print_image"),
            "QQ 里没有终端可打:{platform}"
        );

        let local = assistant_instruction(false);
        assert!(local.contains("print_image"), "{local}");
        assert!(
            !local.contains("send_message_to_user"),
            "终端会话里没有平台可发:{local}"
        );
    }

    /// 平台版必须说清「一次调用发完整批」。
    ///
    /// 09-22 我第一版写的是 `images=[{...}], once per image`,她读成「一张调
    /// 一次」——真模型 A/B 里 3 张图发成了 3 条独立消息,再加一条「发出来了」,
    /// 一共 4 条。`send_message_to_user.images` 本来就收数组,配文也该放同一次
    /// 调用的 text 里,那样才是一条消息。
    #[test]
    fn the_platform_instruction_asks_for_a_single_call() {
        let text = assistant_instruction(true);
        assert!(
            !text.contains("once per image"),
            "这句会被读成「一张调一次」:{text}"
        );
        assert!(
            text.contains("in one send_message_to_user call"),
            "没说清一次调用发完整批:{text}"
        );
        assert!(
            text.contains("images takes the whole list"),
            "没说清 images 收的是整个清单:{text}"
        );
        assert!(
            text.contains("caption"),
            "没说清配文该放同一次调用里:{text}"
        );
    }

    /// 搜图不再自己显示任何东西:声明里不该还留着 preview 那一套。
    #[test]
    fn the_schema_no_longer_carries_preview_knobs() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::tools::tests::test_paths(temp.path());
        let mut registry = ToolRegistry::new();
        let mut config = AppConfig::default();
        config.plugins.web_images.enabled = true;
        register(&mut registry, config, paths, true);
        let schema = registry
            .get("search_web_images")
            .expect("搜图工具该注册")
            .parameters
            .to_string();
        for gone in ["preview", "preview_count"] {
            assert!(!schema.contains(gone), "schema 里还留着 {gone}:{schema}");
        }
        assert!(
            schema.contains("query") && schema.contains("count"),
            "{schema}"
        );
    }
}
