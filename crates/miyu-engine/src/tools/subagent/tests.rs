use super::*;

/// 中途的量报只刷标题和状态行，**不进流水账**。
///
/// 它一秒能来好几次（每调完一个工具报一次）。落进去的话，面板里那条时间线
/// 会被「统计」节点撑满，真正在干什么反而看不见了。
#[test]
fn running_metric_never_lands_in_the_job_log() {
    assert_eq!(
        readable_subagent_log_line_timed("__subagent_metric__1.2K\t工具调用 3 次", None),
        ""
    );
    // 跑完那一次照旧留底。
    assert_eq!(
        readable_subagent_log_line_timed("__subagent_stats__工具调用 3 次", None),
        "[统计] 工具调用 3 次"
    );
}

/// 结果那一行要把工具真吐出来的东西带上。
///
/// 只写一句「运行命令 ok · ls」的话，面板里那一步点开看到的还是同一句话
/// ——等于点开是空的（用户实测：浮层里这些工具展开都没内容）。
#[test]
fn tool_result_carries_its_output_into_the_log() {
    let json = serde_json::json!({
        "name": "run_command",
        "args": r#"{"command":"ls"}"#,
        "ok": true,
        "output": "total 12\n\ndrwxr-xr-x 2 shorin\n",
    })
    .to_string();
    let line = readable_subagent_log_line_timed(&format!("__subtool_result__{json}"), None);
    let mut lines = line.lines();
    assert!(
        lines
            .next()
            .unwrap_or_default()
            .starts_with("[结果] run_command\t"),
        "{line}"
    );
    assert_eq!(lines.next(), Some("[输出] total 12"), "{line}");
    // 空行不占一条记录。
    assert_eq!(lines.next(), Some("[输出] drwxr-xr-x 2 shorin"), "{line}");
    assert_eq!(lines.next(), None, "{line}");
}

/// 正文也是**逐 delta** 来的，得攒成段落再落盘。
///
/// 一条一行的话日志会变成每行一个词的字符梯（用户实测截图：整屏
/// `[正文] the` / `[正文] and`）。
#[test]
fn streamed_speech_is_batched_into_paragraphs() {
    let mut speech = super::log::StreamBuffer::new("[正文]", "[正文+]");
    let mut lines = Vec::new();
    for chunk in ["Now ", "let ", "me ", "enumerate."] {
        accumulate_stream(&mut speech, chunk, &mut lines);
    }
    assert!(lines.is_empty(), "还没到段落就落盘了: {lines:?}");
    flush_stream_buffer(&mut speech, &mut lines);
    assert_eq!(lines, vec!["[正文] Now let me enumerate.".to_string()]);
    // 空行就是段落分隔，到了就落一条。段里的换行原样留着——读那侧按续行拼。
    let mut lines = Vec::new();
    accumulate_stream(&mut speech, "第一段\n\n第二段", &mut lines);
    assert_eq!(lines, vec!["[正文] 第一段\n\n".to_string()]);
    flush_stream_buffer(&mut speech, &mut lines);
    assert_eq!(lines[1], "[正文] 第二段");
}

/// 一段被时间闸切开的话，后半截写成 `[正文+]`：读那侧据此**粘回去**，而不是
/// 另起一行（用户 09-17：「浮层的正文现在是每个 token 都会换一次行」）。
#[test]
fn a_paragraph_cut_in_half_marks_the_second_piece_as_a_continuation() {
    let mut speech = super::log::StreamBuffer::new("[正文]", "[正文+]");
    let mut lines = Vec::new();
    accumulate_stream(&mut speech, "the quick ", &mut lines);
    // 攒够久了，这一截先落——切在 `quick ` 和 `brown` 中间。
    speech.age(super::log::STREAM_FLUSH_INTERVAL);
    accumulate_stream(&mut speech, "brown ", &mut lines);
    accumulate_stream(&mut speech, "fox", &mut lines);
    flush_stream_buffer(&mut speech, &mut lines);
    // 两头的空白**不掐**：它们就是词边界，掐了就粘成 `the quick brownfox`。
    assert_eq!(
        lines,
        vec![
            "[正文] the quick brown ".to_string(),
            "[正文+] fox".to_string(),
        ]
    );
    // 读那侧照 `continues` 拼回去，要和模型说的一模一样。
    let joined = lines
        .iter()
        .map(|line| {
            line.strip_prefix("[正文+] ")
                .or_else(|| line.strip_prefix("[正文] "))
                .unwrap_or_default()
        })
        .collect::<String>();
    assert_eq!(joined, "the quick brown fox");
}

