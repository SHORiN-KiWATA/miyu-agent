"use strict";

/*
 * 文件编辑(`edit` / `apply_patch`)工具的 diff 渲染。
 *
 * edit 工具的参数是一坨 apply_patch 文本(`*** Begin Patch … *** End Patch`),
 * 工具签里以前只把它当原始 JSON 参数摊出来,一屏问号。可这坨文本本身就是 diff:
 * `*** Update File:` 分文件、`@@` 分块、` `/`+`/`-` 分上下文/增/删。这里把它解析成
 * 一张带增删配色的 diff 卡。
 *
 * 关键:patchText 是**参数**,既在实时的 tool_call 里、也随 tool_flow 落库,所以
 * 实时和刷新回看走同一份、同样画得出,不依赖后端另发 diff、也不依赖工具输出。
 *
 * 单独成文件:app.js 已经上万行(与 todos.js / shared.js 同构)。
 */
window.MiyuDiff = (() => {
  // 三个补丁工具都吃 apply_patch 格式的 patchText:edit(文件系统)/ kb(知识库)/
  // artifact(WebUI 交付文件),外加中转线上的别名。渲染只认 patchText,认不出照样
  // 退回原始参数,所以名单宽一点没风险。
  const EDIT_TOOLS = new Set([
    "edit", "kb", "artifact", "apply_patch", "apply_artifact_patch",
  ]);
  function isEditTool(name) {
    return EDIT_TOOLS.has(String(name || "").toLowerCase());
  }

  /// 从工具调用里取出 patchText。取不到返回 null,调用方照常走原来的参数渲染。
  function patchTextOf(argumentsRaw) {
    if (argumentsRaw == null) return null;
    if (typeof argumentsRaw === "object") {
      const value = argumentsRaw.patchText ?? argumentsRaw.patch ?? null;
      return typeof value === "string" && value.trim() ? value : null;
    }
    const text = String(argumentsRaw).trim();
    if (!text) return null;
    // 参数落库是原样 JSON 字符串;先按 JSON 解,失败再看它本身是不是就是补丁文本。
    if (text.startsWith("{")) {
      try {
        const parsed = JSON.parse(text);
        const value = parsed?.patchText ?? parsed?.patch ?? null;
        return typeof value === "string" && value.trim() ? value : null;
      } catch (_) {
        // 落进来的可能是被截断/含裸换行的半个 JSON——退一步找 patchText 字段。
        const m = text.match(/"patch(?:Text)?"\s*:\s*"((?:\\.|[^"\\])*)"/s);
        if (m) {
          try { return JSON.parse(`"${m[1]}"`); } catch (_) { return null; }
        }
        return null;
      }
    }
    if (text.includes("*** Begin Patch")) return text;
    return null;
  }

  /// 显示用路径:去掉 artifact:/kb: 命名空间前缀,但记下来当徽标。
  function splitNamespace(rawPath) {
    const path = String(rawPath || "").trim();
    if (path.startsWith("artifact:")) return { ns: "artifact", path: path.slice("artifact:".length) };
    if (path.startsWith("kb:")) return { ns: "kb", path: path.slice("kb:".length) };
    return { ns: "", path };
  }

  const HEADERS = [
    { key: "add", label: "*** Add File: " },
    { key: "update", label: "*** Update File: " },
    { key: "delete", label: "*** Delete File: " },
  ];

  /// 把 apply_patch 文本解析成 [{op, ns, path, moveTo, lines:[{kind, text}]}]。
  /// kind: "add" | "del" | "ctx" | "hunk"(@@ 分隔)。解析不出任何文件返回 null。
  function parsePatch(patchText) {
    const raw = String(patchText || "").replace(/\r\n/g, "\n").split("\n");
    // 掐头去尾到 Begin/End 之间(容忍缺失)。
    let start = raw.findIndex((l) => l.trim() === "*** Begin Patch");
    let end = raw.length;
    for (let i = raw.length - 1; i >= 0; i--) {
      if (raw[i].trim() === "*** End Patch") { end = i; break; }
    }
    const lines = raw.slice(start >= 0 ? start + 1 : 0, end);
    const files = [];
    let cur = null;
    const flush = () => { if (cur) files.push(cur); cur = null; };
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      let matchedHeader = false;
      for (const h of HEADERS) {
        if (line.startsWith(h.label)) {
          flush();
          const { ns, path } = splitNamespace(line.slice(h.label.length));
          cur = { op: h.key, ns, path, moveTo: "", lines: [] };
          matchedHeader = true;
          break;
        }
      }
      if (matchedHeader) continue;
      if (!cur) continue;
      if (line.startsWith("*** Move to: ")) {
        cur.moveTo = splitNamespace(line.slice("*** Move to: ".length)).path;
        continue;
      }
      if (line.startsWith("*** ")) continue; // 其它元行忽略
      if (line.startsWith("--- ") || line.startsWith("+++ ")) continue; // unified 头行跳过
      if (line.startsWith("@@")) {
        const ctx = line.replace(/^@@+/, "").replace(/@@\s*$/, "").trim();
        cur.lines.push({ kind: "hunk", text: ctx });
        continue;
      }
      if (cur.op === "add") {
        // 新增文件:每行以 + 开头就是新内容;容忍没带 + 的裸行。
        cur.lines.push({ kind: "add", text: line.startsWith("+") ? line.slice(1) : line });
        continue;
      }
      if (cur.op === "delete") {
        cur.lines.push({ kind: "del", text: line.startsWith("-") ? line.slice(1) : line });
        continue;
      }
      // update:按首字符分增/删/上下文。
      const head = line[0];
      if (head === "+") cur.lines.push({ kind: "add", text: line.slice(1) });
      else if (head === "-") cur.lines.push({ kind: "del", text: line.slice(1) });
      else cur.lines.push({ kind: "ctx", text: head === " " ? line.slice(1) : line });
    }
    flush();
    return files.length ? files : null;
  }

  const OP_LABEL = { add: t("新建"), update: t("修改"), delete: t("已删除") };

  function counts(file) {
    let adds = 0, dels = 0;
    for (const l of file.lines) {
      if (l.kind === "add") adds++;
      else if (l.kind === "del") dels++;
    }
    return { adds, dels };
  }

  /// 渲染一坨补丁文本成 diff 卡;解析不出返回 null。
  function renderPatch(patchText) {
    const files = parsePatch(patchText);
    if (!files) return null;
    const view = document.createElement("div");
    view.className = "diff-view";
    for (const file of files) {
      const block = document.createElement("div");
      block.className = `diff-file is-${file.op}`;

      const head = document.createElement("div");
      head.className = "diff-file-head";
      const op = document.createElement("span");
      op.className = "diff-op";
      op.textContent = OP_LABEL[file.op] || file.op;
      const path = document.createElement("span");
      path.className = "diff-path";
      path.textContent = file.moveTo ? `${file.path} → ${file.moveTo}` : file.path;
      path.title = path.textContent;
      head.append(op);
      if (file.ns) {
        const ns = document.createElement("span");
        ns.className = "diff-ns";
        ns.textContent = file.ns;
        head.append(ns);
      }
      head.append(path);
      const { adds, dels } = counts(file);
      const stat = document.createElement("span");
      stat.className = "diff-stat";
      if (adds) { const a = document.createElement("b"); a.className = "diff-stat-add"; a.textContent = `+${adds}`; stat.append(a); }
      if (dels) { const d = document.createElement("b"); d.className = "diff-stat-del"; d.textContent = `−${dels}`; stat.append(d); }
      head.append(stat);
      block.append(head);

      if (file.lines.length) {
        const body = document.createElement("div");
        body.className = "diff-lines";
        for (const l of file.lines) {
          if (l.kind === "hunk") {
            const hunk = document.createElement("div");
            hunk.className = "diff-hunk";
            hunk.textContent = l.text || "⋯";
            body.append(hunk);
            continue;
          }
          const row = document.createElement("div");
          row.className = `diff-line is-${l.kind}`;
          const gutter = document.createElement("span");
          gutter.className = "diff-gutter";
          gutter.textContent = l.kind === "add" ? "+" : l.kind === "del" ? "−" : " ";
          const text = document.createElement("span");
          text.className = "diff-text";
          text.textContent = l.text;
          row.append(gutter, text);
          body.append(row);
        }
        block.append(body);
      }
      view.append(block);
    }
    return view;
  }

  /// 从工具调用(实时 data 或落库 call)渲染;取不到补丁返回 null。
  function renderFromCall(call) {
    if (!isEditTool(call?.name)) return null;
    const patchText = patchTextOf(call?.arguments);
    if (!patchText) return null;
    return renderPatch(patchText);
  }

  return { isEditTool, patchTextOf, parsePatch, renderPatch, renderFromCall };
})();
