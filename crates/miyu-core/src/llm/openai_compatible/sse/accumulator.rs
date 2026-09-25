//! 工具调用的分片累积。
//!
//! 三套协议各有一个累积器，因为分片的**标识方式不同**：Chat 按数组下标，
//! Responses 按 output item id，Anthropic 按 content block 序号。共用一个实现
//! 就得在里面塞三种分支。
//!
//! `Utf8LineBuffer` / `SseDataBuffer` 管字节层：一个 UTF-8 字符可能被切在两个
//! TCP 包之间，直接 `from_utf8_lossy` 会让中文流式输出随机冒出「�」。
//!
//! `MAX_STREAM_*` 是防御性上限——流的内容由对端决定。

use crate::llm::openai_compatible::*;

/// Upper bound on streamed tool calls per response. Indices come from the
/// upstream stream verbatim; without a cap a single malformed chunk (e.g.
/// index 2^30) makes the accumulator allocate gigabytes. Chunks addressing
/// an index beyond the cap are dropped.
pub(in crate::llm::openai_compatible) const MAX_STREAM_TOOL_CALLS: usize = 128;

pub(in crate::llm::openai_compatible) const MAX_STREAM_TOOL_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;

pub(in crate::llm::openai_compatible) const MAX_STREAM_LINE_BYTES: usize = 4 * 1024 * 1024;

pub(in crate::llm::openai_compatible) const MAX_STREAM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

pub(in crate::llm::openai_compatible) fn append_bounded(
    target: &mut String,
    text: &str,
    limit: usize,
) {
    let remaining = limit.saturating_sub(target.len());
    if remaining == 0 {
        return;
    }
    let mut end = text.len().min(remaining);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
}

pub(in crate::llm::openai_compatible) fn bounded_stream_string(
    mut value: String,
    limit: usize,
) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

#[derive(Debug, Default)]
pub(in crate::llm::openai_compatible) struct AnthropicToolAccumulator {
    pub(in crate::llm::openai_compatible) calls: Vec<PartialToolCall>,
}

impl AnthropicToolAccumulator {
    pub fn start(&mut self, index: usize, block: AnthropicStreamBlock) -> Option<String> {
        if index >= MAX_STREAM_TOOL_CALLS {
            return None;
        }
        while self.calls.len() <= index {
            self.calls.push(PartialToolCall::default());
        }
        let call = &mut self.calls[index];
        call.id = block.id.unwrap_or_else(|| format!("tool-{index}"));
        call.kind = "function".to_string();
        call.name = block.name.unwrap_or_default();
        (!call.name.is_empty()).then(|| call.name.clone())
    }

    pub(in crate::llm::openai_compatible) fn append_arguments(
        &mut self,
        index: usize,
        text: String,
    ) {
        if index >= MAX_STREAM_TOOL_CALLS {
            return;
        }
        while self.calls.len() <= index {
            self.calls.push(PartialToolCall::default());
        }
        append_bounded(
            &mut self.calls[index].arguments,
            &text,
            MAX_STREAM_TOOL_ARGUMENT_BYTES,
        );
    }

    pub(in crate::llm::openai_compatible) fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .filter(|call| !call.name.trim().is_empty())
            .map(|call| {
                let id = if call.id.is_empty() {
                    gen_tool_call_id()
                } else {
                    call.id
                };
                ToolCall {
                    id,
                    kind: if call.kind.is_empty() {
                        "function".to_string()
                    } else {
                        call.kind
                    },
                    function: ToolCallFunction {
                        name: call.name,
                        arguments: call.arguments,
                    },
                }
            })
            .collect()
    }
}

#[derive(Debug, Default)]
pub(in crate::llm::openai_compatible) struct ResponsesToolAccumulator {
    pub(in crate::llm::openai_compatible) calls: Vec<PartialResponsesToolCall>,
}

#[derive(Debug, Default)]
pub(in crate::llm::openai_compatible) struct PartialResponsesToolCall {
    pub(in crate::llm::openai_compatible) item_id: String,
    pub(in crate::llm::openai_compatible) call: PartialToolCall,
}

