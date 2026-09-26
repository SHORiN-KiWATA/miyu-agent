"use strict";

/*
 * 跨会话消息(09-23):一个会话里的 AI 用 send_to_other_running_session 给别的开着的
 * 会话发话。这里放 WebUI 这一侧的两件事:
 *
 * 1. 在线登记:这个标签页此刻开着哪条会话。别的会话里的 AI 列「开着的会话」靠它。
 *    心跳 20 秒一次(服务端 150 秒过期——浏览器会把后台标签的定时器放慢到一分钟
 *    一次),换会话当场报,关页面时注销。
 * 2. 消息外壳的拆解与显示(收到的那条、发出去的那次工具调用)。
 *
 * 单独成文件:app.js 已经一万三千多行。
 */
window.MiyuCrossSession = (() => {
  const HEARTBEAT_MS = 20000;
  const CHECK_MS = 1000;
  const viewer = makeViewerId();
  let viewedSession = () => null;
  let reported = null;
  let reportedAt = 0;
  let started = false;

  function makeViewerId() {
    try {
      if (window.crypto?.randomUUID) return window.crypto.randomUUID();
    } catch (_) {
      // 非安全上下文(局域网 http)没有 randomUUID,退回下面那种。
    }
    return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  }

  function postPresence(sessionId, keepalive = false) {
    const payload = sessionId ? { viewer, session_id: sessionId } : { viewer };
    return fetch("/api/presence", {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      credentials: "same-origin",
      body: JSON.stringify(payload),
      keepalive
    }).catch(() => null);
  }

  function tickPresence() {
    const sessionId = String(viewedSession() || "") || null;
    const now = Date.now();
    if (sessionId === reported && now - reportedAt < HEARTBEAT_MS) return;
    reported = sessionId;
    reportedAt = now;
    postPresence(sessionId);
  }

  /// `getViewedSession` 返回这个标签页正在看的会话 id(没有就 null)。
  function startPresence(getViewedSession) {
    if (started) return;
    started = true;
    if (typeof getViewedSession === "function") viewedSession = getViewedSession;
    tickPresence();
    window.setInterval(tickPresence, CHECK_MS);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState !== "visible") return;
      reportedAt = 0;
      tickPresence();
    });
    // 关页面、刷新:当场注销,不用等过期。从往返缓存里回来的页面重新报。
    window.addEventListener("pagehide", () => {
      reported = null;
      postPresence(null, true);
    });
    window.addEventListener("pageshow", (event) => {
      if (!event.persisted) return;
      reportedAt = 0;
      tickPresence();
    });
  }

  // ---- 消息外壳与显示 ----

  const TAG = "<cross-session-message";
  const CLOSE_TAG = "</cross-session-message>";
  // 与 Rust 侧 miyu_core::state::CROSS_SESSION_SENDER_NOTE 同一句,拆外壳时剥掉。
  const SENDER_NOTE = "Sent by the AI in another session, not by the user.";
  const SEND_TOOL = "send_to_other_running_session";
  let deps = { makeIconSlot: null, renderMarkdown: null, formatDateTime: null, previewLines: null };

  /// app.js 把图标工厂、正文渲染器、时间格式、预览行数递过来(它们都在 app.js 的闭包里)。
  function init(options) {
    deps = { ...deps, ...(options || {}) };
  }

  /// 会话 id 的短写:末尾那段随机串(`sess_…_acd86d38` → `acd86d38`),与 Rust 的
  /// `short_session_id` 一致。完整 id 二十几个字符,界面上放不下(用户 09-24)。
  function shortId(sessionId) {
    const id = String(sessionId || "");
    const at = id.lastIndexOf("_");
    return at >= 0 && at < id.length - 1 ? id.slice(at + 1) : id;
  }

  function previewRows() {
    const value = Number(typeof deps.previewLines === "function" ? deps.previewLines() : 10);
    return Number.isFinite(value) && value >= 0 ? Math.floor(value) : 10;
  }

  /// ` key="…"`:值按 JSON 字符串的转义规则读(发件端就是这么写的)。
  function readAttr(input, key) {
    const text = input.trimStart();
    const prefix = `${key}="`;
    if (!text.startsWith(prefix)) return null;
    let escaped = false;
    for (let index = prefix.length; index < text.length; index += 1) {
      const ch = text[index];
      if (ch === "\\" && !escaped) {
        escaped = true;
        continue;
      }
      if (ch === '"' && !escaped) {
        try {
          return { value: JSON.parse(`"${text.slice(prefix.length, index)}"`), rest: text.slice(index + 1) };
        } catch (_) {
          return null;
        }
      }
      escaped = false;
    }
    return null;
  }

  /// 拆外壳;不是跨会话消息(或外壳坏了)返回 null。规则与 Rust 的
  /// `parse_cross_session_message` 一致。
  function parse(content) {
    const text = String(content || "").trimStart();
    if (!text.startsWith(TAG)) return null;
    const from = readAttr(text.slice(TAG.length), "from");
    if (!from) return null;
    const session = readAttr(from.rest, "session");
    if (!session) return null;
    let rest = session.rest.trimStart();
    if (!rest.startsWith(">")) return null;
    rest = rest.slice(1);
    if (rest.startsWith("\n")) rest = rest.slice(1);
    if (rest.startsWith(SENDER_NOTE)) {
      rest = rest.slice(SENDER_NOTE.length);
      if (rest.startsWith("\n")) rest = rest.slice(1);
    }
    let body = rest.trimEnd();
    if (body.endsWith(CLOSE_TAG)) body = body.slice(0, -CLOSE_TAG.length);
    return { fromName: from.value, fromSession: session.value, body: body.replace(/\n+$/, "") };
  }

  /// 正文先露 N 行(按屏幕上占的行算,和命令预览一个口径),露不全时底下给 `⋮`。
  function clampedBody(text) {
    const body = document.createElement("div");
    body.className = "xs-body";
    if (typeof deps.renderMarkdown === "function") deps.renderMarkdown(body, text);
    else body.textContent = text;
    // `⋮` 得在裁剪容器外面,否则它自己也被 max-height 裁掉。
    const more = document.createElement("span");
    more.className = "xs-more";
    more.textContent = "⋮";
    more.title = t("还有，点开看全文");
    more.hidden = true;
    return { body, more };
  }

  /// 前 `rows` 行的底边离正文顶多高;不满 `rows` 行返回 null(不用截)。
  ///
  /// 按实际排出来的行盒量,不按「行数 × line-height」算:中文回退字体、行内代码的
  /// 等宽字体会把行盒撑得比 line-height 高,按乘法截会切出半行(09-23 走查撞到)。
  function rowsHeight(body, rows) {
    if (rows <= 0) return 0;
    const box = body.getBoundingClientRect();
    // 外壳有 zoom(1.1 上下):量出来的是缩放后的坐标,max-height 却按缩放前的 px 生效,
    // 不除回去就多露一行(同 app.js 的 procLineFit)。
    const zoom = body.offsetWidth ? box.width / body.offsetWidth : 1;
    const rects = [];
    const range = document.createRange();
    const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      if (!node.textContent.trim()) continue;
      range.selectNodeContents(node);
      for (const rect of range.getClientRects()) if (rect.height > 0) rects.push(rect);
    }
    rects.sort((a, b) => a.top - b.top);
    let lines = 0;
    let lineBottom = -Infinity;
    for (const rect of rects) {
      // 同一行里的几段(正文、行内代码)上沿参差,按「落在上一行底边之上」归到同一行。
      if (rect.top < lineBottom - 1) {
        lineBottom = Math.max(lineBottom, rect.bottom);
        continue;
      }
      if (lines === rows) return Math.ceil((lineBottom - box.top) / zoom);
      lines += 1;
      lineBottom = rect.bottom;
    }
    return null;
  }

  /// 按配置的行数截,再看露全了没有。插进页面之后才量得出来,所以挂在下一帧和
  /// 尺寸变化上(宽度一变,折行跟着变)。
  function watchClip(owner, body, more) {
    const measure = () => {
      if (!owner.classList.contains("is-expanded")) {
        const height = rowsHeight(body, previewRows());
        body.style.setProperty("--xs-clip", height === null ? "none" : `${height}px`);
      }
      const clipped = body.scrollHeight > body.clientHeight + 1;
      more.hidden = !clipped;
      owner.classList.toggle("is-clipped", clipped);
    };
    window.requestAnimationFrame(measure);
    if (window.ResizeObserver) new ResizeObserver(measure).observe(body);
  }

  /// 拖着选文字、点链接都不算「点开」。
  function isPlainClick(event) {
    if (String(window.getSelection?.() || "").length) return false;
    return !event.target?.closest?.("a, button, summary");
  }

  /// 收到的那条:铃铛 + 「从 xxx 收到消息」,底下一条竖线串着正文。先露 N 行,
  /// 点一下看全文,再点收回去。
  /// `attributes.headline` / `icon`:别的「不是谁敲的话」借这一块的样子(子代理会话的
  /// 第一轮是主会话派的任务,会话项目第 4 段),换一句抬头、换一个图标。
  function createReceived(message, attributes = {}) {
    const node = document.createElement("div");
    node.className = "xs-message";
    if (attributes.turnId) node.dataset.turnId = attributes.turnId;
    if (attributes.followupId) node.dataset.followupId = attributes.followupId;
    const head = document.createElement("div");
    head.className = "xs-head";
    if (typeof deps.makeIconSlot === "function") {
      head.appendChild(deps.makeIconSlot(attributes.icon || "bell"));
    }
    const label = document.createElement("span");
    label.textContent = attributes.headline || t("从 {name}（{id}）收到消息", {
      name: message.fromName || shortId(message.fromSession),
      id: shortId(message.fromSession)
    });
    if (attributes.timestamp && typeof deps.formatDateTime === "function") {
      label.title = deps.formatDateTime(attributes.timestamp);
    }
    head.appendChild(label);
    const { body, more } = clampedBody(message.body);
    node.append(head, body, more);
    watchClip(node, body, more);
    node.addEventListener("click", (event) => {
      if (!isPlainClick(event)) return;
      if (!node.classList.contains("is-clipped") && !node.classList.contains("is-expanded")) return;
      node.classList.toggle("is-expanded");
    });
    return node;
  }

  /// 收到的那一块并进紧跟着的那段 AI 回复,放在最前面(用户 09-24 定的版式):
  /// 「Miyu → 从 X 收到消息 → 她的回复」连成一体。挂在时间线顶层、后面紧跟一段
  /// 回复的才挪;回复还没出来的先留在原地,等那段回复建出来时再调一次。
  /// 插进正在跑的那一轮的消息,落在它之后那一段回复的最前面,也就是时间线上当时
  /// 的位置。本页实时画过的回复重画时原样复用(那一块已经在里面),撞上一模一样
  /// 的就扔掉新的。
  function foldInto(timeline) {
    if (!timeline) return;
    for (const node of [...timeline.querySelectorAll(":scope > .xs-message")]) {
      let next = node.nextElementSibling;
      // 一次读到好几条:连着的几块一起挪进同一段回复,先后不变。
      while (next && next.classList.contains("xs-message")) next = next.nextElementSibling;
      const blocks = next?.classList.contains("assistant-message") ? next.querySelector(".assistant-blocks") : null;
      if (!blocks) continue;
      const already = [...blocks.querySelectorAll(":scope > .xs-message")]
        .some((existing) => existing.textContent === node.textContent);
      if (already) {
        node.remove();
        continue;
      }
      const anchor = [...blocks.children].find((child) => !child.classList.contains("xs-message")) || null;
      blocks.insertBefore(node, anchor);
    }
  }

  function isSendTool(name) {
    return String(name || "") === SEND_TOOL;
  }

  function parsedArgs(value) {
    if (value && typeof value === "object") return value;
    try {
      const parsed = JSON.parse(String(value || ""));
      return parsed && typeof parsed === "object" ? parsed : {};
    } catch (_) {
      return {};
    }
  }

  /// 工具签抬头右边那句:发话时是 `<短 id> <会话名>`,列名单时空着(抬头已经叫「列出
  /// 其他会话」了)。名字先按侧栏里认得的填,结果回来再用它报的名字。
  function sendSubject(argumentsValue, knownName) {
    const args = parsedArgs(argumentsValue);
    if (args.action === "list") return "";
    const session = shortId(String(args.session_id || "").trim());
    const name = String(knownName || "").trim();
    return [session, name].filter(Boolean).join(" ");
  }

  /// 这一次调用自己的抬头:列名单时叫「列出其他会话」(用户 09-26,和终端一个说法),
  /// 发话时空串,照用 daemon 给的显示名。
  function sendTitle(argumentsValue) {
    return parsedArgs(argumentsValue).action === "list" ? t("列出其他会话") : "";
  }

  /// 结果里对方会话的名字(发成功时才有)。
  function targetName(output) {
    const result = parsedArgs(output);
    return typeof result.name === "string" ? result.name : "";
  }

  /// 发话那张工具签:抬头底下露正文前 N 行(点它等于点抬头),展开区顶上放正文
  /// 全文,裸参数那栏收起来。只处理 `action=send`,列名单照普通工具签画。
  function decorateSendCard({ card, head, body, argumentsDetail, argumentsValue }) {
    const args = parsedArgs(argumentsValue);
    const message = typeof args.message === "string" ? args.message.trim() : "";
    if (args.action !== "send" || !message) return false;
    card.classList.add("is-xs-send");
    const preview = clampedBody(message);
    preview.body.classList.add("xs-send-preview");
    preview.more.classList.add("xs-send-more");
    const anchor = head.nextSibling;
    card.insertBefore(preview.body, anchor);
    card.insertBefore(preview.more, anchor);
    watchClip(card, preview.body, preview.more);
    for (const node of [preview.body, preview.more]) {
      node.addEventListener("click", (event) => {
        if (!isPlainClick(event)) return;
        event.preventDefault();
        head.click();
      });
    }
    const full = document.createElement("div");
    full.className = "xs-send-full";
    if (typeof deps.renderMarkdown === "function") deps.renderMarkdown(full, message);
    else full.textContent = message;
    body.insertBefore(full, body.firstChild);
    if (argumentsDetail?.wrapper) argumentsDetail.wrapper.hidden = true;
    return true;
  }

  return {
    startPresence,
    init,
    parse,
    createReceived,
    foldInto,
    isSendTool,
    sendSubject,
    sendTitle,
    targetName,
    decorateSendCard
  };
})();
