//! `log.rs` 的测试（09-24 从文件里挪出来：那份超过了 800 行的目标线）。

use super::*;
use miyu_engine::tools::subagent::protocol::from_marker;

/// 「准备执行」只有作为**末尾**那一条时才算数。
///
/// 参数是逐块流的，`tool_preparing` 每来一块就报一条——攒起来就是一屏的
/// 「准备执行」（用户 09-17 实测截图）。
#[test]
fn only_the_last_preparing_row_survives() {
    let markers = [
        "__subtool_preparing__run_command",
        "__subtool_preparing__run_command",
        "__subtool_preparing__run_command",
    ];
    let steps = steps_from_events(markers.iter().flat_map(|m| from_marker(m)), true);
    assert_eq!(steps.len(), 1, "准备执行攒了一堆: {steps:?}");
}

/// 工具那一步的耗时也从标记流里来。
///
/// 标记流里原来一点时间戳都没有（掐表的是写日志那一侧），于是订标记流的面板
/// 上每个工具都是光秃秃的 `运行命令 · 看看输出`。现在发的那一侧在结果里带上
/// `ms`，抬头拼成 `运行命令 · 1.2s · 看看输出`——名字后面、窥视前面，和主线
/// 一个次序（用户 09-17 点名的形状：`运行命令 · <计时> · <short_title>`）。
#[test]
fn a_tool_step_carries_its_elapsed_from_the_marker_stream() {
    let args = serde_json::json!({"command": "ls", "title": "看看输出"}).to_string();
    let result = serde_json::json!({
        "name": "run_command",
        "display": "运行命令",
        "args": args,
        "ok": true,
        "ms": 1_234,
        "output": "total 0",
    })
    .to_string();
    let steps = steps_from_events(
        std::iter::once(format!("__subtool_result__{result}")).flat_map(|m| from_marker(&m)),
        true,
    );
    assert_eq!(steps.len(), 1, "{steps:#?}");
    assert_eq!(
        steps[0].elapsed,
        Some(std::time::Duration::from_secs_f64(1.2)),
        "耗时没带过来: {steps:#?}"
    );
    assert!(
        steps[0].head.starts_with("运行命令 · 1.2s · 看看输出"),
        "抬头次序不对: {:?}",
        steps[0].head
    );
}

/// 命令那一步：抬头给 **title**，命令全文归正文。
///
/// 主线和前台浮层一直是这么写的，后台这条路却把命令全文当窥视塞进抬头——
/// 同一步在两块面板上长得不一样（用户 09-17 实测截图：「命令不对啊，正确的
/// 是这样的」「不是说没有单独搞一套吗，怎么还是不统一呢」）。
#[test]
fn a_command_step_puts_the_title_on_the_head_and_the_command_in_the_body() {
    let args = serde_json::json!({
        "command": "echo hi; ls -la /tmp",
        "title": "看看临时目录",
    })
    .to_string();
    let call = serde_json::json!({
        "name": "run_command",
        "display": "运行命令",
        "args": args,
    })
    .to_string();
    let steps = steps_from_events(
        std::iter::once(format!("__subtool_call__{call}")).flat_map(|m| from_marker(&m)),
        true,
    );
    assert_eq!(steps.len(), 1, "{steps:#?}");
    assert!(
        steps[0].head.contains("看看临时目录"),
        "抬头上不是 title: {:?}",
        steps[0].head
    );
    assert!(
        !steps[0].head.contains("echo hi"),
        "命令全文又跑到抬头上了: {:?}",
        steps[0].head
    );
    assert_eq!(
        steps[0].command_text().as_deref(),
        Some("echo hi; ls -la /tmp"),
        "正文里拿不到命令全文"
    );
    assert!(
        log_detail_body(&steps[0]).join("\n").contains("echo hi"),
        "点开看不到命令"
    );
}

/// 订标记流时：命令那一步露**全文**、点开有输出。原来全文取的是截成一行 80 字的
/// 主题，而标记流解码又把结果里的输出丢了——底下只一行加「..」，点开也是空的
/// （用户 09-24 截图）。读日志那条路一直有输出，所以走查碰巧读日志时看不出来。
#[test]
fn a_marker_fed_command_step_keeps_its_whole_command_and_output() {
    let command = format!(
        "WT=/home/someone/{}/worktree && grep -rn skills src; echo 尾巴在这儿",
        "很长的路径".repeat(8)
    );
    let args = serde_json::json!({"command": command, "title": "看看技能"}).to_string();
    let call =
        serde_json::json!({"name": "run_command", "display": "运行命令", "args": args}).to_string();
    let result = serde_json::json!({
        "name": "run_command",
        "display": "运行命令",
        "args": args,
        "ok": true,
        "ms": 1200,
        "output": "第一行输出\n第二行输出",
    })
    .to_string();
    let steps = steps_from_events(
        [
            format!("__subtool_call__{call}"),
            format!("__subtool_result__{result}"),
        ]
        .iter()
        .flat_map(|m| from_marker(m)),
        false,
    );
    let step = steps
        .iter()
        .find(|step| step.head.contains("看看技能"))
        .expect("那一步不见了");
    assert_eq!(
        step.command_tail().as_deref(),
        Some(command.as_str()),
        "命令不是全文"
    );
    let body = log_detail_body(step).join("\n");
    assert!(body.contains("尾巴在这儿"), "点开看不到命令全文: {body}");
    assert!(body.contains("第二行输出"), "点开看不到输出: {body}");
}