/// 攒够久也要落一条——别让后台面板干等。
///
/// 原来只有「空行」和「600 字符」两道闸。实测（`testkit/tui/bg_latency.py`）模型
/// 想一大段不带空行的时候，面板整整 **12.0 秒**不动一下。
#[test]
fn a_long_paragraph_still_lands_before_it_finishes() {
    let mut thinking = super::log::StreamBuffer::new("[思考]", "[思考+]");
    let mut lines = Vec::new();
    // 刚起头的那一段不会一个 delta 一条：计时从这一段的第一块算起。
    accumulate_stream(&mut thinking, "想到", &mut lines);
    assert!(
        lines.is_empty(),
        "刚起头就落盘 = 每个 delta 一行: {lines:?}"
    );
    // 假装这一段已经攒够久了（真实调用里是模型慢慢吐出来的）。
    thinking.age(super::log::STREAM_FLUSH_INTERVAL);
    accumulate_stream(&mut thinking, "一半", &mut lines);
    assert_eq!(
        lines,
        vec!["[思考] 想到一半".to_string()],
        "攒够久了还不落，面板就得干等"
    );
    assert!(thinking.is_empty(), "落过之后缓冲要清干净");

    // 落过之后重新计时：下一块不会立刻再落一条。
    let mut lines = Vec::new();
    accumulate_stream(&mut thinking, "接着想", &mut lines);
    assert!(lines.is_empty(), "刚落过又落 = 每个 delta 一行: {lines:?}");
}

/// 工具吐的原始输出要洗干净再进流水账。
///
/// 转义序列、回车、制表符原样写进去的话，面板按纯文本算宽度，算出来的和
/// 真实占宽对不上，右边那根竖线跟着参差不齐。
#[test]
fn tool_output_is_plain_text_in_the_log() {
    let json = serde_json::json!({
        "name": "run_command",
        "args": "{}",
        "ok": true,
        "output": "\u{1b}[31m红的\u{1b}[0m\ta\u{7}b\r\n干净一行\n",
    })
    .to_string();
    let line = readable_subagent_log_line_timed(&format!("__subtool_result__{json}"), None);
    let outputs = line
        .lines()
        .filter_map(|line| line.strip_prefix("[输出] "))
        .collect::<Vec<_>>();
    assert_eq!(outputs, vec!["红的 ab", "干净一行"], "{line}");
    // `[结果]` 那一行自己带一个制表符（工具 id 的分隔），只看输出那几行。
    assert!(
        outputs
            .iter()
            .all(|line| !line.contains(|ch: char| ch.is_control())),
        "{line}"
    );
}

