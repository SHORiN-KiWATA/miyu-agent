//! 候选图的排序与去重。
//!
//! 只用便宜的启发式打分（分辨率、标题匹配度、来源）。下载后的视觉审核 09-23
//! 撤掉了（用户拍板）：审核模型一失败就把整批图判成「不安全」，报出来的是
//! 「rejected by safety review」，真正的原因（免费视觉模型 429）被藏住了。

use crate::tools::web_images::*;

#[derive(Debug, Clone)]
pub(in crate::tools::web_images) struct ImageCandidate {
    pub(in crate::tools::web_images) title: String,
    pub(in crate::tools::web_images) page_url: String,
    pub(in crate::tools::web_images) image_url: String,
    pub(in crate::tools::web_images) thumbnail_url: String,
    pub(in crate::tools::web_images) source: String,
    pub(in crate::tools::web_images) width: u32,
    pub(in crate::tools::web_images) height: u32,
    pub(in crate::tools::web_images) search_description: String,
    pub(in crate::tools::web_images) provider_rank: usize,
}

pub(in crate::tools::web_images) struct ImageSearchResult {
    pub(in crate::tools::web_images) candidates: Vec<ImageCandidate>,
    pub(in crate::tools::web_images) diagnostics: Vec<Value>,
}

pub(in crate::tools::web_images) fn rank_candidates(
    query: &str,
    candidates: &mut [ImageCandidate],
) {
    candidates.sort_by(|left, right| {
        score_candidate(query, right)
            .partial_cmp(&score_candidate(query, left))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

pub(in crate::tools::web_images) fn score_candidate(
    query: &str,
    candidate: &ImageCandidate,
) -> f32 {
    let metadata = format!(
        "{} {} {}",
        candidate.title, candidate.page_url, candidate.image_url
    )
    .to_ascii_lowercase();
    let terms = image_query_terms(query);
    let mut title_matches = 0usize;
    let mut metadata_matches = 0usize;
    for term in &terms {
        if candidate.title.to_ascii_lowercase().contains(term) {
            title_matches += 1;
        } else if metadata.contains(term) {
            metadata_matches += 1;
        }
    }
    let denominator = terms.len().max(1) as f32;
    let mut score =
        title_matches as f32 / denominator * 48.0 + metadata_matches as f32 / denominator * 20.0;
    let compact_query = compact_search_text(query);
    let compact_title = compact_search_text(&candidate.title);
    if compact_query.len() >= 4 && compact_title.contains(&compact_query) {
        score += 20.0;
    }
    for number in numeric_query_terms(query) {
        if !contains_token(&metadata, &number) {
            score -= 45.0;
        }
    }
    let accessory_terms = [
        "手机壳",
        "保护壳",
        "保护套",
        "phone case",
        "模板",
        "素材",
        "贴膜",
    ];
    if accessory_terms.iter().any(|term| metadata.contains(term))
        && !accessory_terms.iter().any(|term| query.contains(term))
    {
        score -= 55.0;
    }
    score += 28.0 / (1.0 + candidate.provider_rank.saturating_sub(1) as f32 * 0.22);
    let short = candidate.width.min(candidate.height);
    let area = candidate.width.saturating_mul(candidate.height);
    score += if short >= 900 {
        16.0
    } else if short >= 600 {
        13.0
    } else if short >= 300 {
        9.0
    } else if short >= 100 {
        2.0
    } else {
        -4.0
    };
    if area >= 1_000_000 {
        score += 4.0;
    }
    let noisy = [
        "thumb",
        "thumbnail",
        "sprite",
        "placeholder",
        "banner",
        "advert",
        "favicon",
    ];
    if noisy.iter().any(|term| metadata.contains(term)) {
        score -= 8.0;
    }
    if metadata.contains("avatar")
        && !query.contains("头像")
        && !query.to_ascii_lowercase().contains("avatar")
    {
        score -= 8.0;
    }
    score
}

pub(in crate::tools::web_images) fn image_query_terms(query: &str) -> Vec<String> {
    let generic = [
        "图片",
        "照片",
        "高清",
        "壁纸",
        "photo",
        "image",
        "images",
        "picture",
        "wallpaper",
        "hd",
        "4k",
    ];
    let mut terms = query
        .split(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .map(|term| term.trim().to_ascii_lowercase())
        .filter(|term| term.len() >= 2 && !generic.contains(&term.as_str()))
        .collect::<Vec<_>>();
    for chunk in query
        .split(|ch: char| !is_cjk(ch))
        .filter(|chunk| chunk.chars().count() >= 4)
    {
        let chars = chunk.chars().collect::<Vec<_>>();
        for window in chars.windows(2) {
            terms.push(window.iter().collect::<String>());
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

pub(in crate::tools::web_images) fn numeric_query_terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

pub(in crate::tools::web_images) fn contains_token(metadata: &str, token: &str) -> bool {
    metadata
        .split(|ch: char| !ch.is_ascii_digit())
        .any(|value| value == token)
}

pub(in crate::tools::web_images) fn compact_search_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric() || is_cjk(*ch))
        .flat_map(char::to_lowercase)
        .collect()
}

pub(in crate::tools::web_images) fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff)
}

pub(in crate::tools::web_images) fn dedupe_candidates(
    candidates: Vec<ImageCandidate>,
) -> Vec<ImageCandidate> {
    let mut seen_images = HashSet::new();
    let mut seen_pages = HashSet::new();
    let mut deduped = Vec::new();
    for candidate in candidates {
        let key = candidate
            .image_url
            .split('?')
            .next()
            .unwrap_or(&candidate.image_url)
            .to_ascii_lowercase();
        let page_key = format!(
            "{}|{}",
            candidate
                .page_url
                .split('?')
                .next()
                .unwrap_or(&candidate.page_url)
                .to_ascii_lowercase(),
            compact_search_text(&candidate.title)
        );
        if seen_images.contains(&key) || seen_pages.contains(&page_key) {
            continue;
        }
        seen_images.insert(key);
        seen_pages.insert(page_key);
        deduped.push(candidate);
    }
    deduped
}

pub(in crate::tools::web_images) fn image_candidate_pool_limit(count: usize) -> usize {
    count.max((count * 4).max(count + 8).min(30))
}

pub(in crate::tools::web_images) fn image_download_probe_limit(count: usize) -> usize {
    count.max((count * 4).max(count + 6).min(16))
}

pub(in crate::tools::web_images) fn candidate_json(candidate: ImageCandidate) -> Value {
    json!({
        "title": candidate.title,
        "page_url": candidate.page_url,
        "image_url": candidate.image_url,
        "thumbnail_url": candidate.thumbnail_url,
        "source": candidate.source,
        "provider_rank": candidate.provider_rank,
        "width": candidate.width,
        "height": candidate.height,
        "search_description": candidate.search_description,
    })
}

pub(in crate::tools::web_images) fn stored_json(item: StoredImage) -> Value {
    json!({
        "title": item.candidate.title,
        "page_url": item.candidate.page_url,
        "image_url": item.candidate.image_url,
        "thumbnail_url": item.candidate.thumbnail_url,
        "source": item.candidate.source,
        "local_path": item.local_path,
        "mime_type": item.mime_type,
        "width": item.candidate.width,
        "height": item.candidate.height,
        "size_bytes": item.size_bytes,
        "size_human": format_bytes(item.size_bytes),
        "sha256": item.sha256,
        "used_thumbnail": item.used_thumbnail,
        "search_description": item.candidate.search_description,
    })
}
