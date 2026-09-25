use super::protocol::format_mcp_result;
use super::*;
use serde_json::{json, Value};

#[test]
fn mcp_error_flag_survives_text_conversion() {
    let result = json!({
        "isError": true,
        "content": [{"type": "text", "text": "File does not exist."}]
    });
    let output = format_mcp_result(&result);
    let failure: Value = serde_json::from_str(&output).expect("MCP errors need a failure envelope");
    assert_eq!(failure["ok"], false);
    assert_eq!(failure["error"], "File does not exist.");
}

#[test]
fn mcp_error_flag_wins_over_success_shaped_text() {
    let result = json!({
        "isError": true,
        "content": [{"type": "text", "text": "{\"success\":true}"}]
    });
    let failure: Value = serde_json::from_str(&format_mcp_result(&result)).unwrap();
    assert_eq!(failure["ok"], false);
    assert!(failure.get("success").is_none());
    assert_eq!(failure["error"], "{\"success\":true}");
}

#[test]
fn mcp_error_without_text_preserves_its_payload() {
    for result in [
        json!({"isError": true, "content": []}),
        json!({"isError": true, "structuredContent": {"reason": "missing"}}),
        json!({"isError": true, "content": [
            {"type": "resource", "resource": {"uri": "test://error", "text": "missing"}}
        ]}),
    ] {
        let failure: Value = serde_json::from_str(&format_mcp_result(&result)).unwrap();
        assert_eq!(failure["ok"], false);
        assert!(!failure["error"].as_str().unwrap().is_empty());
    }
}

#[test]
fn successful_mcp_outputs_keep_their_existing_bytes() {
    for flag in [None, Some(false)] {
        let mut result = json!({"content":[
            {"type":"text","text":"hello"},
            {"type":"text","text":"world"}
        ]});
        if let Some(flag) = flag {
            result["isError"] = flag.into();
        }
        assert_eq!(format_mcp_result(&result), "hello\n\nworld");
    }
    let result = json!({"content":[], "structuredContent":{"answer":42}});
    assert_eq!(
        format_mcp_result(&result),
        serde_json::to_string_pretty(&result).unwrap()
    );
}

#[tokio::test]
async fn stdio_mcp_tool_failure_is_not_reported_as_success() {
    let script = r#"
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    if request['method'] == 'initialize':
        result = {'protocolVersion':'2025-03-26','capabilities':{},'serverInfo':{'name':'mock-error','version':'1'}}
    else:
        result = {'isError':True,'content':[{'type':'text','text':'Fixture rejected the operation.'}]}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#;
    let server = McpServerConfig {
        id: "mock-error".into(),
        command: "python3".into(),
        args: vec!["-c".into(), script.into()],
        timeout_seconds: 5,
        ..Default::default()
    };
    let output = call_tool(
        McpToolBinding {
            server,
            tool_name: "fail".into(),
        },
        json!({}),
    )
    .await
    .unwrap();
    let failure: Value =
        serde_json::from_str(&output).expect("stdio tool errors need a failure envelope");
    assert_eq!(failure["ok"], false);
    assert_eq!(failure["error"], "Fixture rejected the operation.");
}