/// 差事写在流水账开头，换行折成 `\u{1}`（面板那边再拆回来）。
#[test]
fn prompt_header_folds_newlines() {
    let dir = std::env::temp_dir().join(format!("miyu-prompt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建目录");
    let path = dir.join("job.log");
    write_subagent_prompt_header(&path, "  第一行\n第二行  ");
    let text = std::fs::read_to_string(&path).expect("读日志");
    assert_eq!(text, "[提示] 第一行\u{1}第二行\n");
    // 空差事不写。
    let empty = dir.join("empty.log");
    write_subagent_prompt_header(&empty, "   ");
    assert!(!empty.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

fn test_paths(root: &std::path::Path) -> MiyuPaths {
    crate::tools::tests::test_paths(root)
}

#[test]
fn dev_flag_defaults_to_off_and_parses() {
    let base = json!({"description": "d", "prompt": "p"});
    assert!(!parse_params(&base).unwrap().dev);
    let mut with_dev = base.clone();
    with_dev["dev"] = json!(true);
    assert!(parse_params(&with_dev).unwrap().dev);
}

/// dev 子代理的系统提示词是三段拼起来的,少任何一段它都得先浪费一轮
/// 去问「我在哪、说给谁听」。
#[test]
fn dev_system_prompt_carries_the_dev_prompt_host_block_and_contract() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    // 09-24 起开发模式提示词默认为空:没写就从环境块开始,开头不空行。
    let prompt = build_dev_system_prompt(&config, &paths).unwrap();
    assert!(prompt.starts_with("<host-environment"), "{prompt}");
    assert!(prompt.contains("<runtime cwd="), "{prompt}");
    assert!(prompt.ends_with(SUBAGENT_DEV_CONTRACT), "{prompt}");

    // 用户写了就放在最前面,和环境块之间空一行。
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let dev_prompt = paths.config_dir.join(miyu_base::config::DEV_PROMPT_FILE);
    std::fs::write(&dev_prompt, "你是资深前端工程师\n").unwrap();
    let prompt = build_dev_system_prompt(&config, &paths).unwrap();
    assert!(
        prompt.starts_with("你是资深前端工程师\n\n<host-environment"),
        "{prompt}"
    );
}

/// 同一个会话里连开两个 dev 子代理,系统提示词必须逐字节相同——不然
/// 每一个都是一次冷前缀。
#[test]
fn dev_system_prompt_is_byte_stable_within_a_session() {
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths(temp.path());
    let config = AppConfig::default();
    assert_eq!(
        build_dev_system_prompt(&config, &paths).unwrap(),
        build_dev_system_prompt(&config, &paths).unwrap()
    );
}

/// 递归防护:dev 子代理拿的是 dev 会话那张面,而那张面里也注册着
/// `subagent`——排除表必须认得新名,否则子代理能自己再开子代理。
#[test]
fn subagent_excludes_itself_by_its_current_name() {
    assert!(SUBAGENT_EXCLUDED.contains(&"subagent"));
}

/// 后台子代理起在 `tokio::spawn` 的新任务上,回合的 task-local 到那儿
/// 全空了:成员的后台子代理会因此跑在 Landlock 之外,工具的工作目录
/// 也退回 daemon 的 cwd。这条钉住「抓下来再套回去」。
#[tokio::test]
async fn background_scope_is_carried_across_the_spawn() {
    let workspace = std::path::PathBuf::from("/tmp/miyu-subagent-scope");
    let session: std::sync::Arc<str> = "sess_probe".into();
    let (bare, restored) = miyu_base::workspace::with_workspace(
        workspace.clone(),
        miyu_base::workspace::with_session(session.clone(), async {
            let carried_workspace = miyu_base::workspace::try_workspace();
            let carried_session = miyu_base::workspace::try_session();
            tokio::spawn(async move {
                let bare = (
                    miyu_base::workspace::try_workspace(),
                    miyu_base::workspace::try_session(),
                );
                let restored = with_turn_scope(None, carried_workspace, carried_session, async {
                    (
                        miyu_base::workspace::try_workspace(),
                        miyu_base::workspace::try_session(),
                    )
                })
                .await;
                (bare, restored)
            })
            .await
            .unwrap()
        }),
    )
    .await;
    assert_eq!(bare, (None, None), "裸 spawn 本就看不见回合作用域");
    assert_eq!(restored, (Some(workspace), Some(session)));
}

/// 写日志这一侧只会写 `LOG_TAGS` 里的标签——清单与实际输出对得上。
///
/// 读的那一侧按标签分支，认不出的行会掉进「无标签续行」。所以这张清单不能
/// 靠人记：把每一种标记喂进去，看它吐出来的每一行到底挂着什么标签。
#[test]
fn every_marker_writes_a_tag_from_the_list() {
    let result = serde_json::json!({
        "name": "run_command",
        "args": "{}",
        "ok": true,
        "output": "total 0",
    })
    .to_string();
    let call = serde_json::json!({"name": "run_command", "args": "{}"}).to_string();
    let brief = serde_json::json!({"prompt": "去看看那个目录里有什么"}).to_string();
    // 逐个点名 `INNER_MARKERS`：手写一份标记清单本身就是漂移的来源，这里对
    // 着协议那一份走——将来加了新标记而这边没给样例，`payload` 当场 panic。
    let payload = |marker: &str| match marker {
        "__subagent_brief__" => brief.clone(),
        "__subagent_reasoning__" => "先列一下".to_string(),
        "__subagent_content__" => "里面是空的。".to_string(),
        "__subagent_metric__" => "工具调用 3 次".to_string(),
        super::protocol::REASONING_DONE_MARKER => "1234".to_string(),
        "__subagent_stats__" => "词元 1234".to_string(),
        "__subtool_preparing__" => "run_command".to_string(),
        "__subtool_call__" => call.clone(),
        "__subtool_result__" => result.clone(),
        other => panic!("{other} 是新标记，给它补个样例"),
    };
    let mut seen = std::collections::BTreeSet::new();
    // 写日志有**两个入口**：逐 delta 的原始标记走 `readable_subagent_log_line_timed`
    // （写 `+` 标签，一条是一截），攒成段落的走 `accumulate_stream`（写不带 `+`
    // 的，一条是一行）。两个都得覆盖，否则清单里会剩下"没人写"的标签。
    for (tag, continued) in [("[思考]", "[思考+]"), ("[正文]", "[正文+]")] {
        let mut stream = super::log::StreamBuffer::new(tag, continued);
        let mut lines = Vec::new();
        super::log::accumulate_stream(&mut stream, "一段话", &mut lines);
        super::log::flush_stream_buffer(&mut stream, &mut lines);
        for line in &lines {
            seen.insert(
                *super::protocol::LOG_TAGS
                    .iter()
                    .find(|tag| line.starts_with(**tag))
                    .unwrap_or_else(|| panic!("{line:?} 的标签不在 LOG_TAGS 里")),
            );
        }
    }
    for marker in super::protocol::INNER_MARKERS {
        let message = format!("{marker}{}", payload(marker));
        let written = readable_subagent_log_line_timed(&message, Some(Duration::from_millis(400)));
        // 这两条**故意**不进流水账：
        //   - 中途量报：每调一次工具记一条的话，面板的时间线会被这些节点撑满
        //     （跑完那次的 `__subagent_stats__` 照旧留一行）；
        //   - 段末耗时：日志那条路自己掐表（`stamp_thought_lines`），两边都报
        //     的话同一段的时间会被加两遍。
        if *marker == "__subagent_metric__" || *marker == super::protocol::REASONING_DONE_MARKER {
            assert!(written.is_empty(), "{marker} 不该进流水账: {written:?}");
            continue;
        }
        assert!(!written.is_empty(), "{marker} 什么都没写");
        assert!(
            !written.contains(marker),
            "{marker} 没被认出来，原样写进流水账了: {written:?}"
        );
        for line in written.lines() {
            let tag = super::protocol::LOG_TAGS
                .iter()
                .find(|tag| line.starts_with(**tag))
                .unwrap_or_else(|| panic!("{line:?} 的标签不在 LOG_TAGS 里（{marker}）"));
            seen.insert(*tag);
        }
    }
    // 反过来也要对上：清单里挂着一个谁都不写的标签，等于给读的那一侧留了一条
    // 死分支，手写样本时还会照着它造出不存在的格式。
    let missing: Vec<_> = super::protocol::LOG_TAGS
        .iter()
        .filter(|tag| !seen.contains(**tag))
        .collect();
    assert!(missing.is_empty(), "清单里这些标签没人写: {missing:?}");
}

/// 界面从子代理的结果里认出子会话（会话项目第 3 段）：回放时时间线上那一行靠它链到
/// 子会话。认的就是 `format_child_outcome` 写出来的那几种形状。
#[test]
fn the_child_session_is_read_back_from_the_tool_output() {
    let finished = |state: &str| {
        format_child_outcome(
            "查日志",
            ModelTier::Standard,
            miyu_base::host_ports::ChildOutcome::Finished(miyu_base::host_ports::ChildTaskResult {
                session_id: "sess_child1".to_string(),
                state: state.to_string(),
                final_text: "查完了 (session 在正文里也不算)".to_string(),
                turns: 1,
                total_tokens: 10,
                provider_id: None,
                model: None,
            }),
        )
        .unwrap()
    };
    assert_eq!(
        subagent_session_of_output(&finished("done")).as_deref(),
        Some("sess_child1")
    );
    assert_eq!(
        subagent_session_of_output(&finished("interrupted")).as_deref(),
        Some("sess_child1")
    );
    let queued = format_child_outcome(
        "查日志",
        ModelTier::Standard,
        miyu_base::host_ports::ChildOutcome::Queued {
            session_id: "sess_child2".to_string(),
        },
    )
    .unwrap();
    assert_eq!(
        subagent_session_of_output(&queued).as_deref(),
        Some("sess_child2")
    );
    // 后台刚派出去：结果里只有任务 id，子会话还没建。
    assert_eq!(
        subagent_session_of_output(r#"{"ok":true,"kind":"background_subagent","job_id":"a1b2c3"}"#),
        None
    );
    assert_eq!(subagent_session_of_output("some other tool output"), None);
}
