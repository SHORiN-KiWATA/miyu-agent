//! 懒加载：工具目录里放桩，用到了才装真的。
//!
//! 目的是省 token——全部工具的完整描述能占掉几万 token，而一轮里真正会用的只有
//! 几个。桩（`stub_definition`）只留名字和一句话，模型要用时先「加载」再调用。
//!
//! `load_target_summary` / `load_target_tool_xml` 生成加载目标的清单。这些文本
//! **进提示词前缀**，所以生成必须是确定性的（见 AGENTS.md 1.1）。

use crate::tools::registry::*;

/// 按工具自己的 schema 还原被写成「JSON 字符串」的数组/对象参数。
///
/// 模型经常把结构化参数序列化成字符串再传，实测同一天踩到三次：
/// `reference_images` 传成 `"[\"/path.png\"]"`、`todos` 传成
/// `"[{\"content\":…}]"`、`updates` 同理。stub 加载模式尤其容易——模型看到
/// 的只有一句摘要和宽松参数壳，没取契约就调用时只能猜形状。
///
/// 逐个工具打补丁治标不治本，所以放在这里做一次：只动 schema 明确声明为
/// array/object 的顶层参数，且只在给来的字符串确实能解析成那个形状时才换。
/// 声明成 string 的参数一个字节都不碰——`run_command.command` 里恰好是一段
/// JSON 的情况必须原样透传。
pub(crate) fn coerce_declared_shapes(parameters: &Value, args: &mut Value) {
    let Some(properties) = parameters.get("properties").and_then(Value::as_object) else {
        return;
    };
    let Some(object) = args.as_object_mut() else {
        return;
    };
    for (name, schema) in properties {
        let Some(declared) = schema.get("type").and_then(Value::as_str) else {
            continue;
        };
        let Some(Value::String(text)) = object.get(name) else {
            continue;
        };
        let text = text.trim();
        let restored = match declared {
            "array" if text.starts_with('[') => serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_array),
            "object" if text.starts_with('{') => serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_object),
            // 数字/布尔被写成字符串同样常见:实测
            // `"start_line": "1"` 让 edit_knowledge_base_file 一直报
            // "start_line is required"。
            "integer" => text.parse::<i64>().ok().map(Value::from),
            "number" => text.parse::<f64>().ok().map(Value::from),
            // 大小写都收:实测模型发过 Python 风格的 "False"。
            "boolean" => match text.to_ascii_lowercase().as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
            _ => None,
        };
        if let Some(restored) = restored {
            object.insert(name.clone(), restored);
        }
    }
}

pub(crate) fn stub_definition(tool: &ToolSpec) -> ToolDefinition {
    let summary = load_target_summary(&tool.description);
    // 后缀只留标记:整套"先 load_tools 取契约再调用"的说明在 load_tools
    // 自己的 description 里统一给一次,N 个 stub 各背一遍纯属重复计费
    // (验收 08-16:每条省 ~55B)。
    //
    // 例外是 stub_example:参数壳是空的,模型没取契约就调用时只能猜字段名,
    // 声明了示例的工具在这里补上。示例并进同一对括号,不额外占一对。
    // "stub" 而非 "stub entry":entry 一词零信息,35 条 stub 每请求各省 1 tok
    // (08-21 token-diet)。
    let note = match &tool.stub_example {
        Some(example) => format!("stub, e.g. {example}"),
        None => "stub".to_string(),
    };
    let description = if summary.is_empty() {
        format!("({note}: fetch the full contract via load_tools)")
    } else {
        format!("{summary} ({note})")
    };
    ToolDefinition {
        kind: "function",
        function: miyu_core::llm::FunctionDefinition {
            name: tool.name.clone(),
            description,
            // Permissive shell: real parameters go at the top level exactly as
            // in a normal call, so execution needs no unwrapping; the actual
            // contract arrives via the load_tools result.
            //
            // 裸 {"type":"object"} 与 {"type":"object","properties":{},
            // "additionalProperties":true} 在 JSON Schema 下完全等价——后者
            // 只是把两个默认值显式写了一遍,每条 stub 白占 48 字符
            // (08-17 实测:57 条 stub 共 2,464 字符)。
            parameters: serde_json::json!({ "type": "object" }),
        },
    }
}