impl ResponsesToolAccumulator {
    pub fn start(&mut self, item: ResponsesStreamItem) -> Option<String> {
        if item.kind != "function_call" || self.calls.len() >= MAX_STREAM_TOOL_CALLS {
            return None;
        }
        let item_id = item.id.unwrap_or_default();
        let name = item.name.unwrap_or_default();
        self.calls.push(PartialResponsesToolCall {
            call: PartialToolCall {
                id: item.call_id.unwrap_or_else(|| item_id.clone()),
                kind: "function".to_string(),
                name: name.clone(),
                arguments: bounded_stream_string(
                    item.arguments.unwrap_or_default(),
                    MAX_STREAM_TOOL_ARGUMENT_BYTES,
                ),
            },
            item_id,
        });
        (!name.is_empty()).then_some(name)
    }

    pub(in crate::llm::openai_compatible) fn append_arguments(
        &mut self,
        item_id: Option<String>,
        delta: String,
    ) {
        if let Some(item_id) = item_id {
            if let Some(partial) = self.calls.iter_mut().find(|call| call.item_id == item_id) {
                append_bounded(
                    &mut partial.call.arguments,
                    &delta,
                    MAX_STREAM_TOOL_ARGUMENT_BYTES,
                );
                return;
            }
            return;
        }
        if let Some(partial) = self.calls.last_mut() {
            append_bounded(
                &mut partial.call.arguments,
                &delta,
                MAX_STREAM_TOOL_ARGUMENT_BYTES,
            );
        }
    }

    pub(in crate::llm::openai_compatible) fn finish_item(&mut self, item: ResponsesStreamItem) {
        if item.kind != "function_call" {
            return;
        }
        let item_id = item.id.unwrap_or_default();
        let call_id = item.call_id.unwrap_or_default();
        let existing = self.calls.iter_mut().find(|partial| {
            (!item_id.is_empty() && partial.item_id == item_id)
                || (item_id.is_empty() && !call_id.is_empty() && partial.call.id == call_id)
        });
        if let Some(partial) = existing {
            if !call_id.is_empty() {
                partial.call.id = call_id;
            }
            if let Some(name) = item.name {
                partial.call.name = name;
            }
            if let Some(arguments) = item.arguments {
                partial.call.arguments =
                    bounded_stream_string(arguments, MAX_STREAM_TOOL_ARGUMENT_BYTES);
            }
        } else {
            let _ = self.start(ResponsesStreamItem {
                kind: "function_call".to_string(),
                id: (!item_id.is_empty()).then_some(item_id),
                call_id: (!call_id.is_empty()).then_some(call_id),
                name: item.name,
                arguments: item.arguments,
            });
        }
    }

    pub(in crate::llm::openai_compatible) fn finish_arguments(
        &mut self,
        item_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    ) {
        let Some(item_id) = item_id else {
            return;
        };
        let Some(partial) = self.calls.iter_mut().find(|call| call.item_id == item_id) else {
            return;
        };
        if let Some(name) = name {
            partial.call.name = name;
        }
        if let Some(arguments) = arguments {
            partial.call.arguments =
                bounded_stream_string(arguments, MAX_STREAM_TOOL_ARGUMENT_BYTES);
        }
    }

    pub(in crate::llm::openai_compatible) fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .map(|partial| partial.call)
            .filter(|call| !call.name.trim().is_empty())
            .map(|call| {
                let id = if call.id.is_empty() {
                    gen_tool_call_id()
                } else {
                    call.id
                };
                ToolCall {
                    id,
                    kind: call.kind,
                    function: ToolCallFunction {
                        name: call.name,
                        arguments: call.arguments,
                    },
                }
            })
            .collect()
    }
}

#[derive(Debug, Default)]
pub(in crate::llm::openai_compatible) struct ToolCallAccumulator {
    pub(in crate::llm::openai_compatible) calls: Vec<PartialToolCall>,
}

#[derive(Debug, Default)]
pub(in crate::llm::openai_compatible) struct PartialToolCall {
    pub(in crate::llm::openai_compatible) id: String,
    pub(in crate::llm::openai_compatible) kind: String,
    pub(in crate::llm::openai_compatible) name: String,
    pub(in crate::llm::openai_compatible) arguments: String,
}

