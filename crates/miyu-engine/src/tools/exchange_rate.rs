use super::{ToolRegistry, ToolSpec};
use anyhow::{bail, Result};
use miyu_base::config::ExchangeRatePluginConfig;
pub(crate) use miyu_core::ledger::rates::fetch_rate;
use serde_json::{json, Value};

pub fn register(registry: &mut ToolRegistry, config: ExchangeRatePluginConfig) {
    registry.register(ToolSpec::new(
        "get_exchange_rate",
        "Query exchange rate between two currencies. Supports ISO codes such as USD/EUR/JPY and common Chinese names.",
        json!({
            "type": "object",
            "properties": {
                "base": { "type": "string", "description": "Base currency, e.g. USD or 美元." },
                "target": { "type": "string", "description": "Target currency, e.g. JPY or 日元." }
            },
            "required": ["base", "target"],
            "additionalProperties": false
        }),
        move |args| {
            let config = config.clone();
            async move { get_exchange_rate(args, config).await }
        },
    ));
}

async fn get_exchange_rate(args: Value, config: ExchangeRatePluginConfig) -> Result<String> {
    let base = currency_code(args.get("base").and_then(Value::as_str).unwrap_or_default());
    let target = currency_code(
        args.get("target")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if base.is_empty() || target.is_empty() {
        bail!("base and target are required");
    }
    let (rate, source) = fetch_rate(&base, &target, &config).await?;
    if source == "allratestoday" {
        // 这个源要求在展示汇率的地方附上可点的链接。
        return Ok(format!(
            "{base} 到 {target} 的汇率是: {rate}（ECB 参考汇率，来源 AllRatesToday: https://allratestoday.com）"
        ));
    }
    Ok(format!("{base} 到 {target} 的汇率是: {rate}"))
}

fn currency_code(value: &str) -> String {
    match value.trim().to_uppercase().as_str() {
        "美元" | "美金" => "USD".to_string(),
        "人民币" | "元" => "CNY".to_string(),
        "日元" => "JPY".to_string(),
        "欧元" => "EUR".to_string(),
        "英镑" => "GBP".to_string(),
        "港币" => "HKD".to_string(),
        "台币" | "新台币" => "TWD".to_string(),
        "韩元" => "KRW".to_string(),
        code => code.to_string(),
    }
}