/// Upper bound for a stub / catalog one-line summary, in characters.
///
/// 摘要是模型判断「要不要 load 这个工具」的唯一依据,不是契约——真正的约束
/// (参数、前置条件、互斥规则)都在完整契约里,而模型必须先取契约才能调用。
/// 所以摘要只需回答"这是不是我要的那个工具"。
pub(crate) const SUMMARY_MAX_CHARS: usize = 60;

/// Catalog entries carry a bounded one-line summary instead of the full tool
/// description. This keeps the loader catalog small and byte-stable: without
/// the bound, `load_skill`'s description (which embedded the whole skills
/// catalog until 09-25) was nested wholesale into the loader XML and
/// re-rendered on every skills rescan.
pub(crate) fn load_target_summary(description: &str) -> String {
    let first_line = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if first_line.chars().count() <= SUMMARY_MAX_CHARS {
        return first_line.to_string();
    }
    // 优先在预算内的句末断开,断不出来才硬截。半句话比整句更容易让模型
    // 误判用途,而句末断开读起来仍是一句完整的摘要。
    let window: Vec<char> = first_line.chars().take(SUMMARY_MAX_CHARS).collect();
    let sentence_end = window
        .iter()
        .enumerate()
        .rev()
        .find(|(index, ch)| {
            matches!(ch, '。' | '；' | '！' | '？')
                // 英文句点只在后面跟空格时算句末,免得切断 "0.5" 或 "e.g."。
                || (**ch == '.' && window.get(index + 1) == Some(&' '))
        })
        .map(|(index, _)| index);
    if let Some(index) = sentence_end {
        // 句末太靠前就别snap:一句"好。"当摘要还不如半句话。四分之一预算
        // 是经验下限,足以放下"按文字提示生成图片，返回本地路径。"这种。
        if index + 1 >= SUMMARY_MAX_CHARS / 4 {
            return window[..=index].iter().collect();
        }
    }
    let mut summary: String = window.iter().collect();
    summary.push('…');
    summary
}

/// 经典两行 DP 编辑距离,只服务 suggest_similar 的小字符串,不引依赖。
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ch_a) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, ch_b) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ch_a != ch_b);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(description: &str) -> ToolSpec {
        ToolSpec::new("t", description, json!({"type": "object"}), |_| async {
            Ok(String::new())
        })
    }

    #[test]
    fn stub_without_example_is_unchanged() {
        let stub = stub_definition(&spec("查询某个游戏在 Linux 上的兼容性。"));
        assert_eq!(
            stub.function.description,
            "查询某个游戏在 Linux 上的兼容性。 (stub)"
        );
    }

    #[test]
    fn stub_example_joins_the_existing_parenthetical() {
        // 示例并进同一对括号,不额外占一对。
        let stub = stub_definition(
            &spec("查询某个游戏在 Linux 上的兼容性。")
                .with_stub_example(r#"{"query":"艾尔登法环"}"#),
        );
        assert_eq!(
            stub.function.description,
            r#"查询某个游戏在 Linux 上的兼容性。 (stub, e.g. {"query":"艾尔登法环"})"#
        );
    }

    #[test]
    fn stub_example_survives_an_empty_summary() {
        let stub = spec("").with_stub_example(r#"{"query":"x"}"#);
        assert_eq!(
            stub_definition(&stub).function.description,
            r#"(stub, e.g. {"query":"x"}: fetch the full contract via load_tools)"#
        );
    }
}

#[cfg(any(test, feature = "testkit"))]
mod test_support;
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub use test_support::*;
