"use strict";

/*
 * 回复末尾那行 ✻(09-26,用户照 Claude Code 定)。和终端
 * `crates/miyu-hosts/src/render/stream/timeline/turn_end.rs` 同一套词表、同一个哈希、同一种写法:
 *
 *   ✻ deepseek-v4.1-flash · 处理了 3 分 14 秒 · 1:53 完成
 *
 * - 动词按轮号固定(FNV-1a),刷新、重开、终端和网页上都是同一个词;
 * - 不是今天的轮在时刻前面带上日期;
 * - 网页只给跑完的轮画:被打断的轮已经有「本轮已中断」那一行(带词元数);
 * - 混合模型池时「供应商 / 模型」写在这一行的模型位置上,不再在用量那一行另写一遍。
 *
 * 单独成文件:app.js 已经一万四千行。
 */
window.MiyuTurnEnd = (() => {
  // 顺序和 turn_end.rs 的 VERBS_ZH / VERBS_EN 一一对应(英文在 i18n-en.js)。
  const VERBS = [t("处理了"), t("忙活了"), t("琢磨了"), t("推敲了"), t("折腾了"), t("消耗了")];
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

  function verbIndex(turnId) {
    let hash = 0x811c9dc5;
    for (const byte of new TextEncoder().encode(String(turnId || ""))) {
      hash = Math.imul((hash ^ byte) >>> 0, 0x01000193) >>> 0;
    }
    return hash % VERBS.length;
  }

  // 花了多久:整秒,中文不补零(`3 分 4 秒`),英文同终端的 `3m 04s`。
  function spent(milliseconds) {
    if (!(milliseconds >= 1000)) return t("不到 1 秒");
    const total = Math.floor(milliseconds / 1000);
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    const pad = (n) => String(n).padStart(2, "0");
    if (h) return t("{h} 小时 {m} 分 {s} 秒", { h, m, s, mm: pad(m), ss: pad(s) });
    if (m) return t("{m} 分 {s} 秒", { m, s, ss: pad(s) });
    return t("{s} 秒", { s });
  }

  // 几点:今天的只写时刻,今年的带月日,更早的带年份。
  function clock(at, now) {
    const time = `${at.getHours()}:${String(at.getMinutes()).padStart(2, "0")}`;
    const sameDay = at.getFullYear() === now.getFullYear()
      && at.getMonth() === now.getMonth()
      && at.getDate() === now.getDate();
    if (sameDay) return time;
    const params = {
      clock: time,
      month: at.getMonth() + 1,
      day: at.getDate(),
      year: at.getFullYear(),
      mon: MONTHS[at.getMonth()],
    };
    return at.getFullYear() === now.getFullYear()
      ? t("{month}月{day}日 {clock}", params)
      : t("{year}年{month}月{day}日 {clock}", params);
  }

  // 一轮的收尾行;缺轮号或时刻(还在跑、老数据)就是空串。
  function text({ turnId, model, startedAt, finishedAt, interrupted = false }) {
    const started = new Date(startedAt);
    const finished = new Date(finishedAt);
    if (!turnId || Number.isNaN(started.getTime()) || Number.isNaN(finished.getTime())) return "";
    const at = clock(finished, new Date());
    const parts = [];
    const name = String(model || "").trim();
    if (name) parts.push(name);
    // 数字前留一个空格(`处理了 3 秒`),「不到 1 秒」直接接上(`处理了不到 1 秒`),同终端。
    const took = spent(finished - started);
    const gap = /^[\d<]/.test(took) ? " " : "";
    parts.push(`${VERBS[verbIndex(turnId)]}${gap}${took}`);
    parts.push(interrupted ? t("{at} 中断", { at }) : t("{at} 完成", { at }));
    return `✻ ${parts.join(" · ")}`;
  }

  // 挂在这段回复的最末尾;同一个 article 只有一行,重画时就地改。混合模型池时用量那一行开头的
  // 「供应商 / 模型」并进这一行的模型位置、那个标签藏起来(用户 09-26:同一个模型名写了两遍),
  // 和终端一个样子。并过一次就记在 article 上,重画时标签已经藏了也还认得。
  function attach(article, info) {
    if (!article) return;
    const endpoint = article.querySelector(".assistant-meta .assistant-endpoint");
    const shown = endpoint && !endpoint.hidden ? endpoint.textContent.trim() : "";
    if (shown) article.dataset.endpointLabel = shown;
    const label = article.dataset.endpointLabel || "";
    const value = text({ ...(info || {}), model: label || info?.model });
    let line = article.querySelector(":scope > .turn-end");
    if (!value) {
      line?.remove();
      if (endpoint && label) endpoint.hidden = false;
      return;
    }
    if (endpoint && label) endpoint.hidden = true;
    if (!line) {
      line = document.createElement("div");
      line.className = "turn-end";
      article.appendChild(line);
    }
    line.textContent = value;
  }

  return { text, attach, verbIndex };
})();
