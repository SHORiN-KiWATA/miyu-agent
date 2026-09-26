//! 删会话，连同它名下的子代理树。
//!
//! 顺序是「先占住、再停、再拆、最后删」（09-25）。原来是先停回合、先拆子代理树，最后才去
//! 占这条会话：拆后台子代理会给父会话回一条「后台任务报告」，它当场起一轮把会话占住，最后
//! 那一步报 busy——回合被掐了、子代理全删了，会话本身还在，面板一声不响地关掉（用户实测：
//! 再按一次才真删掉，看着像会话「异常消失」）。现在一开始就占住：排队消息、后台报告、目标
//! 续轮这些新回合都认 `admin_blocks_session`，一个也进不来；占不住（别的管理操作正占着）
//! 就原样报错，什么都不碰。

use crate::web::*;

/// 删会话要的管理预约。和压缩那种（`reserve_admin_for_session`）不同，它不要求会话里没有
/// 回合——跑着的回合接下来就会被停掉；它只负责挡住**新**回合。
fn reserve_admin_for_deletion(
    manager: &Arc<Mutex<ManagerState>>,
    session_id: &str,
) -> std::result::Result<(), String> {
    let mut manager = manager.lock().unwrap();
    if manager.admin_busy {
        return Err(t(
            "another operation is in progress; try again in a moment",
            "正在进行别的管理操作，稍后再删",
        )
        .to_string());
    }
    manager.admin_busy = true;
    manager.admin_session = Some(session_id.to_string());
    Ok(())
}

/// 删 `record` 这条会话：停掉它的回合、拆掉子代理树、它是当前会话就先换到兜底会话，然后
/// 删行并清掉库外的东西。
pub(in crate::web) async fn delete_session_tree(
    state: &DaemonState,
    record: &miyu_core::state::SessionRecord,
) -> std::result::Result<(), String> {
    reserve_admin_for_deletion(&state.manager, &record.session_id)?;
    let deleted = delete_reserved(state, &record.session_id).await;
    release_admin(&state.manager);
    deleted?;
    // 这个会话钉住的思考档位（09-24）跟着会话走。
    miyu_core::llm::remove_session_thinking_variants(
        &session_variant_paths(state, &record.owner),
        &record.session_id,
    );
    // 库里的目标行随会话级联删除；进程内的 goal 状态（armed 等）也一起清，不然条目在内存里
    // 陪跑到进程退出。
    miyu_engine::tools::goal::forget_session(&record.session_id);
    state.events.publish(
        "session.deleted",
        json!({ "session_id": record.session_id }),
    );
    Ok(())
}

async fn delete_reserved(state: &DaemonState, session_id: &str) -> std::result::Result<(), String> {
    // 运行中的会话也能删：先替用户按停止，等 run 退场再删。停不下来就到此为止——这时候
    // 还什么都没拆。
    if state.manager.lock().unwrap().session_has_runs(session_id)
        && !stop_session_runs(state, session_id, std::time::Duration::from_secs(5)).await
    {
        return Err(t(
            "the running turn did not stop in time; nothing was deleted",
            "正在跑的这一轮没能及时停下，什么都没删",
        )
        .to_string());
    }
    // 子代理树先拆(09-18 会话化):停回合、停后台任务、收中转进程、删行。
    teardown_subagent_tree(state, session_id).await;
    if &*state.state_store.session_id() == session_id {
        let fallback = fallback_session_id(state, session_id)?;
        switch_session_via_actor_reserved(state, fallback).await?;
    }
    let deleted = state
        .stores
        .for_session(session_id)
        .delete_session(session_id)
        .map_err(|error| safe_error_message(&error));
    crate::web::forget_session_processes(session_id);
    state
        .manager
        .lock()
        .unwrap()
        .compact_requests
        .remove(session_id);
    deleted
}
