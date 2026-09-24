//! 网页要的一页回合：回合本身，挂上图片、artifact、生成速度；按页取的时候再带上
//! 往前翻的游标、前面那些回合的用量合计、会话第一句。
//!
//! 会话回合接口和首屏快照（bootstrap）共用这一份。会话项目第 2 段起按页取：以前
//! 两处都借模型那条整段读的路，最重的会话打开一次要解析约 3.5 MB、发出约 3 MB。

use crate::web::*;

/// 网页首屏、切会话时取多少轮。往上翻到顶再补。
pub(in crate::web) const WEB_TURN_PAGE: usize = 30;

/// 用量合计。网页按页取回合时，从这里接着算「会话到这一轮为止累计多少」。
#[derive(Serialize, Default)]
pub(in crate::web) struct TokenBase {
    pub(in crate::web) total: u64,
    pub(in crate::web) prompt: u64,
    pub(in crate::web) cache_read: u64,
}

pub(in crate::web) struct SafeTurnPage {
    pub(in crate::web) turns: Vec<SafeTurn>,
    /// 更早的回合还有：下一页把它当 `before` 往前取。
    pub(in crate::web) older: Option<i64>,
    pub(in crate::web) tokens_before: TokenBase,
    /// 会话里用户说的第一句：按页取时第一轮不一定在手上，网页拿它当标题。
    pub(in crate::web) first_user_content: Option<String>,
}

/// `store` 要钉在 `session_id` 上（图片、artifact 按它查）。`limit` 为 None 就整段
/// 取：刷新之前就开着的老页面还是这么取。
pub(in crate::web) fn safe_turn_page(
    store: &StateStore,
    session_id: &str,
    before: Option<i64>,
    limit: Option<usize>,
) -> Result<SafeTurnPage> {
    let mut assets_by_turn = HashMap::<String, Vec<ImageAsset>>::new();
    for asset in store.load_image_assets()? {
        assets_by_turn
            .entry(asset.turn_id.clone())
            .or_default()
            .push(asset);
    }
    let mut artifacts_by_turn = HashMap::<String, Vec<ArtifactAsset>>::new();
    for artifact in store.load_artifact_assets()? {
        artifacts_by_turn
            .entry(artifact.turn_id.clone())
            .or_default()
            .push(artifact);
    }
    let generation_by_turn = store.load_turn_generation(session_id)?;
    let (turns, older, tokens_before, first_user_content) = match limit {
        Some(limit) => {
            let page = store.turn_page(before, limit.clamp(1, 200))?;
            let base = TokenBase {
                total: page.tokens_before.total,
                prompt: page.tokens_before.prompt,
                cache_read: page.tokens_before.cache_read,
            };
            (page.turns, page.older, base, store.first_user_content()?)
        }
        None => (
            store
                .load_turns()?
                .into_iter()
                .filter(|turn| !turn.is_summary)
                .collect(),
            None,
            TokenBase::default(),
            None,
        ),
    };
    let turns = turns
        .into_iter()
        .map(|turn| {
            let assets = assets_by_turn.remove(&turn.turn_id).unwrap_or_default();
            let artifacts = artifacts_by_turn.remove(&turn.turn_id).unwrap_or_default();
            let mut safe = SafeTurn::from_turn(turn, assets, artifacts);
            if let Some((tokens, millis)) = generation_by_turn.get(&safe.id) {
                safe.generation_tokens = *tokens;
                safe.generation_ms = *millis;
            }
            safe
        })
        .collect();
    Ok(SafeTurnPage {
        turns,
        older,
        tokens_before,
        first_user_content,
    })
}