/// 编辑那一步要有 diff：抬头上 `+N -M`，点开是渲染好的 diff。
///
/// 子代理内层的编辑拿不到 `__patch_preview__` 的真 diff，只有调用参数里那份
/// 信封。抬头那半由写的那一侧拼进文本（两条路都有），正文那半靠标记流把参数
/// 带过来（用户 09-17：「子代理浮层的编辑文件没有 diff 信息」）。
#[test]
fn an_edit_step_shows_its_diff() {
    let patch = "*** Begin Patch\n*** Update File: /tmp/a.txt\n-旧的一行\n+新的一行\n+又一行\n*** End Patch";
    let args = serde_json::json!({ "patchText": patch }).to_string();
    let call = serde_json::json!({
        "name": "edit",
        "display": "编辑文件",
        "args": args,
    })
    .to_string();
    let steps = steps_from_events(
        std::iter::once(format!("__subtool_call__{call}")).flat_map(|m| from_marker(&m)),
        true,
    );
    assert_eq!(steps.len(), 1, "{steps:#?}");
    // 加减行数从文本里摘出来单存了——画的时候要上色（绿加红减），留在
    // 文本里就只能是白的。
    assert_eq!(steps[0].diff, Some((2, 1)), "没摘出加减行数: {steps:#?}");
    let body = log_detail_body(&steps[0]).join("\n");
    assert!(
        body.contains("新的一行") && body.contains("旧的一行"),
        "点开不是 diff: {body}"
    );
}

/// 收段的时候，卷进去的那些「准备执行」要扔掉。
///
/// 末尾那道 `retain` 只扫顶层，而收段是 `drain` 进收缩行的肚子里的——于是
/// 点开收缩行看到的是一串「准备执行」，而且它们 `kind == Tool`，`N tools`
/// 跟着虚高。真机日志实测：2 次工具报成 **17 tools**（用户 09-17 截图）。
#[test]
fn collapsing_a_segment_drops_the_preparing_rows() {
    let call =
        serde_json::json!({"name": "run_command", "display": "运行命令", "args": "{}"}).to_string();
    let result =
        serde_json::json!({"name": "run_command", "args": "{}", "ok": true, "output": "x"})
            .to_string();
    let mut markers = vec!["__subagent_reasoning__想一句".to_string()];
    // 参数逐块流：一次调用前面挂着一长串「准备执行」。
    for _ in 0..13 {
        markers.push("__subtool_preparing__run_command".to_string());
    }
    markers.push(format!("__subtool_call__{call}"));
    markers.push(format!("__subtool_result__{result}"));
    // 它开口说话 = 前面那一段收成 `Worked for …`。
    markers.push("__subagent_content__跑完了。".to_string());

    let steps = steps_from_events(markers.iter().flat_map(|m| from_marker(m)), true);
    let fold = steps
        .iter()
        .find(|step| step.kind == StepKind::Fold)
        .unwrap_or_else(|| panic!("这一段没收起来，测的就不是收段: {steps:#?}"));
    assert!(
        !fold.inner.iter().any(|step| step.preparing),
        "收缩行里卷进了「准备执行」: {:#?}",
        fold.inner
    );
    assert!(
        fold.head.contains("1 tool") && !fold.head.contains("14 tool"),
        "工具数被准备行撑虚了: {:?}",
        fold.head
    );
}

/// 真实顺序下也一样：每轮工具之前都有一串「准备执行」，只有最后那一条留得下。
#[test]
fn preparing_rows_from_earlier_rounds_are_dropped() {
    let call =
        serde_json::json!({"name": "run_command", "display": "运行命令", "args": "{}"}).to_string();
    let result =
        serde_json::json!({"name": "run_command", "args": "{}", "ok": true, "output": "x"})
            .to_string();
    let mut markers = Vec::new();
    for _ in 0..2 {
        markers.push("__subagent_reasoning__想一句".to_string());
        for _ in 0..4 {
            markers.push("__subtool_preparing__run_command".to_string());
        }
        markers.push(format!("__subtool_call__{call}"));
        markers.push(format!("__subtool_result__{result}"));
    }
    // 最后一轮的参数还在流：末尾又挂着一串。
    for _ in 0..4 {
        markers.push("__subtool_preparing__run_command".to_string());
    }
    let steps = steps_from_events(markers.iter().flat_map(|m| from_marker(m)), true);
    let preparing = steps.iter().filter(|step| step.preparing).count();
    assert_eq!(preparing, 1, "准备执行攒了一堆: {steps:#?}");
}

/// 标记流那条路也要有「已思考 · 1.2s」。
///
/// 标记流里原来一点时间信息都没有（写日志那侧是自己掐表的），而后台面板
/// 09-17 起优先订标记流——于是同一段思考，换条路看就没有耗时了。发的那一侧
/// 现在在段末补一条 `__subagent_reasoning_done__`，这儿把它盖到上一步上。
#[test]
fn the_marker_stream_carries_the_thought_elapsed() {
    let markers = [
        "__subagent_reasoning__先看一眼",
        "__subagent_reasoning__再决定怎么下手。",
        "__subagent_reasoning_done__1234",
    ];
    let steps = steps_from_events(
        markers.iter().flat_map(|message| from_marker(message)),
        true,
    );
    assert_eq!(steps.len(), 1, "逐 delta 的思考该并成一步: {steps:?}");
    assert_eq!(steps[0].kind, StepKind::Thought);
    assert_eq!(steps[0].head, "先看一眼再决定怎么下手。", "没粘回一段");
    assert_eq!(
        steps[0].elapsed,
        Some(std::time::Duration::from_millis(1234)),
        "段末那条耗时没盖上去"
    );
}
