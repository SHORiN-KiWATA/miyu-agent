"use strict";

/*
 * 子代理的会话(会话项目第 4 段):09-18 起子代理是一条会话。
 *
 * 1. 父会话里子代理那张卡片点下去,打开它的会话——和终端里点时间线上那一行、点任务条上
 *    那一行同一件事。卡片认子会话有三处来源:实时那条 `__subagent_session__` 标记(子会话
 *    一建好就报)、回看时落库的 `child_session_id`、结果里的 `session …`。后台子代理刚派
 *    出去时子会话还没建,三处都没有,卡片照旧展开收起。
 * 2. 在子代理的会话里,输入框上方挂一块「↑ 主会话 · 名字」,点它回去。
 *
 * 单独成文件:app.js 已经一万四千行。
 */
window.MiyuSubagents = (() => {
  const SESSION_MARKER = "__subagent_session__";
  let openSession = () => {};
  let bar = null;

  /** app.js 起来时交进来:怎么打开一条会话。「↑ 主会话」那块挂在输入框上方。 */
  function init({ open }) {
    openSession = open;
    bar = document.getElementById("subagentParentBar");
    bar?.addEventListener("click", () => {
      const id = bar.dataset.sessionId;
      if (id) openSession(id);
    });
  }

  /**
   * 子代理结果里的子会话 id:前台跑完是 `subagent <状态> (tier …, session <id>): …` 那一行,
   * 追话排进去了是 JSON 的 `session_id`。和 Rust 那边 `subagent_session_of_output` 同一个
   * 认法,格式由 `format_child_outcome` 定。
   */
  function sessionOfOutput(output) {
    const text = String(output || "").trimStart();
    if (text.startsWith("{")) {
      try {
        const id = JSON.parse(text)?.session_id;
        return typeof id === "string" && id ? id : "";
      } catch (_) {
        return "";
      }
    }
    const match = /^subagent \S+ \([^)]*?, session ([^)\s]+)\)/.exec(text.split("\n", 1)[0]);
    return match ? match[1] : "";
  }

  /** 卡片认出了它的子会话:点抬头就打开它。 */
  function link(card, sessionId) {
    if (!card || !sessionId) return;
    card.dataset.childSession = sessionId;
    card.classList.add("has-child-session");
    const head = card.querySelector(":scope > .tool-head");
    if (head) head.title = t("打开子代理的会话");
  }

  /** 实时那条标记:认出来就挂上、吃掉,别让它漏进窥视那一行。 */
  function takeMarker(card, message) {
    if (!String(message || "").startsWith(SESSION_MARKER)) return false;
    link(card, String(message).slice(SESSION_MARKER.length).trim());
    return true;
  }

  /** 抬头被点了:认得子会话就打开它,返回真表示这一下已经处理了。 */
  function openFromCard(card) {
    const id = card?.dataset?.childSession;
    if (!id) return false;
    openSession(id);
    return true;
  }

  /** 换了会话(回合接口的回包):是子代理的会话就挂上回去的那块,不是就收起。 */
  function viewed(payload) {
    if (!bar) return;
    const parent = payload?.session_kind === "subagent" ? payload?.parent : null;
    if (!parent?.session_id) {
      bar.hidden = true;
      delete bar.dataset.sessionId;
      return;
    }
    bar.dataset.sessionId = parent.session_id;
    bar.textContent = `↑ ${t("主会话")} · ${parent.name || t("新会话")}`;
    bar.hidden = false;
  }

  /** 打开一条子代理会话(任务条上的后台子代理那一行)。 */
  function open(sessionId) {
    if (sessionId) openSession(String(sessionId));
  }

  return { init, link, open, takeMarker, openFromCard, sessionOfOutput, viewed };
})();