impl ToolCallAccumulator {
    pub fn push(&mut self, delta: ToolCallDelta) -> Option<String> {
        if delta.index >= MAX_STREAM_TOOL_CALLS {
            return None;
        }
        while self.calls.len() <= delta.index {
            self.calls.push(PartialToolCall::default());
        }
        let call = &mut self.calls[delta.index];
        let name_before = call.name.len();
        if let Some(id) = delta.id {
            call.id = id;
        }
        if let Some(kind) = delta.kind {
            call.kind = kind;
        }
        if let Some(name) = delta.function.name {
            // Some gateways resend the complete function name on every delta
            // instead of streaming fragments; blind appending would build
            // "use_tooluse_tool…". Treat an exact repeat (or a full-name replay
            // that extends the current prefix) as a replacement, and only
            // append genuine fragments.
            if call.name.is_empty() {
                append_bounded(&mut call.name, &name, 16 * 1024);
            } else if name == call.name {
                // full-name replay, ignore
            } else if name.starts_with(&call.name) {
                call.name.clear();
                append_bounded(&mut call.name, &name, 16 * 1024);
            } else {
                append_bounded(&mut call.name, &name, 16 * 1024);
            }
        }
        if let Some(arguments) = delta.function.arguments {
            append_bounded(
                &mut call.arguments,
                &arguments,
                MAX_STREAM_TOOL_ARGUMENT_BYTES,
            );
        }
        // 只在名字真的变了（第一次出现、或碎片续上）时往外报（09-24 B7）。有的网关
        // 每一片都重发完整函数名：原来每片都报一次，下游把一个调用数成好几个，单个
        // 调用被画成「批量准备」。
        (call.name.len() != name_before && !call.name.is_empty()).then(|| call.name.clone())
    }

    pub(in crate::llm::openai_compatible) fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .filter(|call| !call.name.trim().is_empty())
            .map(|call| {
                let id = if call.id.is_empty() {
                    gen_tool_call_id()
                } else {
                    call.id
                };
                ToolCall {
                    id,
                    kind: if call.kind.is_empty() {
                        "function".to_string()
                    } else {
                        call.kind
                    },
                    function: ToolCallFunction {
                        name: call.name,
                        arguments: call.arguments,
                    },
                }
            })
            .collect()
    }
}

#[derive(Default)]
pub(in crate::llm::openai_compatible) struct Utf8LineBuffer {
    pub(in crate::llm::openai_compatible) buffer: Vec<u8>,
    pub(in crate::llm::openai_compatible) received_bytes: usize,
}

impl Utf8LineBuffer {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        if self.received_bytes.saturating_add(bytes.len()) > MAX_STREAM_RESPONSE_BYTES {
            bail!("streaming response exceeded {MAX_STREAM_RESPONSE_BYTES} bytes");
        }
        if self.buffer.len().saturating_add(bytes.len()) > MAX_STREAM_LINE_BYTES {
            bail!("streaming response line exceeded {MAX_STREAM_LINE_BYTES} bytes");
        }
        self.received_bytes += bytes.len();
        self.buffer.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=index).collect::<Vec<_>>();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(
                std::str::from_utf8(&line)
                    .context("invalid utf-8 in streaming response")?
                    .to_string(),
            );
        }
        Ok(lines)
    }

    pub(in crate::llm::openai_compatible) fn finish(mut self) -> Result<Vec<String>> {
        if self.buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
            return Ok(Vec::new());
        }
        if self.buffer.last() == Some(&b'\r') {
            self.buffer.pop();
        }
        Ok(vec![std::str::from_utf8(&self.buffer)
            .context("invalid utf-8 in streaming response")?
            .to_string()])
    }
}

#[derive(Default)]
pub(in crate::llm::openai_compatible) struct SseDataBuffer {
    pub(in crate::llm::openai_compatible) lines: Utf8LineBuffer,
    pub(in crate::llm::openai_compatible) data_lines: Vec<String>,
}

impl SseDataBuffer {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        let mut events = Vec::new();
        for line in self.lines.push(bytes)? {
            if let Some(event) = self.push_line(&line) {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub(in crate::llm::openai_compatible) fn finish(mut self) -> Result<Vec<String>> {
        let mut events = Vec::new();
        for line in std::mem::take(&mut self.lines).finish()? {
            if let Some(event) = self.push_line(&line) {
                events.push(event);
            }
        }
        if !self.data_lines.is_empty() {
            events.push(self.data_lines.join("\n"));
        }
        Ok(events)
    }

    pub(in crate::llm::openai_compatible) fn push_line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            if self.data_lines.is_empty() {
                return None;
            }
            return Some(std::mem::take(&mut self.data_lines).join("\n"));
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data_lines.push(data.trim_start().to_string());
        }
        None
    }
}
