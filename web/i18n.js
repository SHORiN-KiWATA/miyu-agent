// WebUI 双语运行时(2026-09-23 用户拍板:界面语言跟 config 的 display.language,
// auto = 浏览器语言)。语言由服务端解析后注入 `window.MIYU_LANG`(见
// crates/miyu-hosts/src/web/ui_locale.rs)—— 同一个 daemon 可能同时服务中英
// 浏览器,前端不自己读 navigator.language,也不自己猜。
//
// 词典以中文原文为键(gettext 风格):中文界面直接返回原文,英文界面查
// window.MIYU_I18N_EN,查不到回退中文原文(宁可混一句中文,不要空文案)。
//
// 用法:
//   t("删除会话")                     → 静态文案
//   t("还有 {count} 项", {count: 3})  → 带占位符
//   <span data-i18n="删除会话"></span> → index.html 静态骨架,启动时扫一遍
// 新增文案必须同步 web/i18n-en.js;漏掉会被 scripts/check-webui-i18n.py
// 与 testkit/webui-i18n 拦下。
window.MiyuI18n = (() => {
  "use strict";

  const LANG = window.MIYU_LANG === "zh" ? "zh" : "en";
  const DICT = LANG === "en" && window.MIYU_I18N_EN ? window.MIYU_I18N_EN : {};

  function translate(text, params) {
    if (text == null) return text;
    let out = LANG === "zh" ? String(text) : DICT[text] ?? String(text);
    if (params) {
      out = out.replace(/\{(\w+)\}/g, (whole, key) =>
        params[key] == null ? whole : String(params[key]));
    }
    return out;
  }

  // 数字/日期的 Intl 区域:跟随界面语言。en-US 而不是 en —— 12 小时制与
  // 月名顺序是美式习惯,和界面文案同语言才不别扭。
  const INTL_LOCALE = LANG === "zh" ? "zh-CN" : "en-US";

  function number(value, options) {
    return new Intl.NumberFormat(INTL_LOCALE, options || {}).format(value);
  }

  function date(value, options) {
    return new Intl.DateTimeFormat(INTL_LOCALE, options || {}).format(value);
  }

  // index.html 里写死的中文骨架:启动时按属性扫一遍。JS 动态生成的 DOM 不走
  // 这里,由生成它的代码自己 t()。
  //
  // data-i18n-text 给「混着子元素」的元素用(如 `<li><span>1</span>人格</li>`):
  // 只换它自己的文本节点,不动子元素——data-i18n 是整块 textContent,会把
  // 子元素一起擦掉。
  function applyTextNodes(el, value) {
    for (const node of Array.from(el.childNodes)) {
      if (node.nodeType !== Node.TEXT_NODE) continue;
      const raw = node.nodeValue;
      const trimmed = raw.trim();
      if (!trimmed) continue;
      const start = raw.indexOf(trimmed);
      node.nodeValue =
        raw.slice(0, start) + translate(trimmed) + raw.slice(start + trimmed.length);
    }
  }

  const ATTRS = [
    ["data-i18n", (el, value) => { el.textContent = value; }],
    ["data-i18n-text", (el, value) => { applyTextNodes(el, value); }],
    ["data-i18n-title", (el, value) => { el.title = value; }],
    ["data-i18n-aria-label", (el, value) => { el.setAttribute("aria-label", value); }],
    ["data-i18n-placeholder", (el, value) => { el.placeholder = value; }],
    ["data-i18n-alt", (el, value) => { el.alt = value; }],
    // 空会话的建议按钮:可见文字在 <span> 上,点击发送的值在 data-prompt 上,
    // 两个都要跟界面语言(app.js 载入人格后会用服务端下发的值覆盖)。
    ["data-i18n-prompt", (el, value) => { el.dataset.prompt = value; }],
  ];

  function applyDom(root) {
    const scope = root || document;
    for (const [attr, set] of ATTRS) {
      for (const el of scope.querySelectorAll(`[${attr}]`)) {
        const value = el.getAttribute(attr);
        if (value) set(el, translate(value));
      }
    }
  }

  document.documentElement.lang = INTL_LOCALE;

  // defer 脚本执行时文档已解析完,直接扫;动态加载(readyState=loading)才等
  // DOMContentLoaded。必须先于 app.js 的初始化跑,免得覆盖它写进去的动态值。
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => applyDom(), { once: true });
  } else {
    applyDom();
  }

  return { lang: LANG, intlLocale: INTL_LOCALE, t: translate, number, date, applyDom };
})();

// 短别名:文案调用点太多,`MiyuI18n.t(...)` 到处写太长;`t` 只做这一件事。
window.t = window.MiyuI18n.t;
