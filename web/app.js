(() => {
  "use strict";

  const MAX_CONTENT_CHARS = 20_000;
  const MAX_CUSTOM_ANSWER_CHARS = 4_000;
  const MAX_TOOL_OUTPUT_CHARS = 200_000;
  const MAX_ATTACHMENTS = 12;
  const COMMAND_OUTPUT_PREVIEW_ROWS = 8;
  const NEAR_BOTTOM_PX = 120;
  // 常驻任务面板的宽度闸,与 styles.css 里 `.main-stage.is-wide` 的判据一致。
  const STAGE_WIDE_PX = 1360;
  // smooth 滚动的兜底:动画期间 scroll 事件由 programmaticScroll 守卫吃掉,
  // 万一条数不够(或压根没滚动)也不能让守卫永久卡住。
  const PROGRAMMATIC_SCROLL_MS = 600;
  // auto 滚动的兜底:视口已经在底时 scrollTo 不会派发 scroll 事件,守卫没有
  // 那条「回执」可吃,得靠超时解除,否则用户下一次滚动的第一条事件会被吞掉。
  const PROGRAMMATIC_SCROLL_AUTO_MS = 150;
  // Mirrors the CSS --ui-scale custom property; mobile drops it to 1 via a
  // media query, so read it at runtime instead of hardcoding.
  let UI_SCALE = 1.1;
  function refreshUiScale() {
    const raw = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--ui-scale")
    );
    if (Number.isFinite(raw) && raw > 0) UI_SCALE = raw;
  }
  refreshUiScale();
  window.addEventListener("resize", refreshUiScale);
  const artifactTextScale = () => 1.2 / UI_SCALE;
  const DEFAULT_BOARD_TITLE = t("今天想聊些什么？");
  const DEFAULT_BOARD_SUBTITLE = t("从一个问题、计划或此刻的想法开始。");
  // 输入框提示跟着人格名走,所以是函数不是常量;与后端
  // `web::dto::default_composer_placeholder` 保持同一句话。
  const defaultComposerPlaceholder = (name) => t("给 {name} 发消息", {name: name});
  const DEFAULT_STARTER_PROMPTS = [t("查询今天的天气"), t("分析一个问题"), t("发表情包打个招呼吧"), t("搜索一张图片")];
  // 档位一律用供应商原值(max/high/minimal…),不翻译:译名和文档、和模型
  // 实际认的参数值对不上,查起来反而费劲。"没设"这一档没有原值,只好写字。
  const THINKING_VARIANT_DEFAULT_LABEL = "default";
  // 会话钉成「模型默认档」的记法(09-24:会话里选默认是模型默认,不是回到跟随全局)。
  // 与 daemon 的 `MODEL_DEFAULT_PIN` 同一个值。
  const MODEL_DEFAULT_PIN = "@model-default";

  function layoutViewportWidth() {
    return (window.innerWidth || document.documentElement.clientWidth || 0) / UI_SCALE;
  }

  function visualPixelsToLayout(value) {
    return Number(value || 0) / UI_SCALE;
  }

  const SVG_NS = "http://www.w3.org/2000/svg";
  const ICONS = {
    "arrow-down": [["path", { d: "M12 5v14" }], ["path", { d: "m19 12-7 7-7-7" }]],
    "arrow-up": [["path", { d: "m5 12 7-7 7 7" }], ["path", { d: "M12 19V5" }]],
    atom: [["circle", { cx: "12", cy: "12", r: "1" }], ["path", { d: "M20.2 20.2c2.04-2.03.02-7.37-4.5-11.9-4.52-4.52-9.87-6.54-11.9-4.5-2.04 2.03-.02 7.37 4.5 11.9 4.52 4.52 9.87 6.54 11.9 4.5Z" }], ["path", { d: "M15.7 15.7c4.52-4.52 6.54-9.87 4.5-11.9-2.03-2.04-7.37-.02-11.9 4.5-4.52 4.52-6.54 9.87-4.5 11.9 2.03 2.04 7.37.02 11.9-4.5Z" }]],
    lightbulb: [["path", { d: "M15 14c.2-1 .7-1.7 1.5-2.5 1-.9 1.5-2.2 1.5-3.5A6 6 0 0 0 6 8c0 1 .2 2.2 1.5 3.5.7.7 1.3 1.5 1.5 2.5" }], ["path", { d: "M9 18h6" }], ["path", { d: "M10 22h4" }]],
    brain: [["path", { d: "M9.5 4A2.5 2.5 0 0 1 12 6.5v11a2.5 2.5 0 0 1-4.96.44A2.5 2.5 0 0 1 5.5 13a3 3 0 0 1 .34-5.98A2.5 2.5 0 0 1 9.5 4Z" }], ["path", { d: "M14.5 4A2.5 2.5 0 0 0 12 6.5v11a2.5 2.5 0 0 0 4.96.44A2.5 2.5 0 0 0 18.5 13a3 3 0 0 0-.34-5.98A2.5 2.5 0 0 0 14.5 4Z" }]],
    check: [["path", { d: "M20 6 9 17l-5-5" }]],
    "chevron-down": [["path", { d: "m6 9 6 6 6-6" }]],
    terminal: [["polyline", { points: "4 17 10 11 4 5" }], ["line", { x1: "12", x2: "20", y1: "19", y2: "19" }]],
    target: [["circle", { cx: "12", cy: "12", r: "10" }], ["circle", { cx: "12", cy: "12", r: "6" }], ["circle", { cx: "12", cy: "12", r: "2" }]],
    bot: [["path", { d: "M12 8V4H8" }], ["rect", { x: "4", y: "8", width: "16", height: "12", rx: "2" }], ["path", { d: "M2 14h2" }], ["path", { d: "M20 14h2" }], ["path", { d: "M15 13v2" }], ["path", { d: "M9 13v2" }]],
    "book-open": [["path", { d: "M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" }], ["path", { d: "M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" }]],
    image: [["rect", { x: "3", y: "3", width: "18", height: "18", rx: "2", ry: "2" }], ["circle", { cx: "9", cy: "9", r: "2" }], ["path", { d: "m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" }]],
    smile: [["circle", { cx: "12", cy: "12", r: "10" }], ["path", { d: "M8 14s1.5 2 4 2 4-2 4-2" }], ["line", { x1: "9", x2: "9.01", y1: "9", y2: "9" }], ["line", { x1: "15", x2: "15.01", y1: "9", y2: "9" }]],
    "alarm-clock": [["circle", { cx: "12", cy: "13", r: "8" }], ["path", { d: "M12 9v4l2 2" }], ["path", { d: "M5 3 2 6" }], ["path", { d: "m22 6-3-3" }], ["path", { d: "M6.38 18.7 4 21" }], ["path", { d: "M17.64 18.67 20 21" }]],
    clipboard: [["rect", { x: "8", y: "2", width: "8", height: "4", rx: "1", ry: "1" }], ["path", { d: "M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" }]],
    calculator: [["rect", { x: "4", y: "2", width: "16", height: "20", rx: "2" }], ["line", { x1: "8", x2: "16", y1: "6", y2: "6" }], ["line", { x1: "16", x2: "16", y1: "14", y2: "18" }], ["path", { d: "M16 10h.01" }], ["path", { d: "M12 10h.01" }], ["path", { d: "M8 10h.01" }], ["path", { d: "M12 14h.01" }], ["path", { d: "M8 14h.01" }], ["path", { d: "M12 18h.01" }], ["path", { d: "M8 18h.01" }]],
    search: [["circle", { cx: "11", cy: "11", r: "8" }], ["path", { d: "m21 21-4.3-4.3" }]],
    wallet: [["path", { d: "M19 7V4a1 1 0 0 0-1-1H5a2 2 0 0 0 0 4h15a1 1 0 0 1 1 1v4h-3a2 2 0 0 0 0 4h3a1 1 0 0 0 1-1v-2a1 1 0 0 0-1-1" }], ["path", { d: "M3 5v14a2 2 0 0 0 2 2h15a1 1 0 0 0 1-1v-4" }]],

    puzzle: [["path", { d: "M19.439 7.85c-.049.322.059.648.289.878l1.568 1.568c.47.47.706 1.087.706 1.704s-.235 1.233-.706 1.704l-1.611 1.611a.98.98 0 0 1-.837.276c-.47-.07-.802-.48-.968-.925a2.501 2.501 0 1 0-3.214 3.214c.446.166.855.497.925.968a.979.979 0 0 1-.276.837l-1.61 1.61a2.404 2.404 0 0 1-1.705.707 2.402 2.402 0 0 1-1.704-.706l-1.568-1.568a1.026 1.026 0 0 0-.877-.29c-.493.074-.84.504-1.02.968a2.5 2.5 0 1 1-3.237-3.237c.464-.18.894-.527.967-1.02a1.026 1.026 0 0 0-.289-.877l-1.568-1.568A2.402 2.402 0 0 1 1.998 12c0-.617.236-1.234.706-1.704L4.23 8.77c.24-.24.581-.353.917-.303.515.077.877.528 1.073 1.01a2.5 2.5 0 1 0 3.259-3.259c-.482-.196-.933-.558-1.01-1.073-.05-.336.062-.676.303-.917l1.525-1.525A2.402 2.402 0 0 1 12 1.998c.617 0 1.234.236 1.704.706l1.568 1.568c.23.23.556.338.877.29.493-.074.84-.504 1.02-.968a2.5 2.5 0 1 1 3.237 3.237c-.464.18-.894.527-.967 1.02Z" }]],
    package: [["path", { d: "m7.5 4.27 9 5.15" }], ["path", { d: "M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z" }], ["path", { d: "m3.3 7 8.7 5 8.7-5" }], ["path", { d: "M12 22V12" }]],
    sparkles: [["path", { d: "M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z" }], ["path", { d: "M20 3v4" }], ["path", { d: "M22 5h-4" }]],
    code: [["polyline", { points: "16 18 22 12 16 6" }], ["polyline", { points: "8 6 2 12 8 18" }]],
    arch: [["path", { d: "M12 2c-.9 2.3-1.5 3.8-2.6 5.9.7.7 1.5 1.5 2.8 2.4-1.4-.6-2.4-1.2-3.2-1.8C7.5 11.6 5.2 16 2 22c2.9-1.7 5.4-2.8 7.7-3.3.6-2.1 1.4-3.2 2.3-3.2s1.7 1.1 2.3 3.2c2.3.5 4.8 1.6 7.7 3.3-3.2-6-5.5-10.4-7-13.5-.8.6-1.8 1.2-3.2 1.8 1.3-.9 2.1-1.7 2.8-2.4C13.5 5.8 12.9 4.3 12 2z", fill: "currentColor", stroke: "none" }]],
    "chevron-left": [["path", { d: "m15 18-6-6 6-6" }]],
    "layout-grid": [["rect", { x: "3", y: "3", width: "7", height: "7", rx: "1" }], ["rect", { x: "14", y: "3", width: "7", height: "7", rx: "1" }], ["rect", { x: "14", y: "14", width: "7", height: "7", rx: "1" }], ["rect", { x: "3", y: "14", width: "7", height: "7", rx: "1" }]],
    "chart-column": [["path", { d: "M3 3v16a2 2 0 0 0 2 2h16" }], ["path", { d: "M7 15v-4m5 4V8m5 7v-6" }]],
    "chevron-right": [["path", { d: "m9 18 6-6-6-6" }]],
    // 目标状态行用（lucide: target / pause / play / x）
    "target": [["circle", { cx: "12", cy: "12", r: "10" }], ["circle", { cx: "12", cy: "12", r: "6" }], ["circle", { cx: "12", cy: "12", r: "2" }]],
    "pause": [["rect", { x: "14", y: "4", width: "4", height: "16", rx: "1" }], ["rect", { x: "6", y: "4", width: "4", height: "16", rx: "1" }]],
    "play": [["polygon", { points: "6 3 20 12 6 21 6 3" }]],
    "x": [["path", { d: "M18 6 6 18" }], ["path", { d: "m6 6 12 12" }]],
    "pin": [["path", { d: "M12 17v5" }], ["path", { d: "M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" }]],
    "undo-2": [["path", { d: "M9 14 4 9l5-5" }], ["path", { d: "M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5a5.5 5.5 0 0 1-5.5 5.5H11" }]],
    "circle-alert": [["circle", { cx: "12", cy: "12", r: "10" }], ["line", { x1: "12", x2: "12", y1: "8", y2: "12" }], ["line", { x1: "12", x2: "12.01", y1: "16", y2: "16" }]],
    "circle-help": [["circle", { cx: "12", cy: "12", r: "10" }], ["path", { d: "M9.09 9a3 3 0 1 1 5.83 1c0 2-3 3-3 3" }], ["path", { d: "M12 17h.01" }]],
    "circle-stop": [["circle", { cx: "12", cy: "12", r: "10" }], ["rect", { width: "6", height: "6", x: "9", y: "9", rx: "1" }]],
    "cloud-sun": [["path", { d: "M12 2v2" }], ["path", { d: "m4.93 4.93 1.41 1.41" }], ["path", { d: "M20 12h2" }], ["path", { d: "m19.07 4.93-1.41 1.41" }], ["path", { d: "M16 6a4 4 0 0 0-3.46 6" }], ["path", { d: "M17.5 19H9a4 4 0 1 1 3.68-5.57A3 3 0 1 1 17.5 19Z" }]],
    copy: [["rect", { width: "14", height: "14", x: "8", y: "8", rx: "2", ry: "2" }], ["path", { d: "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" }]],
    "code-2": [["path", { d: "m18 16 4-4-4-4" }], ["path", { d: "m6 8-4 4 4 4" }], ["path", { d: "m14.5 4-5 16" }]],
    download: [["path", { d: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" }], ["polyline", { points: "7 10 12 15 17 10" }], ["line", { x1: "12", x2: "12", y1: "15", y2: "3" }]],
    "dollar-sign": [["line", { x1: "12", x2: "12", y1: "2", y2: "22" }], ["path", { d: "M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6" }]],
    ellipsis: [["circle", { cx: "12", cy: "12", r: "1" }], ["circle", { cx: "19", cy: "12", r: "1" }], ["circle", { cx: "5", cy: "12", r: "1" }]],
    eye: [["path", { d: "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0" }], ["circle", { cx: "12", cy: "12", r: "3" }]],
    "external-link": [["path", { d: "M15 3h6v6" }], ["path", { d: "M10 14 21 3" }], ["path", { d: "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" }]],
    folder: [["path", { d: "M3 6a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" }]],
    globe: [["circle", { cx: "12", cy: "12", r: "10" }], ["path", { d: "M2 12h20" }], ["path", { d: "M12 2a15.3 15.3 0 0 1 0 20" }], ["path", { d: "M12 2a15.3 15.3 0 0 0 0 20" }]],
    "file-text": [["path", { d: "M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z" }], ["polyline", { points: "14 2 14 8 20 8" }], ["line", { x1: "8", x2: "16", y1: "13", y2: "13" }], ["line", { x1: "8", x2: "16", y1: "17", y2: "17" }]],
    "trash-2": [["path", { d: "M3 6h18" }], ["path", { d: "M8 6V4h8v2" }], ["path", { d: "M19 6 18 20H6L5 6" }], ["path", { d: "M10 11v5" }], ["path", { d: "M14 11v5" }]],
    lightbulb: [["path", { d: "M9 18h6" }], ["path", { d: "M10 22h4" }], ["path", { d: "M15.09 14c.18-.59.59-1.05 1.05-1.52A6 6 0 1 0 7.86 12.5c.45.44.85.9 1.03 1.5" }], ["path", { d: "M9 14h6v1a3 3 0 0 1-6 0v-1Z" }]],
    "list-todo": [["rect", { x: "3", y: "5", width: "6", height: "6", rx: "1" }], ["path", { d: "m3 17 2 2 4-4" }], ["path", { d: "M13 6h8" }], ["path", { d: "M13 12h8" }], ["path", { d: "M13 18h8" }]],
    "loader-circle": [["path", { d: "M21 12a9 9 0 1 1-6.219-8.56" }]],
    "log-out": [["path", { d: "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" }], ["polyline", { points: "16 17 21 12 16 7" }], ["line", { x1: "21", x2: "9", y1: "12", y2: "12" }]],
    ticket: [["path", { d: "M2 9a3 3 0 0 1 0 6v2a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-2a3 3 0 0 1 0-6V7a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2Z" }], ["path", { d: "M13 5v2" }], ["path", { d: "M13 17v2" }], ["path", { d: "M13 11v2" }]],
    user: [["path", { d: "M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2" }], ["circle", { cx: "12", cy: "7", r: "4" }]],
    "user-plus": [["path", { d: "M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" }], ["circle", { cx: "9", cy: "7", r: "4" }], ["line", { x1: "19", x2: "19", y1: "8", y2: "14" }], ["line", { x1: "22", x2: "16", y1: "11", y2: "11" }]],
    "lock-keyhole": [["circle", { cx: "12", cy: "16", r: "1" }], ["rect", { x: "3", y: "10", width: "18", height: "12", rx: "2" }], ["path", { d: "M7 10V7a5 5 0 0 1 10 0v3" }]],
    "log-in": [["path", { d: "M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4" }], ["polyline", { points: "10 17 15 12 10 7" }], ["line", { x1: "15", x2: "3", y1: "12", y2: "12" }]],
    "message-circle": [["path", { d: "M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z" }]],
    // 跨会话消息(09-23):收到的那条挂铃铛,发出去的那张工具签挂纸飞机。
    bell: [["path", { d: "M10.268 21a2 2 0 0 0 3.464 0" }], ["path", { d: "M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326" }]],
    send: [["path", { d: "M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z" }], ["path", { d: "m21.854 2.147-10.94 10.939" }]],
    "messages-square": [["path", { d: "M14 9a2 2 0 0 1-2 2H6l-4 4V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2z" }], ["path", { d: "M18 9h2a2 2 0 0 1 2 2v10l-4-4h-6a2 2 0 0 1-2-2v-1" }]],
    moon: [["path", { d: "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" }]],
    "image-search": [["rect", { x: "3", y: "3", width: "14", height: "14", rx: "2" }], ["circle", { cx: "11", cy: "9", r: "2" }], ["path", { d: "m3 15 4-4 5 5" }], ["circle", { cx: "18", cy: "18", r: "3" }], ["path", { d: "m20.2 20.2 1.8 1.8" }]],
    image: [["rect", { x: "3", y: "3", width: "18", height: "18", rx: "2" }], ["circle", { cx: "8.5", cy: "8.5", r: "1.5" }], ["path", { d: "m21 15-5-5L5 21" }]],
    "file-video": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "m10 12.5 4 2.5-4 2.5z" }]],
    "file-audio": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "M15 12v5" }], ["path", { d: "M15 12l-4 1v5" }], ["circle", { cx: "9.5", cy: "18", r: "1.5" }], ["circle", { cx: "13.5", cy: "17", r: "1.5" }]],
    "file-pdf": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "M8 18v-5h1.5a1.5 1.5 0 0 1 0 3H8" }], ["path", { d: "M13 18v-5h1a2 2 0 0 1 0 5z" }], ["path", { d: "M18 13h-2v5" }], ["path", { d: "M16 15.5h1.5" }]],
    "file-archive": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "M9 6h1" }], ["path", { d: "M9 9h1" }], ["path", { d: "M9 12h1" }], ["rect", { x: "8", y: "15", width: "3", height: "4", rx: "1" }]],
    "file-code": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "m10 13-2 2 2 2" }], ["path", { d: "m14 13 2 2-2 2" }]],
    "file-markdown": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "M8 16v-4l2 2 2-2v4" }], ["path", { d: "M15 12v4" }]],
    "file-json": [["path", { d: "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" }], ["path", { d: "M14 2v6h6" }], ["path", { d: "M8 12h1a1 1 0 0 1 0 2H8v2h1a1 1 0 0 1 0 2H8" }], ["path", { d: "M16 12h-1a1 1 0 0 0 0 2h1v2h-1" }]],
    "maximize-2": [["path", { d: "M15 3h6v6" }], ["path", { d: "m21 3-7 7" }], ["path", { d: "m3 21 7-7" }], ["path", { d: "M9 21H3v-6" }]],
    "minimize-2": [["path", { d: "m14 10 7-7" }], ["path", { d: "M20 10h-6V4" }], ["path", { d: "m3 21 7-7" }], ["path", { d: "M4 14h6v6" }]],
    paintbrush: [["path", { d: "m14.622 17.897-10.68-2.913" }], ["path", { d: "M18.376 2.622a1 1 0 0 1 3.002 3.002L17.36 9.642a2 2 0 0 1-2.121.447l-2.741-1.02a1 1 0 0 1-.583-.583l-1.02-2.741a2 2 0 0 1 .447-2.121Z" }], ["path", { d: "M9 8c-1.804.716-3.5 2.5-3.5 4.5 0 .6.4 1 1 1 2 0 3.784-1.696 4.5-3.5" }]],
    "panel-left": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M9 3v18" }]],
    "panel-left-close": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M9 3v18" }], ["path", { d: "m15 9-3 3 3 3" }]],
    "panel-left-open": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M9 3v18" }], ["path", { d: "m12 9 3 3-3 3" }]],
    "panel-right": [["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2" }], ["path", { d: "M15 3v18" }]],
    paperclip: [["path", { d: "m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" }]],
    mic: [["path", { d: "M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z" }], ["path", { d: "M19 10v2a7 7 0 0 1-14 0v-2" }], ["line", { x1: "12", x2: "12", y1: "19", y2: "22" }]],
    "refresh-cw": [["path", { d: "M21 12a9 9 0 0 0-15.35-6.35L3 8" }], ["path", { d: "M3 3v5h5" }], ["path", { d: "M3 12a9 9 0 0 0 15.35 6.35L21 16" }], ["path", { d: "M16 16h5v5" }]],
    route: [["circle", { cx: "6", cy: "19", r: "3" }], ["path", { d: "M9 19h8.5a3.5 3.5 0 0 0 0-7h-11a3.5 3.5 0 0 1 0-7H15" }], ["circle", { cx: "18", cy: "5", r: "3" }]],
    "settings-2": [["path", { d: "M20 7h-9" }], ["path", { d: "M14 17H5" }], ["circle", { cx: "17", cy: "17", r: "3" }], ["circle", { cx: "7", cy: "7", r: "3" }]],
    "sliders-horizontal": [["line", { x1: "21", x2: "14", y1: "4", y2: "4" }], ["line", { x1: "10", x2: "3", y1: "4", y2: "4" }], ["line", { x1: "21", x2: "12", y1: "12", y2: "12" }], ["line", { x1: "8", x2: "3", y1: "12", y2: "12" }], ["line", { x1: "21", x2: "16", y1: "20", y2: "20" }], ["line", { x1: "12", x2: "3", y1: "20", y2: "20" }], ["line", { x1: "14", x2: "14", y1: "2", y2: "6" }], ["line", { x1: "8", x2: "8", y1: "10", y2: "14" }], ["line", { x1: "16", x2: "16", y1: "18", y2: "22" }]],
    sparkles: [["path", { d: "m12 3-1.9 5.8a2 2 0 0 1-1.3 1.3L3 12l5.8 1.9a2 2 0 0 1 1.3 1.3L12 21l1.9-5.8a2 2 0 0 1 1.3-1.3L21 12l-5.8-1.9a2 2 0 0 1-1.3-1.3Z" }], ["path", { d: "M5 3v4" }], ["path", { d: "M19 17v4" }], ["path", { d: "M3 5h4" }], ["path", { d: "M17 19h4" }]],
    smile: [["circle", { cx: "12", cy: "12", r: "9" }], ["path", { d: "M8 14s1.5 2 4 2 4-2 4-2" }], ["path", { d: "M9 9h.01" }], ["path", { d: "M15 9h.01" }]],
    "stop-square": [["rect", { x: "6", y: "6", width: "12", height: "12", rx: "2", fill: "currentColor", stroke: "none" }]],
    "square-pen": [["path", { d: "M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" }], ["path", { d: "M18.37 2.63a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4Z" }]],
    sun: [["circle", { cx: "12", cy: "12", r: "4" }], ["path", { d: "M12 2v2" }], ["path", { d: "M12 20v2" }], ["path", { d: "m4.93 4.93 1.42 1.42" }], ["path", { d: "m17.66 17.66 1.41 1.41" }], ["path", { d: "M2 12h2" }], ["path", { d: "M20 12h2" }], ["path", { d: "m6.34 17.66-1.41 1.41" }], ["path", { d: "m19.07 4.93-1.41 1.41" }]],
    "sun-moon": [["path", { d: "M12 8a2.83 2.83 0 0 0 4 4 4 4 0 1 1-4-4" }], ["path", { d: "M12 2v2" }], ["path", { d: "M12 20v2" }], ["path", { d: "m4.9 4.9 1.4 1.4" }], ["path", { d: "m17.7 17.7 1.4 1.4" }], ["path", { d: "M2 12h2" }], ["path", { d: "M20 12h2" }], ["path", { d: "m6.3 17.7-1.4 1.4" }], ["path", { d: "m19.1 4.9-1.4 1.4" }]],
    "triangle-alert": [["path", { d: "m21.73 18-8-14a2 2 0 0 0-3.46 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3" }], ["path", { d: "M12 9v4" }], ["path", { d: "M12 17h.01" }]],
    wrench: [["path", { d: "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94z" }]],
    "zoom-in": [["circle", { cx: "11", cy: "11", r: "8" }], ["path", { d: "m21 21-4.3-4.3" }], ["path", { d: "M11 8v6" }], ["path", { d: "M8 11h6" }]],
    "zoom-out": [["circle", { cx: "11", cy: "11", r: "8" }], ["path", { d: "m21 21-4.3-4.3" }], ["path", { d: "M8 11h6" }]],
    x: [["path", { d: "M18 6 6 18" }], ["path", { d: "m6 6 12 12" }]]
  };

  const EVENT_NAMES = [
    "run.started",
    "turn.started",
    "assistant.delta",
    "reasoning.start",
    "reasoning.reset",
    "reasoning.part_start",
    "reasoning.part_end",
    "reasoning.title",
    "reasoning.delta",
    "tool.started",
    "tool.preparing",
    "tool.progress",
    "subagent.progress",
    "tool.output",
    "tool.image",
    "tool.artifact",
    "tool.finished",
    "question.requested",
    "question.answered",
    "question.closed",
    "context.compact_start",
    "context.compact_delta",
    "context.compact_end",
    "context.pop_start",
    "context.pop_end",
    "context.error",
    "queue.added",
    "queue.removed",
    "queue.consumed",
    "generation.superseded",
    "chat.round_usage",
    "run.completed",
    "run.cancelled",
    "run.failed",
    "conversation.reset",
    "conversation.pop",
    "conversation.compacted",
    "session.created",
    "session.renamed",
    "session.deleted",
    "session.current_changed",
    "session.updated",
    "session.reordered",
    "job.started",
    "job.finished",
    "job.acknowledged",
    "job.progress",
    "resync_required"
  ];

  const RUN_EVENTS = new Set(EVENT_NAMES.filter((name) => !name.startsWith("session.") && !name.startsWith("job.") && !["conversation.reset", "conversation.pop", "resync_required", "queue.added", "queue.removed"].includes(name)));

  const elements = {
    body: document.body,
    appShell: document.getElementById("appShell"),
    mainStage: document.getElementById("mainStage"),
    sidebar: document.getElementById("sidebar"),
    sidebarScrim: document.getElementById("sidebarScrim"),
    sidebarClose: document.getElementById("sidebarClose"),
    sidebarCollapseButton: document.getElementById("sidebarCollapseButton"),
    sidebarExpandButton: document.getElementById("sidebarExpandButton"),
    mobileMenuButton: document.getElementById("mobileMenuButton"),
    sidebarStatusDot: document.getElementById("sidebarStatusDot"),
    sidebarConnectionStatus: document.getElementById("sidebarConnectionStatus"),
    newChatButton: document.getElementById("newChatButton"),
    matugenThemeLink: document.getElementById("matugenThemeLink"),
    languageSeg: document.getElementById("languageSeg"),
    reasoningExpandToggle: document.getElementById("reasoningExpandToggle"),
    toolExpandToggle: document.getElementById("toolExpandToggle"),
    procCollapseToggle: document.getElementById("procCollapseToggle"),
    sessionList: document.getElementById("sessionList"),
    sessionItems: document.getElementById("sessionItems"),
    contextNumbers: document.getElementById("contextNumbers"),
    contextTrack: document.getElementById("contextTrack"),
    contextBar: document.getElementById("contextBar"),
    contextRing: document.getElementById("contextRing"),
    composerSpeed: document.getElementById("composerSpeed"),
    composerSpeedValue: document.getElementById("composerSpeedValue"),
    composerCumulative: document.getElementById("composerCumulative"),
    composerCumulativeValue: document.getElementById("composerCumulativeValue"),
    composerTimer: document.getElementById("composerTimer"),
    composerTimerValue: document.getElementById("composerTimerValue"),
    consoleButton: document.getElementById("consoleButton"),
    sidebarSettingsButton: document.getElementById("sidebarSettingsButton"),
    consoleView: document.getElementById("consoleView"),
    consoleBack: document.getElementById("consoleBack"),
    conRailToggle: document.getElementById("conRailToggle"),
    usageStamp: document.getElementById("usageStamp"),
    usageRangeSeg: document.getElementById("usageRangeSeg"),
    usageTiles: document.getElementById("usageTiles"),
    usageHeatmap: document.getElementById("usageHeatmap"),
    usageHeatMonths: document.getElementById("usageHeatMonths"),
    usageHeatTotal: document.getElementById("usageHeatTotal"),
    usageBars: document.getElementById("usageBars"),
    usageBarsX: document.getElementById("usageBarsX"),
    usageBarsY: document.getElementById("usageBarsY"),
    usageBarsHint: document.getElementById("usageBarsHint"),
    usageSources: document.getElementById("usageSources"),
    usageRecords: document.getElementById("usageRecords"),
    usageRefresh: document.getElementById("usageRefresh"),
    usageClear: document.getElementById("usageClear"),
    usageSrcFilter: document.getElementById("usageSrcFilter"),
    usageModelFilter: document.getElementById("usageModelFilter"),
    sidebarThemeButton: document.getElementById("sidebarThemeButton"),
    brandAvatar: document.getElementById("brandAvatar"),
    brandName: document.getElementById("brandName"),
    modelMenuWrap: document.getElementById("modelMenuWrap"),
    modelButton: document.getElementById("modelButton"),
    modelLabel: document.getElementById("modelLabel"),
    modelMenu: document.getElementById("modelMenu"),
    artifactToggleButton: document.getElementById("artifactToggleButton"),
    artifactWorkspace: document.getElementById("artifactWorkspace"),
    artifactResizeHandle: document.getElementById("artifactResizeHandle"),
    artifactCloseButton: document.getElementById("artifactCloseButton"),
    artifactTitle: document.getElementById("artifactTitle"),
    artifactTypeLabel: document.getElementById("artifactTypeLabel"),
    artifactTitleButton: document.getElementById("artifactTitleButton"),
    artifactResourceMenu: document.getElementById("artifactResourceMenu"),
    artifactPreviewButton: document.getElementById("artifactPreviewButton"),
    artifactSourceButton: document.getElementById("artifactSourceButton"),
    artifactImageActions: document.getElementById("artifactImageActions"),
    artifactImageExternalButton: document.getElementById("artifactImageExternalButton"),
    artifactImageZoomOutButton: document.getElementById("artifactImageZoomOutButton"),
    artifactImageZoomInButton: document.getElementById("artifactImageZoomInButton"),
    artifactCopyButton: document.getElementById("artifactCopyButton"),
    artifactDownloadButton: document.getElementById("artifactDownloadButton"),
    artifactMaximizeButton: document.getElementById("artifactMaximizeButton"),
    artifactView: document.getElementById("artifactView"),
    errorRegion: document.getElementById("errorRegion"),
    chatScroll: document.getElementById("chatScroll"),
    loadingState: document.getElementById("loadingState"),
    blockedState: document.getElementById("blockedState"),
    blockedTitle: document.getElementById("blockedTitle"),
    blockedMessage: document.getElementById("blockedMessage"),
    loginForm: document.getElementById("loginForm"),
    loginUsername: document.getElementById("loginUsername"),
    loginPassword: document.getElementById("loginPassword"),
    showRegisterButton: document.getElementById("showRegisterButton"),
    showLoginButton: document.getElementById("showLoginButton"),
    registerForm: document.getElementById("registerForm"),
    setupForm: document.getElementById("setupForm"),
    setupUsername: document.getElementById("setupUsername"),
    setupDisplayName: document.getElementById("setupDisplayName"),
    setupPassword: document.getElementById("setupPassword"),
    setupPassword2: document.getElementById("setupPassword2"),
    setupError: document.getElementById("setupError"),
    setupSubmit: document.getElementById("setupSubmit"),
    registerInvite: document.getElementById("registerInvite"),
    registerUsername: document.getElementById("registerUsername"),
    registerDisplayName: document.getElementById("registerDisplayName"),
    registerPassword: document.getElementById("registerPassword"),
    registerError: document.getElementById("registerError"),
    registerSubmit: document.getElementById("registerSubmit"),
    registerSubmitLabel: document.getElementById("registerSubmitLabel"),
    accountStamp: document.getElementById("accountStamp"),
    accountSelfHint: document.getElementById("accountSelfHint"),
    accountUsername: document.getElementById("accountUsername"),
    accountDisplayName: document.getElementById("accountDisplayName"),
    accountCurrentPassword: document.getElementById("accountCurrentPassword"),
    accountNewPassword: document.getElementById("accountNewPassword"),
    accountProfile: document.getElementById("accountProfile"),
    accountSave: document.getElementById("accountSave"),
    accountLogout: document.getElementById("accountLogout"),
    accountError: document.getElementById("accountError"),
    inviteCreate: document.getElementById("inviteCreate"),
    inviteFresh: document.getElementById("inviteFresh"),
    inviteRows: document.getElementById("inviteRows"),
    personaList: document.getElementById("personaList"),
    personaCreate: document.getElementById("personaCreate"),
    oobe: document.getElementById("oobe"),
    oobeSteps: document.getElementById("oobeSteps"),
    oobeSkip: document.getElementById("oobeSkip"),
    oobePanes: document.getElementById("oobePanes"),
    oobePersonaForm: document.getElementById("oobePersonaForm"),
    oobeAvatarInput: document.getElementById("oobeAvatarInput"),
    oobeAvatarPreview: document.getElementById("oobeAvatarPreview"),
    oobeName: document.getElementById("oobeName"),
    oobeDesc: document.getElementById("oobeDesc"),
    oobePrompt: document.getElementById("oobePrompt"),
    oobeSharedName: document.getElementById("oobeSharedName"),
    oobeSharedHint: document.getElementById("oobeSharedHint"),
    oobePlugins: document.getElementById("oobePlugins"),
    oobeProfile: document.getElementById("oobeProfile"),
    oobeDoneAvatar: document.getElementById("oobeDoneAvatar"),
    oobeDoneTitle: document.getElementById("oobeDoneTitle"),
    oobeDoneText: document.getElementById("oobeDoneText"),
    oobeError: document.getElementById("oobeError"),
    oobeBack: document.getElementById("oobeBack"),
    oobeNext: document.getElementById("oobeNext"),
    oobeNextLabel: document.getElementById("oobeNextLabel"),
    accountRows: document.getElementById("accountRows"),
    loginError: document.getElementById("loginError"),
    loginSubmit: document.getElementById("loginSubmit"),
    loginSubmitLabel: document.getElementById("loginSubmitLabel"),
    retryBootstrapButton: document.getElementById("retryBootstrapButton"),
    timeline: document.getElementById("timeline"),
    conversationStage: document.getElementById("conversationStage"),
    emptyState: document.getElementById("emptyState"),
    emptyVisual: document.getElementById("emptyVisual"),
    emptyBoardImage: document.getElementById("emptyBoardImage"),
    emptyKickerName: document.getElementById("emptyKickerName"),
    emptyTitle: document.getElementById("emptyTitle"),
    emptySubtitle: document.getElementById("emptySubtitle"),
    promptGrid: document.getElementById("promptGrid"),
    jumpBottomButton: document.getElementById("jumpBottomButton"),
    composerDock: document.getElementById("composerDock"),
    stageTodos: document.getElementById("stageTodos"),
    modelLevelMenu: document.getElementById("modelLevelMenu"),
    composerRunIndicator: document.getElementById("composerRunIndicator"),
    jobsStrip: document.getElementById("jobsStrip"),
    goalBar: document.getElementById("goalBar"),
    liveStopRail: document.getElementById("liveStopRail"),
    questionDock: document.getElementById("questionDock"),
    composerForm: document.getElementById("composerForm"),
    composerInput: document.getElementById("composerInput"),
    attachmentTray: document.getElementById("attachmentTray"),
    attachmentInput: document.getElementById("attachmentInput"),
    attachButton: document.getElementById("attachButton"),
    micButton: document.getElementById("micButton"),
    voiceIndicator: document.getElementById("voiceIndicator"),
    voiceLevel: document.getElementById("voiceLevel"),
    queueTray: document.getElementById("queueTray"),
    composerState: document.getElementById("composerState"),
    characterCount: document.getElementById("characterCount"),
    sendButton: document.getElementById("sendButton"),
    settingsNav: document.querySelector(".settings-nav"),
    settingsPanels: Array.from(document.querySelectorAll("[data-settings-panel]")),
    settingsModelMark: document.getElementById("settingsModelMark"),
    settingsModelName: document.getElementById("settingsModelName"),
    settingsModelProvider: document.getElementById("settingsModelProvider"),
    capabilityList: document.getElementById("capabilityList"),
    versionLabel: document.getElementById("versionLabel"),
    advancedConfigEditor: document.getElementById("advancedConfigEditor"),
    applyAdvancedConfigButton: document.getElementById("applyAdvancedConfigButton"),
    reloadConfigButton: document.getElementById("reloadConfigButton"),
    saveConfigButton: document.getElementById("saveConfigButton"),
    settingsStatus: document.getElementById("settingsStatus"),
    settingsFooter: document.getElementById("settingsFooter"),
    toastRegion: document.getElementById("toastRegion"),
    resetDialog: document.getElementById("resetDialog"),
    popDialog: document.getElementById("popDialog"),
    popDialogList: document.getElementById("popDialogList"),
    popDialogAll: document.getElementById("popDialogAll"),
    popConfirmButton: document.getElementById("popConfirmButton"),
    resetCancelButton: document.getElementById("resetCancelButton"),
    resetConfirmButton: document.getElementById("resetConfirmButton")
  };

  const state = {
    // 输入框下面那排的「用时」(09-24):{ runId, startedAt };这个视图没有在跑的轮就是 null。
    turnClock: null,
    backgroundJobs: new Map(),
    jobsStripOpen: localStorage.getItem("miyu.web.jobsStripOpen") === "1",
    expandedJobs: new Set(),
    // 后台子代理任务此刻的样子(`job.progress` 里收好的窥视、词元、子会话,会话项目第 4 段
    // 之二):任务条那一行取它画,点它打开子会话。
    subagentJobs: new Map(),
    commandLogs: new Map(),
    commandPeekLine: new Map(),
    commandPeekTimers: new Map(),
    bootId: null,
    buildId: null,
    latestEventId: 0,
    lastEventId: 0,
    replayRunIds: null,
    replayCutoff: 0,
    replayResyncCount: 0,
    replayResyncAt: 0,
    turns: [],
    // 按页取回合（会话项目第 2 段）：往前翻的游标、前面那些回合的用量合计、会话第一句。
    turnsOlder: null,
    turnsBase: null,
    firstUserContent: "",
    loadingOlderTurns: false,
    queuedPrompts: [],
    models: [],
    persona: {
      name: "Miyu",
      avatar_url: "/assets/miyu-logo.png",
      board_image_url: "/assets/miyuwallpaper.png",
      board_title: DEFAULT_BOARD_TITLE,
      board_subtitle: DEFAULT_BOARD_SUBTITLE,
      composer_placeholder: defaultComposerPlaceholder("Miyu"),
      starter_prompts: DEFAULT_STARTER_PROMPTS
    },
    sessions: [],
    currentSessionId: null,
    viewSessionId: null,
    viewRunningTurnId: null,
    viewLoading: false,
    // 正在切往的会话:点击标签的瞬间就高亮它、并铺一层加载动画,等 turns 拉回来
    // 再真正应用视图(09-12 用户报「先加载后切换、点大会话像卡住」)。
    switchingToSessionId: "",
    viewLoadGeneration: 0,
    viewSyncTimer: null,
    runsBySession: new Map(),
    // 跑完了、但用户还没切进去看过的会话。
    // 「完成」不是能持续的状态（否则每个会话都会永远挂着「已完成」），
    // 「未读」才是——产生于回合结束，消失于用户切进那个会话。
    unreadSessions: new Set(),
    liveRuns: new Map(),
    // 输入框「累计」合成用(#131):cumulativeBase = 后端给的会话实时累计基线;
    // liveSubagentTokens = 正在跑的子代理各自的实时 token 估算,按 tool_id 存。
    cumulativeBase: null,
    liveSubagentTokens: new Map(),
    sessionMenuFor: null,
    sessionRenaming: null,
    sessionDragId: null,
    lastReorderIds: "",
    modeChooserOpen: false,
    modeChooserKeyHandler: null,
    sessionBusy: false,
    display: {
      reasoning: "summary",
      tool_calls: "summary",
      readable_tool_names: true,
      command_output_lines: 8,
      thinking_scroll_lines: 10,
      cross_session_preview_lines: 10,
      mixed_model_endpoint_display: "interactive",
      show_mixed_model_endpoint: false
    },
    context: { tokens: 0, window: null },
    usage: {},
    capabilities: {},
    /// 登录者(阶段 5 多用户):{account_id, username, display_name, admin}。
    account: null,
    version: null,
    eventSource: null,
    connection: "connecting",
    blocked: false,
    adminBusy: false,
    loginSubmitting: false,
    modelSelectionSubmitting: false,
    stagedModelKeys: null,
    stagedFollowGlobal: false,
    stagedVariants: null,
    stageTodos: null,
    goal: null,
    goalGeneration: 0,
    stageTodosGeneration: 0,
    expandedLevelKey: null,
    modelMenuTouched: false,
    modelMenuError: "",
    sessionModelOverride: null,
    sessionModelOverrideFor: "",
    sessionModelOverrideToken: 0,
    // 这个会话钉住的思考档位(09-24:effort 做成会话级):modelKey → 档位名。没钉的模型
    // 跟着全局走(`thinkingVariantModels` 里的 selected)。
    sessionThinkingPins: new Map(),
    sessionThinkingPinsFor: "",
    sessionThinkingPinsToken: 0,
    submitting: false,
    revisionSubmitting: false,
    redoCandidate: null,
    revisionEditor: null,
    pendingSubmission: null,
    composerAttachments: [],
    artifacts: [],
    selectedArtifactId: null,
    artifactOpen: false,
    artifactRenderToken: 0,
    artifactZoom: 1,
    artifactPanX: 0,
    artifactPanY: 0,
    artifactMode: "preview",
    artifactMaximized: false,
    artifactWidthRatio: 0.5,
    artifactSourceCache: new Map(),
    // artifact 列表有两个来源：回合产出的 `turn.artifacts`（每次同步重建），
    // 和用户手动送进来的（气泡上点「在预览工作区打开」）。后者不在任何回合的
    // artifacts 里，光靠重建会在下一个回合到达时被整体覆盖掉——图片刚打开就
    // 没了。所以手动那批单独留一份，同步时并进去。
    //
    // 两份都按会话分。回合产出的天然分会话（同步喂进来的就是当前会话的
    // turns），这两份要是全局的，A 会话置顶的图会出现在 B 会话的列表里，
    // 在 A 里删掉的也会连累 B。
    pinnedArtifacts: new Map(),
    dismissedArtifactIds: new Map(),
    colorScheme: null,
    uiPrefs: {},
    matugenAvailable: null,
    reasoningExpanded: false,
    toolExpanded: false,
    // 过程自动收起:她一开口,前面那串思考+工具收成一行总结。默认开。
    procCollapse: true,
    finishedTurnArticles: new Map(),
    bootstrapPromise: null,
    resyncing: false,
    nearBottom: true,
    followOutput: true,
    programmaticScroll: false,
    settingsOpener: null,
    consolePanel: "usage",
    commandRunning: false,
    brailleFrame: 0,
    sidebarOpener: null,
    sidebarCollapsed: false,
    sidebarAutoCollapsed: false,
    toastTimer: null,
    modeAnimationTimer: null,
    healthTimer: null,
    terminalRunIds: new Set(),
    thinkingVariantModels: [],
    thinkingVariantLoading: false,
    thinkingVariantLoadGeneration: 0,
    thinkingVariantError: "",
    composing: false,
    settingsView: "interface",
    configLoaded: false,
    configLoading: false,
    configSaving: false,
    configDirty: false,
    configDraft: null,
    configOriginal: null,
    promptDraft: null,
    promptOriginal: null,
    secretStates: {},
    secretChanges: {},
    providerSecretStates: [],
    configMultimodalModels: [],
    configInferredImageModels: [],
    invalidConfigFields: new Map()
  };

  class ApiError extends Error {
    constructor(message, status) {
      super(message);
      this.name = "ApiError";
      this.status = status;
    }
  }

  function createIcon(name, className = "") {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    if (className) svg.setAttribute("class", className);
    const definition = ICONS[name] || ICONS["circle-alert"];
    for (const [tag, attributes] of definition) {
      const node = document.createElementNS(SVG_NS, tag);
      for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
      svg.appendChild(node);
    }
    return svg;
  }

  function renderIconSlots(root = document) {
    const slots = [];
    if (root instanceof Element && root.matches("[data-icon]")) slots.push(root);
    slots.push(...root.querySelectorAll("[data-icon]"));
    for (const slot of slots) {
      slot.replaceChildren(createIcon(slot.dataset.icon));
    }
  }

  function makeIconSlot(name, className = "") {
    const slot = document.createElement("span");
    slot.className = `icon-slot${className ? ` ${className}` : ""}`;
    // 图标名留在 DOM 上:走查要断言「视频附件用的是视频图标」,不然只能比 SVG
    // 路径字符串,那是一读就废的测试。
    slot.dataset.icon = name;
    slot.setAttribute("aria-hidden", "true");
    slot.appendChild(createIcon(name));
    return slot;
  }

  function safeStorageGet(key) {
    try {
      return window.localStorage.getItem(key);
    } catch (_) {
      return null;
    }
  }

  function safeStorageSet(key, value) {
    try {
      window.localStorage.setItem(key, value);
    } catch (_) {
      // Storage can be unavailable in hardened browser profiles.
    }
  }

  /*
   * 外观偏好存在 daemon 那边。localStorage 按 **origin** 隔离:
   * http://127.0.0.1:8300 和 http://192.168.1.7:8300 是两个源,同一台 Miyu 换个
   * 地址进来就是另一份主题——「Miyu 长什么样」不该跟着浏览器地址栏走。
   * 本地那份仍然写:它是首帧的即时值,服务端那份要等一个来回,先按本地上色能
   * 免掉一次闪烁。窗口尺寸相关的偏好(侧栏折叠、分栏比例)故意不同步,手机和
   * 台式机本来就该不一样。
   */
  const UI_PREF_KEYS = ["theme", "colorScheme", "chatFontSize", "reasoningExpanded", "toolExpanded", "procCollapse"];

  function saveUiPref(key, value) {
    if (!UI_PREF_KEYS.includes(key)) return;
    if (state.uiPrefs[key] === value) return;
    state.uiPrefs[key] = value;
    apiRequest("/api/ui-prefs", { method: "PUT", body: JSON.stringify({ [key]: value }) }).catch(() => {});
  }

  /** 登录之后拉一次服务端偏好并应用。失败就维持本地那份,不打扰用户。 */
  async function syncUiPrefs() {
    let prefs;
    try {
      prefs = await (await apiRequest("/api/ui-prefs")).json();
    } catch (_) {
      return;
    }
    if (!prefs || typeof prefs !== "object") return;
    // 先记下服务端的值:下面几个 setter 会走 saveUiPref,记过就不会再发回去。
    state.uiPrefs = { ...prefs };
    if (prefs.theme) setTheme(prefs.theme);
    if (prefs.colorScheme) setColorScheme(prefs.colorScheme);
    if (prefs.chatFontSize) setChatFontSize(prefs.chatFontSize);
    if (prefs.reasoningExpanded) setReasoningExpanded(prefs.reasoningExpanded === "true");
    if (prefs.toolExpanded) setToolExpanded(prefs.toolExpanded === "true");
    if (prefs.procCollapse) setProcCollapse(prefs.procCollapse === "true");
  }

  function setTheme(theme, persist = true) {
    const selected = theme === "linen" ? "linen" : "graphite";
    elements.body.dataset.theme = selected;
    document.querySelectorAll("[data-theme-choice]").forEach((button) => {
      button.classList.toggle("selected", button.dataset.themeChoice === selected);
      button.setAttribute("aria-pressed", String(button.dataset.themeChoice === selected));
    });
    const nextIcon = selected === "graphite" ? "sun" : "moon";
    for (const button of [elements.sidebarThemeButton]) {
      const slot = button.querySelector(".icon-slot");
      slot.replaceChildren(createIcon(nextIcon));
      button.title = selected === "graphite" ? t("切换到晨光主题") : t("切换到夜阑主题");
      button.setAttribute("aria-label", button.title);
    }
    const themeColor = document.querySelector('meta[name="theme-color"]');
    if (themeColor) themeColor.content = selected === "graphite" ? "#171821" : "#f6f0e2";
    if (persist) {
      safeStorageSet("miyu.web.theme", selected);
      saveUiPref("theme", selected);
    }
  }

  /*
   * 配色方案(与明暗正交):
   * - madobe  窗边预设(logo 派生 token,styles.css 内置)
   * - matugen 壁纸取色(后端 /theme.css 输出整套 MD3 token)
   * 通过禁用 /theme.css 的 <link> 切换,不改后端与 matugen 模板。
   */
  function setColorScheme(scheme, persist = true) {
    const requested = scheme === "madobe" ? "madobe" : "matugen";
    const selected = requested === "matugen" && state.matugenAvailable === false ? "madobe" : requested;
    state.colorScheme = selected;
    elements.body.dataset.colorScheme = selected;
    if (elements.matugenThemeLink) elements.matugenThemeLink.disabled = selected !== "matugen";
    document.querySelectorAll("[data-scheme-choice]").forEach((button) => {
      const active = button.dataset.schemeChoice === selected;
      button.classList.toggle("selected", active);
      button.setAttribute("aria-pressed", String(active));
      // 探测不到 matugen 输出时,「壁纸取色」整个选项不显示。
      if (button.dataset.schemeChoice === "matugen") button.hidden = state.matugenAvailable !== true;
    });
    if (persist) {
      safeStorageSet("miyu.web.colorScheme", requested);
      saveUiPref("colorScheme", requested);
    }
  }

  async function probeMatugenTheme() {
    try {
      const response = await fetch("/theme.css", { method: "HEAD", cache: "no-store" });
      state.matugenAvailable = response.ok;
    } catch (_) {
      state.matugenAvailable = false;
    }
    // 无持久化记录时:matugen 可用则维持现状(matugen),否则窗边。默认值不写入存储。
    setColorScheme(safeStorageGet("miyu.web.colorScheme") || (state.matugenAvailable ? "matugen" : "madobe"), false);
  }

  /* 仅 WebUI 的本地显示偏好(localStorage,不写入 config) */
  const CHAT_FONT_SIZES = ["14px", "15px", "16px"];

  function setChatFontSize(size, persist = true) {
    const selected = CHAT_FONT_SIZES.includes(size) ? size : "15px";
    document.documentElement.style.setProperty("--fs-chat", selected);
    document.documentElement.style.setProperty("--fs-artifact-chat", `${Number.parseFloat(selected) * artifactTextScale()}px`);
    document.querySelectorAll("[data-chat-font]").forEach((button) => {
      const active = button.dataset.chatFont === selected;
      button.classList.toggle("active", active);
      button.setAttribute("aria-pressed", String(active));
    });
    if (persist) {
      safeStorageSet("miyu.web.chatFontSize", selected);
      saveUiPref("chatFontSize", selected);
    }
  }

  function setReasoningExpanded(value, persist = true) {
    state.reasoningExpanded = Boolean(value);
    elements.reasoningExpandToggle?.setAttribute("aria-checked", String(state.reasoningExpanded));
    // 对已渲染的思考块即时生效
    document.querySelectorAll(".reasoning-block").forEach((block) => {
      block.open = state.reasoningExpanded;
    });
    if (persist) {
      safeStorageSet("miyu.web.reasoningExpanded", String(state.reasoningExpanded));
      saveUiPref("reasoningExpanded", String(state.reasoningExpanded));
    }
  }

  function setToolExpanded(value, persist = true) {
    state.toolExpanded = Boolean(value);
    elements.toolExpandToggle?.setAttribute("aria-checked", String(state.toolExpanded));
    // 对已渲染的工具签即时生效
    document.querySelectorAll(".tool-card").forEach((card) => {
      card.classList.toggle("collapsed", !state.toolExpanded);
      card.querySelector(".tool-head")?.setAttribute("aria-expanded", String(state.toolExpanded));
    });
    if (persist) {
      safeStorageSet("miyu.web.toolExpanded", String(state.toolExpanded));
      saveUiPref("toolExpanded", String(state.toolExpanded));
    }
  }

  function setProcCollapse(value, persist = true) {
    state.procCollapse = Boolean(value);
    elements.procCollapseToggle?.setAttribute("aria-checked", String(state.procCollapse));
    // 对已经切断的时间线即时生效:开 → 露出总结行并收起;关 → 藏掉总结行并展开
    document.querySelectorAll(".proc-line").forEach((line) => {
      if (!line.miyuProc?.closed) return;
      line.miyuProc.head.hidden = !state.procCollapse;
      procLineSetOpen(line, !state.procCollapse);
    });
    if (persist) {
      safeStorageSet("miyu.web.procCollapse", String(state.procCollapse));
      saveUiPref("procCollapse", String(state.procCollapse));
    }
  }

  /* ─── 过程时间线 ───
   * 连续的思考块和工具签串成一条时间线(.proc-line):一根 1px 细线穿过图标列的中心,
   * 图标处断开,图标就是节点。正文、媒体、任何不是思考/工具的东西一出现,就把当前
   * 时间线「切断」——后面再来工具就另起一条。
   * 「过程自动收起」开着时,切断那一刻收成一行总结(Worked for 5.4 s · 3 tools);
   * 关着就保持展开,也不出总结行。运行中(还没切断)永远没有总结行。
   * 细线是独立元素,起点和终点跟着可见节点走,ResizeObserver 一触发就重算,
   * 高度交给 CSS transition——新出一行,线就平滑长到那个图标,不是瞬间跳。
   */
  function procLineCreate(isStatic) {
    const line = document.createElement("div");
    line.className = "proc-line is-live is-open";
    if (isStatic) line.classList.add("is-static");
    const rail = document.createElement("i");
    rail.className = "proc-rail";
    rail.setAttribute("aria-hidden", "true");
    const head = document.createElement("button");
    head.type = "button";
    head.className = "proc-head";
    head.hidden = true;
    const node = document.createElement("span");
    node.className = "proc-node";
    node.appendChild(makeIconSlot("chevron-right", "proc-chevron"));
    const summary = document.createElement("span");
    summary.className = "proc-summary";
    head.append(node, summary);
    head.addEventListener("click", () => procLineSetOpen(line, !line.classList.contains("is-open")));
    const wrap = document.createElement("div");
    wrap.className = "proc-wrap";
    const inner = document.createElement("div");
    const steps = document.createElement("div");
    steps.className = "proc-steps";
    inner.appendChild(steps);
    wrap.appendChild(inner);
    line.append(rail, head, wrap);
    line.miyuProc = { rail, head, summary, steps, closed: false, batchStart: -Infinity };
    const fit = () => procLineFit(line);
    if (typeof ResizeObserver === "function") new ResizeObserver(fit).observe(line);
    window.requestAnimationFrame(fit);
    return line;
  }

  const PROC_NODE_SELECTOR = ":scope > .tool-head > .tool-icon, :scope > summary > .reasoning-icon, :scope > summary > .subagent-brief-marker, :scope.tool-preparing-tag > .icon-slot";

  function procLineFit(line) {
    const proc = line.miyuProc;
    if (!proc || !line.isConnected) return;
    const nodes = [];
    if (!proc.head.hidden) nodes.push(proc.head.querySelector(".proc-node"));
    if (line.classList.contains("is-open") || proc.folding) {
      for (const step of proc.steps.children) {
        const node = step.querySelector(PROC_NODE_SELECTOR);
        // 隐藏的签(生图签藏着)没有 offsetParent,不算节点
        if (node && node.offsetParent) nodes.push(node);
      }
    }
    if (!nodes.length) {
      proc.rail.style.height = "0px";
      return;
    }
    // app 壳 zoom 1.1 下 getBoundingClientRect 是缩放后的坐标,style 里的 px 是缩放前的,
    // 用容器自己的 rect 宽 / offsetWidth 反推缩放比。
    const box = line.getBoundingClientRect();
    const zoom = line.offsetWidth ? box.width / line.offsetWidth : 1;
    const center = (node) => {
      const rect = node.getBoundingClientRect();
      return (rect.top - box.top + rect.height / 2) / zoom;
    };
    const first = center(nodes[0]);
    let last = center(nodes[nodes.length - 1]);
    // 末尾那一步要是带着命令行（命令签的抬头底下那几行），线得跟到它们下面。
    // 只算到最后一个**节点圆心**的话，末尾那段命令旁边就是空的——后面再跟一个
    // 工具时才"看起来有线"，那根其实是下一段的连线（用户 09-20）。
    // 只在这条时间线**展开着**（或正在开合）时才延伸——收起态的线必须收到 0，
    // 而那几行命令这时还在布局里、只是被裁掉了，不判一下就会把线又拉出来
    // （`rail_sized` / `fold_synced` 两条回归就是这么挂的）。
    const lastStep =
      line.classList.contains("is-open") || proc.folding
        ? [...proc.steps.children].reverse().find((step) => {
            const node = step.querySelector(PROC_NODE_SELECTOR);
            return node && node.offsetParent;
          })
        : null;
    const trailing = lastStep
      ? [...lastStep.querySelectorAll(":scope > .tool-command-preview, :scope > .tool-command-more, :scope > .xs-send-preview, :scope > .xs-send-more")]
          .filter((el) => !el.hidden && el.getBoundingClientRect().height > 0)
      : [];
    for (const el of trailing) {
      const rect = el.getBoundingClientRect();
      last = Math.max(last, (rect.bottom - box.top) / zoom);
    }
    // 开合动画进行中:内层在被裁剪,线的终点不能超过当前可见底边,否则内容收完了线还拖在外面
    const clip = proc.steps.parentElement.getBoundingClientRect();
    last = Math.min(last, (clip.bottom - box.top) / zoom);
    proc.rail.style.top = `${first}px`;
    proc.rail.style.height = `${Math.max(0, last - first)}px`;
  }

  // 展开/收起时间线里某一项(思考块/工具卡)是瞬间的,但 proc-rail 靠 ResizeObserver
  // + 0.45s transition 平滑跟随,不同步就抖一下(09-12 #3)。让细线立即贴合、这次不过渡。
  // 展开/收起时把细线重贴一次内容(#8/#9):内容用 grid-rows fold 平滑展开(~0.3s),
  // 而 .proc-rail 已去掉自己的 transition,靠 proc-line 的 ResizeObserver 在动画每一帧
  // 重量节点位置、瞬时跟着内容长/缩。这里再补一次即时 fit 兜底(有些位移不改 proc-line
  // 高度、ResizeObserver 不触发),不再跑 360ms rAF 循环(那是长页面卡死/崩溃的隐患)。
  function railSnapFit(el) {
    const line = el?.closest?.(".proc-line");
    if (line?.miyuProc) procLineFit(line);
  }

  function procLineSetOpen(line, open) {
    line.classList.toggle("is-open", open);
    const proc = line.miyuProc;
    if (!proc) return;
    proc.head.setAttribute("aria-expanded", String(open));
    // 收起「Worked for」整条时间线时,把里面已展开的思考块/工具卡(含子代理里的)
    // 一并收起(#10),下次展开是干净收起态,不保留上次翻开的。
    if (!open) {
      proc.steps.querySelectorAll("details[open]").forEach((d) => { d.open = false; });
      proc.steps.querySelectorAll(".tool-card:not(.collapsed)").forEach((c) => {
        c.classList.add("collapsed");
        const innerHead = c.querySelector(".tool-head");
        if (innerHead) innerHead.setAttribute("aria-expanded", "false");
      });
    }
    // 开合期间线逐帧跟裁剪边走,不自己再走一遍 transition(两条曲线叠起来就是线拖在内容后面)。
    // 收起时节点仍算数,只是被裁剪边钳住;裁剪到头线也就到头了。
    proc.rail.style.transition = "none";
    proc.folding = true;
    window.clearTimeout(proc.foldTimer);
    proc.foldTimer = window.setTimeout(() => {
      proc.folding = false;
      proc.rail.style.transition = "";
      procLineFit(line);
    }, 420);
    // ResizeObserver 是这一帧布局完才回调,线会慢内容一帧;开合期间每帧自己量一次,
    // 读 rect 会拿到过渡当前值,写回去落在同一帧里。
    const tick = () => {
      if (!proc.folding) return;
      procLineFit(line);
      window.requestAnimationFrame(tick);
    };
    window.requestAnimationFrame(tick);
    procLineFit(line);
  }

  // 把思考块 / 工具签挂进当前时间线;没有开着的就新起一条
  function procLineAttach(blocks, element, isStatic = false) {
    if (!blocks || !element) return null;
    let line = blocks.lastElementChild;
    if (!line?.classList?.contains("proc-line") || line.miyuProc?.closed) {
      line = procLineCreate(isStatic);
      blocks.appendChild(line);
    }
    if (!isStatic) {
      // 快模型一口气吐几个调用:不压着后来的行等,而是让 250ms 窗口内到的行共用
      // 同一条淡入时间轴(负延迟对齐到窗口起点),几行像一批一起浮起来;窗口过了
      // 再开新一批。动画还是那条曲线,只是不会一行一行各自蹦。
      const proc = line.miyuProc;
      const now = performance.now();
      if (now - proc.batchStart > 250) proc.batchStart = now;
      const offset = now - proc.batchStart;
      if (offset > 0) element.style.animationDelay = `-${Math.round(offset)}ms`;
    }
    line.miyuProc.steps.appendChild(element);
    return line;
  }

  // 子代理任务简介作为「时间线的开头」插进去(用户:和 timeline 统一,不再是分离的一块)。
  // 建一条 proc-line(若无),把 brief 放成第一个 step——它的 .subagent-brief-marker
  // 会被 PROC_NODE_SELECTOR 认作节点,细线从它这里起头。
  function attachSubBrief(blocks, brief) {
    if (!blocks || !brief) return;
    let line = blocks.lastElementChild;
    if (!line?.classList?.contains("proc-line") || line.miyuProc?.closed) {
      line = procLineCreate(false);
      blocks.appendChild(line);
    }
    line.miyuProc.steps.insertBefore(brief, line.miyuProc.steps.firstChild);
    procLineFit(line);
  }

  // 正文/媒体来了:把当前时间线切断
  function procLineBreak(blocks) {
    const line = blocks?.lastElementChild;
    if (!line?.classList?.contains("proc-line") || line.miyuProc?.closed) return;
    const proc = line.miyuProc;
    proc.closed = true;
    line.classList.remove("is-live");
    procLineRefresh(line);
    // 子过程时间线(前台/后台子代理展开区)恒展开,不做 procCollapse 折叠:那块本就是
    // 限高滚动的紧凑区,折成「Thought / N tools」摘要既多余又会冒出一条怪「Thought」
    // 顶在 prompt 上面(#2)。只有主对话的过程区才折。
    if (state.procCollapse && !blocks.classList?.contains("sub-blocks")) {
      proc.head.hidden = false;
      procLineSetOpen(line, false);
    }
  }

  // 摘要行的工具分类,与 crates/miyu-base/src/tool_names.rs 的 COMMAND_TOOLS /
  // FILE_EDIT_TOOLS 是同一张名单(Rust 单测 web_fold_summary_lists_match_rust 盯着)。
  // edits 只数改磁盘文件的那几步;知识库、artifact、删文件和其余一切算 tools(09-24)。
  const COMMAND_TOOLS = ["run_command", "Bash"];
  const FILE_EDIT_TOOLS = [
    "edit",
    "apply_patch",
    "write_file",
    "edit_file",
    "edit_string",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "write_to_file",
    "replace_file_content",
    "multi_replace_file_content",
  ];

  function toolKindOf(name) {
    const base = String(name || "");
    if (COMMAND_TOOLS.includes(base)) return "command";
    if (FILE_EDIT_TOOLS.includes(base)) return "edit";
    return "other";
  }

  // 总结行文字(09-24,与 TUI 的 summary_line 同一套规则):
  //   跑过命令 → Ran 3 commands · 2 edits · 4 tools · 1 thought · 1 err · 12s
  //   没跑命令 → Worked for 12s · 2 edits · 4 tools · 1 thought
  //   只想了想 → 1 thought(网页的思考块不记耗时)
  function procLineRefresh(line) {
    const proc = line?.miyuProc;
    if (!proc?.closed) return;
    const counts = { command: 0, edit: 0, other: 0 };
    for (const card of proc.steps.querySelectorAll(":scope > .tool-card")) {
      counts[toolKindOf(card.dataset.toolName)] += 1;
    }
    const thoughts = proc.steps.querySelectorAll(":scope > .reasoning-block").length;
    const errs = proc.steps.querySelectorAll(":scope > .tool-card.is-failure").length;
    // 「Worked for」= 第一个工具开跑到最后一个工具跑完。实时用 performance.now,
    // 回看用落库的 Unix 毫秒,差值同一口径,刷新前后数字一致。
    let first = Infinity;
    let last = -Infinity;
    for (const card of proc.steps.querySelectorAll(":scope > .tool-card")) {
      const timing = card.miyuTiming;
      if (!timing || timing.startedAt == null || timing.finishedAt == null) continue;
      first = Math.min(first, timing.startedAt);
      last = Math.max(last, timing.finishedAt);
    }
    const elapsed = Number.isFinite(first) && Number.isFinite(last) ? formatToolDuration(last - first) : "";
    const strong = (text) => {
      const b = document.createElement("b");
      b.textContent = text;
      return b;
    };
    const plain = (text, className = "") => {
      const span = document.createElement("span");
      if (className) span.className = className;
      span.textContent = text;
      return span;
    };
    const count = (n, one, many) => `${n} ${n === 1 ? one : many}`;
    // 先排好文字,打头那一项加粗;报错那一项标红。
    const items = [];
    const acted = counts.command + counts.edit + counts.other > 0;
    if (!acted) {
      items.push({ text: thoughts ? count(thoughts, "thought", "thoughts") : (elapsed ? `Worked for ${elapsed}` : t("已完成")) });
    } else {
      if (counts.command) items.push({ text: `Ran ${count(counts.command, "command", "commands")}` });
      else if (elapsed) items.push({ text: `Worked for ${elapsed}` });
      if (counts.edit) items.push({ text: count(counts.edit, "edit", "edits") });
      if (counts.other) items.push({ text: count(counts.other, "tool", "tools") });
      if (thoughts) items.push({ text: count(thoughts, "thought", "thoughts") });
      if (errs) items.push({ text: `${errs} err`, className: "proc-err" });
      if (counts.command && elapsed) items.push({ text: elapsed });
    }
    const parts = items.map((item, index) => (index === 0 ? strong(item.text) : plain(item.text, item.className || "")));
    proc.summary.replaceChildren();
    parts.forEach((part, index) => {
      if (index) proc.summary.appendChild(plain(" · ", "proc-dot"));
      proc.summary.appendChild(part);
    });
  }

  function thinkingVariantLabel(variant, short = false) {
    if (variant == null || variant === MODEL_DEFAULT_PIN) {
      return short ? THINKING_VARIANT_DEFAULT_LABEL : t("模型默认");
    }
    return String(variant);
  }

  function normalizeThinkingVariantModels(value) {
    if (!Array.isArray(value)) return [];
    return value.flatMap((item) => {
      const providerId = String(item?.provider_id || "").trim();
      const model = String(item?.model || "").trim();
      if (!providerId || !model) return [];
      const variants = Array.from(new Set(
        (Array.isArray(item?.variants) ? item.variants : [])
          .map((variant) => String(variant).trim())
          .filter(Boolean)
      ));
      const selected = typeof item?.selected === "string" && variants.includes(item.selected)
        ? item.selected
        : null;
      return [{ provider_id: providerId, model, variants, selected }];
    });
  }









  async function loadThinkingVariants() {
    const generation = ++state.thinkingVariantLoadGeneration;
    state.thinkingVariantLoading = true;
    state.thinkingVariantError = "";
    updateControlState();
    try {
      const response = await apiRequest("/api/models/thinking-variants", { cache: "no-store" });
      const payload = await response.json();
      if (generation !== state.thinkingVariantLoadGeneration) return;
      state.thinkingVariantModels = normalizeThinkingVariantModels(payload?.options);
      updateCurrentModelDisplay();
    } catch (error) {
      if (generation !== state.thinkingVariantLoadGeneration) return;
      state.thinkingVariantError = error.message || t("无法载入思考档位");
    } finally {
      if (generation === state.thinkingVariantLoadGeneration) {
        state.thinkingVariantLoading = false;
        updateControlState();
      }
    }
  }




  function closeSidebar() {
    elements.sidebar.classList.remove("open");
    elements.sidebarScrim.classList.remove("visible");
    elements.sidebarScrim.tabIndex = -1;
  }

  function setSidebarCollapsed(collapsed, { automatic = false } = {}) {
    state.sidebarCollapsed = Boolean(collapsed);
    state.sidebarAutoCollapsed = Boolean(automatic && collapsed);
    elements.appShell?.classList.toggle("is-sidebar-collapsed", state.sidebarCollapsed);
    if (elements.sidebarExpandButton) elements.sidebarExpandButton.hidden = !state.sidebarCollapsed;
    if (elements.sidebarCollapseButton) elements.sidebarCollapseButton.hidden = state.sidebarCollapsed;
    if (state.sidebarCollapsed) closeSidebar();
    if (!automatic) safeStorageSet("miyu.web.sidebarCollapsed", String(state.sidebarCollapsed));
    syncArtifactLayout?.();
  }

  function syncSidebarSpace() {
    if (layoutViewportWidth() <= 760) {
      if (state.sidebarAutoCollapsed) setSidebarCollapsed(false, { automatic: true });
      return;
    }
    const shellWidth = elements.appShell.clientWidth;
    const sidebarWidth = Number.parseFloat(getComputedStyle(elements.appShell).getPropertyValue("--sidebar-width")) || 252;
    const artifactWidth = state.artifactOpen && !state.artifactMaximized ? artifactWidthPixels() + 26 : 0;
    const availableWhenExpanded = shellWidth - sidebarWidth - artifactWidth;
    if (!state.sidebarCollapsed && availableWhenExpanded < 360) {
      setSidebarCollapsed(true, { automatic: true });
    } else if (state.sidebarAutoCollapsed && availableWhenExpanded >= 420) {
      setSidebarCollapsed(false, { automatic: true });
    }
  }

  function openSidebar(opener = document.activeElement) {
    state.sidebarOpener = opener;
    elements.sidebar.classList.add("open");
    elements.sidebarScrim.classList.add("visible");
    elements.sidebarScrim.tabIndex = 0;
  }

  function getFocusable(container) {
    return Array.from(container.querySelectorAll("button:not(:disabled), input:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex='-1'])"))
      .filter((node) => !node.hidden && node.getClientRects().length > 0);
  }

  // 设置以前是从右侧滑出来的抽屉,自带遮罩和焦点陷阱。现在它是控制台的一个
  // 标签页——控制台本来就是个整页视图,设置这么大一坨挂在抽屉里,和「数据统计」
  // 各占一套导航,没道理。这两个函数保留下来当入口,内部转成开控制台。
  function openSettings(opener = document.activeElement) {
    state.settingsOpener = opener;
    closeModelMenu();
    consoleOpen("settings");
  }

  function closeSettings({ restoreFocus = true } = {}) {
    if (!settingsIsOpen()) return;
    consoleClose();
    if (restoreFocus && state.settingsOpener instanceof HTMLElement) state.settingsOpener.focus();
    state.settingsOpener = null;
  }

  function settingsIsOpen() {
    return consoleIsOpen() && state.consolePanel === "settings";
  }

  function openModelMenu() {
    if (elements.modelButton.disabled || state.models.length === 0) return;
    resetModelMenuStaging();
    renderModelMenu();
    elements.modelMenu.hidden = false;
    elements.modelButton.setAttribute("aria-expanded", "true");
    positionModelMenu();
    refreshSessionModelOverride();
    const selected = elements.modelMenu.querySelector(".model-menu-item.selected:not(:disabled)");
    const first = elements.modelMenu.querySelector(".model-menu-item:not(:disabled)");
    window.requestAnimationFrame(() => (selected || first)?.focus());
  }

  /// 菜单不在按钮的父元素里(`.composer` 会把它裁掉,见 index.html),所以
  /// 位置得自己算：贴按钮左边、浮在按钮上方,再夹回 dock 的可视范围内。
  function positionModelMenu() {
    if (elements.modelMenu.hidden) return;
    const dock = elements.composerDock.getBoundingClientRect();
    const button = elements.modelButton.getBoundingClientRect();
    const gap = 8;
    const margin = 8;
    const width = elements.modelMenu.offsetWidth * UI_SCALE;
    const left = Math.min(
      Math.max(margin, button.left),
      Math.max(margin, window.innerWidth - width - margin)
    );
    elements.modelMenu.style.left = `${visualPixelsToLayout(left - dock.left)}px`;
    elements.modelMenu.style.bottom = `${visualPixelsToLayout(dock.bottom - button.top + gap)}px`;
    // 上方剩多少就开多高,顶不出视口。
    const room = visualPixelsToLayout(Math.max(160, button.top - gap - margin));
    elements.modelMenu.style.maxHeight = `${Math.min(420, room)}px`;
  }

  function closeModelMenu({ restoreFocus = false, discard = true } = {}) {
    if (elements.modelMenu.hidden) return;
    closeLevelMenu();
    elements.modelMenu.hidden = true;
    elements.modelButton.setAttribute("aria-expanded", "false");
    if (discard) {
      state.stagedModelKeys = null;
      state.stagedFollowGlobal = false;
      state.modelMenuTouched = false;
      state.modelMenuError = "";
    }
    if (restoreFocus) elements.modelButton.focus();
  }

  function showToast(message, type = "info") {
    const toast = document.createElement("div");
    toast.className = `toast${type === "error" ? " is-error" : ""}`;
    toast.textContent = String(message || t("操作未完成"));
    elements.toastRegion.replaceChildren(toast);
    if (state.toastTimer) window.clearTimeout(state.toastTimer);
    state.toastTimer = window.setTimeout(() => {
      if (toast.isConnected) toast.remove();
    }, type === "error" ? 6000 : 3000);
  }

  function showInlineError(message) {
    const text = String(message || t("操作未完成")).trim();
    elements.errorRegion.textContent = text;
    elements.errorRegion.hidden = !text;
  }

  function clearInlineError() {
    elements.errorRegion.textContent = "";
    elements.errorRegion.hidden = true;
  }

  function deepClone(value) {
    if (typeof structuredClone === "function") return structuredClone(value);
    return JSON.parse(JSON.stringify(value));
  }

  function normalizePersona(value) {
    const name = String(value?.name || "").trim() || "Miyu";
    const avatarUrl = typeof value?.avatar_url === "string" && value.avatar_url ? value.avatar_url : null;
    const boardImageUrl = typeof value?.board_image_url === "string" && value.board_image_url
      ? value.board_image_url
      : null;
    const boardTitle = String(value?.board_title || "").trim() || DEFAULT_BOARD_TITLE;
    const boardSubtitle = String(value?.board_subtitle || "").trim() || DEFAULT_BOARD_SUBTITLE;
    const composerPlaceholder =
      String(value?.composer_placeholder || "").trim() || defaultComposerPlaceholder(name);
    const configuredPrompts = Array.isArray(value?.starter_prompts) ? value.starter_prompts : [];
    const starterPrompts = DEFAULT_STARTER_PROMPTS.map((fallback, index) => String(configuredPrompts[index] || "").trim() || fallback);
    // revision 只在图片 URL 真正变化时更新:每次快照都取 Date.now() 会让
    // 头像/看板图的浏览器缓存永远击穿,每次 bootstrap 都重新下载。
    const previous = state.persona;
    const revision =
      previous && previous.avatar_url === avatarUrl && previous.board_image_url === boardImageUrl
        ? previous.revision
        : `${Date.now()}`;
    return {
      name,
      avatar_url: avatarUrl,
      board_image_url: boardImageUrl,
      board_title: boardTitle,
      board_subtitle: boardSubtitle,
      composer_placeholder: composerPlaceholder,
      starter_prompts: starterPrompts,
      revision
    };
  }

  function setPersonaAvatar(image) {
    const url = state.persona?.avatar_url;
    image.hidden = !url;
    if (!url) {
      image.removeAttribute("src");
      return;
    }
    image.hidden = false;
    const separator = url.includes("?") ? "&" : "?";
    image.src = `${url}${separator}v=${encodeURIComponent(state.persona?.revision || "1")}`;
    image.onerror = () => {
      image.hidden = true;
      image.removeAttribute("src");
    };
  }

  function applyPersona(value) {
    state.persona = normalizePersona(value);
    elements.brandName.textContent = state.persona.name;
    elements.brandAvatar.alt = state.persona.name;
    setPersonaAvatar(elements.brandAvatar);
    elements.emptyKickerName.textContent = state.persona.name;
    elements.emptyTitle.textContent = state.persona.board_title;
    elements.emptySubtitle.textContent = state.persona.board_subtitle;
    elements.composerInput.placeholder = state.persona.composer_placeholder;
    const boardImageUrl = state.persona.board_image_url;
    elements.emptyVisual.hidden = !boardImageUrl;
    elements.emptyBoardImage.alt = t("{name} 看板图片", {name: state.persona.name});
    if (boardImageUrl) {
      elements.emptyBoardImage.onerror = () => {
        elements.emptyBoardImage.removeAttribute("src");
        elements.emptyVisual.hidden = true;
      };
      elements.emptyBoardImage.src = `${boardImageUrl}${boardImageUrl.includes("?") ? "&" : "?"}v=${encodeURIComponent(state.persona.revision)}`;
    } else {
      elements.emptyBoardImage.removeAttribute("src");
    }
    elements.promptGrid.querySelectorAll("[data-prompt]").forEach((button, index) => {
      const prompt = state.persona.starter_prompts[index] || DEFAULT_STARTER_PROMPTS[index];
      button.dataset.prompt = prompt;
      const label = button.querySelector("span:last-child");
      if (label) label.textContent = prompt;
    });
    const refreshAssistant = (root) => root.querySelectorAll(".assistant-label").forEach((label) => {
      const name = label.querySelector("strong");
      const avatar = label.querySelector("img");
      if (name) name.textContent = state.persona.name;
      if (avatar) setPersonaAvatar(avatar);
    });
    refreshAssistant(elements.timeline);
    for (const articles of state.finishedTurnArticles.values()) {
      for (const entry of articles) refreshAssistant(entry.article);
    }
  }

  function setSettingsView(view) {
    const selected = ["interface", "prompts", "providers", "models", "general", "mcp", "plugins", "qq", "advanced"].includes(view) ? view : "interface";
    state.settingsView = selected;
    elements.settingsNav.querySelectorAll("[data-settings-view]").forEach((button) => {
      const active = button.dataset.settingsView === selected;
      button.classList.toggle("active", active);
      button.setAttribute("aria-current", active ? "page" : "false");
    });
    elements.settingsPanels.forEach((panel) => {
      panel.hidden = panel.dataset.settingsPanel !== selected;
    });
    window.MiyuSettings?.onShow(selected);
    if (consoleIsOpen() && state.consolePanel === "settings") writeConsoleHash(consoleHashFor("settings", selected));
  }

  function configValue(path, fallback = undefined) {
    let value = state.configDraft;
    for (const key of path.split(".")) {
      if (value == null || typeof value !== "object" || !(key in value)) return fallback;
      value = value[key];
    }
    return value;
  }

  function setConfigValue(path, value) {
    if (!state.configDraft) return;
    const keys = path.split(".");
    let target = state.configDraft;
    for (const key of keys.slice(0, -1)) {
      if (!target[key] || typeof target[key] !== "object") target[key] = {};
      target = target[key];
    }
    target[keys[keys.length - 1]] = value;
    markConfigDirty();
  }

  function markConfigDirty() {
    state.configDirty = true;
    updateSettingsControls();
  }

  function clearProviderSecretChanges() {
    for (const key of Object.keys(state.secretChanges)) {
      if (key.startsWith("providers.")) delete state.secretChanges[key];
    }
  }

  function refreshProviderSecretStates() {
    for (const key of Object.keys(state.secretStates)) {
      if (key.startsWith("providers.")) delete state.secretStates[key];
    }
    state.providerSecretStates.forEach((configured, index) => {
      state.secretStates[`providers.${index}.api_key`] = Boolean(configured);
    });
  }

  function updateSettingsControls() {
    const busy = state.configLoading || state.configSaving;
    // 界面语言是 config 字段(display.language),不在 schema 表单里:显示的是
    // **草稿**里的值(用户 09-23 指定放在「界面」视图的对话字号上面)。
    const language = String(state.configDraft?.display?.language || "auto");
    elements.languageSeg?.querySelectorAll("[data-language]").forEach((button) => {
      const active = button.dataset.language === language;
      button.classList.toggle("active", active);
      button.setAttribute("aria-pressed", String(active));
    });
    elements.reloadConfigButton.disabled = busy;
    elements.saveConfigButton.disabled = busy || !state.configLoaded || !state.configDirty || state.invalidConfigFields.size > 0 || conversationRunning();
    elements.settingsFooter?.classList.toggle("is-dirty", Boolean(state.configLoaded && state.configDirty));
    elements.settingsFooter?.classList.toggle("is-invalid", state.invalidConfigFields.size > 0);
    if (state.configLoading) elements.settingsStatus.textContent = t("正在载入配置");
    else if (state.configSaving) elements.settingsStatus.textContent = t("正在验证并保存");
    else if (!state.configLoaded) elements.settingsStatus.textContent = t("尚未载入配置");
    else if (state.invalidConfigFields.size) elements.settingsStatus.textContent = t("请修正表单中的错误");
    else if (conversationRunning() && state.configDirty) elements.settingsStatus.textContent = t("回复完成后才能保存");
    else elements.settingsStatus.textContent = state.configDirty ? t("有未保存的修改") : t("配置已同步");
  }

  function updateAdvancedConfigEditor() {
    if (!state.configDraft || document.activeElement === elements.advancedConfigEditor) return;
    elements.advancedConfigEditor.value = JSON.stringify(state.configDraft, null, 2);
  }

  // 设置区的渲染在 settings.js:这里只清校验状态、同步「高级」JSON 与底栏。
  function renderConfigEditors() {
    if (!state.configLoaded || !state.configDraft) return;
    state.invalidConfigFields.clear();
    window.MiyuSettings?.render();
    updateAdvancedConfigEditor();
    updateSettingsControls();
  }

  function mapServerSecretStates(payload) {
    const providers = state.configDraft?.providers || [];
    state.providerSecretStates = providers.map((_, index) => Boolean(payload[`providers.${index}.api_key`]));
    const states = { ...payload };
    state.secretStates = states;
    refreshProviderSecretStates();
    return states;
  }

  // 配置文件会省略未修改的平台默认值；草稿仍需补齐真实语义，
  // 以免 WebUI 保存其他设置时覆盖通讯平台的默认策略。
  function ensurePlatformDefaults(draft) {
    if (!draft || typeof draft !== "object") return;
    draft.platforms = Object.assign({
      command_prefix: "/",
      commands: {}
    }, draft.platforms);
    const qq = Object.assign({
      enabled: false,
      reverse_ws_port: 8300,
      access_token: "",
      admin_users: [],
      allow_non_admin_host_tools: false,
      user_identification: true,
      show_group_name: true,
      conversations: [],
      plugins: {},
      asset_base_url: "",
      max_reply_chars: 3000,
    }, draft.platforms.qq);
    qq.private_chats = Object.assign({
      whitelist: [],
      allow_non_whitelist: true,
      non_whitelist_rate_limit: { max_messages: 2, window_seconds: 600 }
    }, qq.private_chats);
    qq.group_chats = Object.assign({
      whitelist: [],
      trigger_keywords: [],
      whitelist_rate_limit: { max_messages: 30, window_seconds: 60 },
      allow_non_whitelist: true,
      non_whitelist_rate_limit: { max_messages: 2, window_seconds: 600 }
    }, qq.group_chats);
    draft.platforms.qq = qq;
  }

  function applyConfigPayload(payload) {
    state.configDraft = deepClone(payload?.config || {});
    ensurePlatformDefaults(state.configDraft);
    state.configOriginal = deepClone(payload?.config || {});
    state.promptDraft = deepClone(payload?.prompts || { personas: [], identities: [] });
    state.promptOriginal = deepClone(payload?.prompts || { personas: [], identities: [] });
    state.secretChanges = {};
    mapServerSecretStates(payload?.secret_states || {});
    state.configDirty = false;
    state.configLoaded = true;
    state.invalidConfigFields.clear();
    if (Array.isArray(payload?.models)) state.models = payload.models;
    state.configMultimodalModels = Array.isArray(payload?.multimodal_models) ? payload.multimodal_models : [];
    const providersById = new Map(
      (Array.isArray(state.configDraft?.providers) ? state.configDraft.providers : [])
        .map((provider) => [String(provider?.id || ""), provider])
    );
    state.configInferredImageModels = state.configMultimodalModels.filter((model) => {
      const provider = providersById.get(String(model?.provider_id || ""));
      const declared = provider?.model_modalities;
      return !(declared && typeof declared === "object"
        && Object.prototype.hasOwnProperty.call(declared, String(model?.model || "")));
    });
    if (payload?.display && typeof payload.display === "object") state.display = payload.display;
    if (payload?.context && typeof payload.context === "object") {
      // /api/config 的上下文是 daemon 当前会话的。「累计」按会话记，别拿它盖掉正在看的
      // 那条的——拉回来之前恰好重绘一次，max 就会把别的会话的数锁成基线（09-23）。
      const { cumulative_tokens, cumulative_prompt_tokens, cumulative_cache_read_tokens } = state.context || {};
      state.context = state.viewSessionId
        ? { ...payload.context, cumulative_tokens, cumulative_prompt_tokens, cumulative_cache_read_tokens }
        : payload.context;
    }
    if (payload?.persona) applyPersona(payload.persona);
    renderConfigEditors();
    renderModelMenu();
    updateContext();
    // /api/config 给的是全局池的窗口;看着的会话钉了模型时以会话接口为准,
    // 否则打开设置页一次,上下文条就被改回全局默认模型的窗口。
    if (state.viewSessionId) refreshSessionContext(state.viewSessionId);
  }

  async function loadConfigDraft() {
    if (state.configLoading || state.configSaving) return;
    if (state.configDirty && !window.confirm(t("放弃尚未保存的配置修改并重新载入？"))) return;
    state.configLoading = true;
    updateSettingsControls();
    try {
      const response = await apiRequest("/api/config");
      applyConfigPayload(await response.json());
    } catch (error) {
      showToast(error.message || t("配置载入失败"), "error");
      elements.settingsStatus.textContent = error.message || t("配置载入失败");
    } finally {
      state.configLoading = false;
      updateSettingsControls();
    }
  }

  function promptStateChanged() {
    if (!state.configOriginal || !state.promptOriginal) return false;
    const promptKeys = ["prompt", "system_prompt_file", "system_prompt"];
    const current = Object.fromEntries(promptKeys.map((key) => [key, state.configDraft?.[key]]));
    const original = Object.fromEntries(promptKeys.map((key) => [key, state.configOriginal?.[key]]));
    const withoutPersonaMetadata = (documents) => Object.fromEntries(
      Object.entries(documents || {}).map(([kind, items]) => [
        kind,
        (Array.isArray(items) ? items : []).map(({
          avatar_path: _avatarPath,
          board_image_path: _BoardImagePath,
          board_title: _BoardTitle,
          board_subtitle: _BoardSubtitle,
          composer_placeholder: _ComposerPlaceholder,
          starter_prompts: _StarterPrompts,
          ...document
        }) => document)
      ])
    );
    return JSON.stringify(current) !== JSON.stringify(original)
      || JSON.stringify(withoutPersonaMetadata(state.promptDraft)) !== JSON.stringify(withoutPersonaMetadata(state.promptOriginal));
  }

  function buildSecretMutations() {
    return { ...state.secretChanges };
  }

  async function saveConfigDraft() {
    if (!state.configLoaded || state.configSaving || state.configLoading || conversationRunning() || state.invalidConfigFields.size) return;
    const personaChanged = String(state.configDraft?.prompt?.active_persona || "")
      !== String(state.configOriginal?.prompt?.active_persona || "");
    // 界面语言变了要整页重载(2026-09-23):语言是服务端按请求注入的
    // (/i18n.js 里的 MIYU_LANG + 根页面 lang),已经渲染好的这一页改不了。
    // 比的是「上一份已保存的配置」与「服务端回执」——draft 里已经是新值,
    // 拿 draft 比永远相等,重载就永远不触发。
    const languageBefore = String(state.configOriginal?.display?.language || "auto");
    state.configSaving = true;
    state.adminBusy = true;
    updateSettingsControls();
    updateControlState();
    try {
      const response = await apiRequest("/api/config", {
        method: "PUT",
        body: JSON.stringify({
          config: state.configDraft,
          secrets: buildSecretMutations(),
          prompts: state.promptDraft,
          reset_conversation: false
        })
      });
      const payload = await response.json();
      const languageAfter = String(payload?.config?.display?.language || "auto");
      applyConfigPayload(payload);
      if (languageBefore !== languageAfter) {
        location.reload();
        return;
      }
      if (personaChanged) await loadBootstrap();
      showToast(t("配置已保存"));
    } catch (error) {
      showToast(error.message || t("配置保存失败"), "error");
      elements.settingsStatus.textContent = error.message || t("配置保存失败");
    } finally {
      state.configSaving = false;
      state.adminBusy = false;
      updateSettingsControls();
      updateControlState();
    }
  }

  function applyAdvancedConfig() {
    try {
      const parsed = JSON.parse(elements.advancedConfigEditor.value);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error(t("配置必须是 JSON 对象"));
      const oldSecretStates = new Map((state.configDraft?.providers || []).map((provider, index) => [String(provider?.id || ""), Boolean(state.providerSecretStates[index])]));
      state.configDraft = parsed;
      ensurePlatformDefaults(state.configDraft);
      state.providerSecretStates = (Array.isArray(parsed.providers) ? parsed.providers : []).map((provider) => oldSecretStates.get(String(provider?.id || "")) || false);
      refreshProviderSecretStates();
      clearProviderSecretChanges();
      markConfigDirty();
      renderConfigEditors();
      showToast(t("完整配置已应用到草稿"));
    } catch (error) {
      showToast(error.message || t("JSON 无效"), "error");
    }
  }

  async function readErrorMessage(response) {
    try {
      const payload = await response.json();
      const message = payload?.error?.message;
      if (typeof message === "string" && message.trim()) return message.trim();
    } catch (_) {
      // Fall through to an HTTP status message.
    }
    return t("请求失败 ({status})", {status: response.status});
  }

  async function apiRequest(path, options = {}) {
    const headers = new Headers(options.headers || {});
    headers.set("Accept", "application/json");
    if (options.body != null && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
    let response;
    try {
      response = await fetch(path, { ...options, headers, credentials: "same-origin" });
    } catch (_) {
      throw new ApiError(t("无法连接 Miyu WebUI"), 0);
    }
    if (response.status === 401 && !state.blocked && !path.startsWith("/api/auth/")) {
      // 登录态没了(daemon 重启、令牌过期):直接回登录页,别等用户发消息时弹一句英文。
      showBlockedState(true, "", { expired: true });
      throw new ApiError(t("登录已过期,请重新登录"), 401);
    }
    if (!response.ok) throw new ApiError(await readErrorMessage(response), response.status);
    return response;
  }

  function asFiniteNumber(value, fallback = 0) {
    const number = Number(value);
    return Number.isFinite(number) ? number : fallback;
  }

  function formatInteger(value) {
    const number = Math.max(0, asFiniteNumber(value));
    try {
      return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 0 }).format(number);
    } catch (_) {
      return String(Math.round(number));
    }
  }

  // 缓存命中率只以输入为分母：输出 token 要到下一轮才进入输入，把它算进
  // 分母会让同样的缓存效果随回复变长而显得越来越差。三家供应商的用量字段
  // 也都是这么定义的（DeepSeek 直接把 prompt 劈成 hit+miss）。
  // 缓存命中率显示口径(09-11 用户拍板,与终端 render::usage::cache_percent 同规矩):
  // 只有 >99.9 才显示成 100;99.1–99.9 留一位小数;99.0 及以下取整(99 不写 99.0)。
  function formatCachePercent(hit, total) {
    if (hit <= 0 || total <= 0) return null;
    const raw = Math.min(100, (hit / total) * 100);
    const roundedOne = Math.round(raw * 10) / 10;
    if (roundedOne >= 100) return "100";
    if (roundedOne > 99) return roundedOne.toFixed(1);
    return String(Math.round(raw));
  }

  function cacheSuffix(cached, prompt) {
    const hit = asFiniteNumber(cached, 0);
    const total = asFiniteNumber(prompt, 0);
    const label = formatCachePercent(hit, total);
    return label == null ? "" : `（C${label}%）`;
  }

  // 输出速度:回合层测的「首块到末块」时长与对应 completion tokens,两者
  // 任一为零就是没测到,不显示——和缓存率同一条规矩。
  function formatGenerationSpeed(tokens, millis) {
    const count = asFiniteNumber(tokens, 0);
    const duration = asFiniteNumber(millis, 0);
    if (count <= 0 || duration <= 0) return "";
    const rate = (count * 1000) / duration;
    return t("每秒 {rate} toks", {rate: rate >= 10 ? formatInteger(Math.round(rate)) : rate.toFixed(1)});
  }

  // 只取速度数字(给输入框下方信息行的「每秒 __ toks」用,模板已带「每秒/toks」)。
  function generationSpeedValue(tokens, millis) {
    const count = asFiniteNumber(tokens, 0);
    const duration = asFiniteNumber(millis, 0);
    if (count <= 0 || duration <= 0) return null;
    const rate = (count * 1000) / duration;
    return rate >= 10 ? formatInteger(Math.round(rate)) : rate.toFixed(1);
  }

  function formatUsageMeta({ turnTotal, turnPrompt, turnCached, estimated, cumulative, cumulativePrompt, cumulativeCached, generationTokens, generationMs }) {
    const parts = [];
    const speed = formatGenerationSpeed(generationTokens, generationMs);
    if (speed) parts.push(speed);
    if (asFiniteNumber(turnTotal) > 0) {
      parts.push(t("本轮{approx}{total}{cache}", {approx: estimated ? t("约 ") : " ", total: formatTokens(turnTotal), cache: cacheSuffix(turnCached, turnPrompt)}));
    }
    if (asFiniteNumber(cumulative) > 0) {
      parts.push(t("累计 {total}{cache}", {total: formatTokens(cumulative), cache: cacheSuffix(cumulativeCached, cumulativePrompt)}));
    }
    return parts.join(" · ");
  }

  function formatTokens(value) {
    const number = Math.max(0, asFiniteNumber(value));
    if (number < 1000) return formatInteger(number);
    const useMillions = number >= 1_000_000;
    const amount = number / (useMillions ? 1_000_000 : 1000);
    const digits = amount >= 100 ? 0 : amount >= 10 ? 1 : 1;
    const suffix = useMillions ? "M" : "k";
    try {
      return `${new Intl.NumberFormat("zh-CN", { maximumFractionDigits: digits }).format(amount)}${suffix}`;
    } catch (_) {
      return `${amount.toFixed(digits)}${suffix}`;
    }
  }

  function parseDate(value) {
    if (value == null || value === "") return null;
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date;
  }

  function formatTime(value) {
    const date = parseDate(value);
    if (!date) return "";
    try {
      return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false }).format(date);
    } catch (_) {
      return date.toLocaleTimeString?.() || "";
    }
  }

  function formatDateTime(value) {
    const date = parseDate(value);
    if (!date) return "";
    try {
      return new Intl.DateTimeFormat("zh-CN", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false
      }).format(date);
    } catch (_) {
      return date.toLocaleString?.() || "";
    }
  }

  function formatRelativeTime(value) {
    const date = parseDate(value);
    if (!date) return "";
    const difference = Date.now() - date.getTime();
    if (difference >= 0 && difference < 60_000) return t("刚刚");
    if (difference >= 0 && difference < 3_600_000) return t("{count} 分钟前", {count: Math.max(1, Math.floor(difference / 60_000))});
    const now = new Date();
    if (date.toDateString() === now.toDateString()) return formatTime(date);
    try {
      return new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric" }).format(date);
    } catch (_) {
      return date.toLocaleDateString?.() || "";
    }
  }

  function dayKey(value) {
    const date = parseDate(value);
    if (!date) return "unknown";
    return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
  }

  function formatDayLabel(value) {
    const date = parseDate(value);
    if (!date) return t("较早");
    const today = new Date();
    const yesterday = new Date(today);
    yesterday.setDate(today.getDate() - 1);
    if (date.toDateString() === today.toDateString()) return t("今天");
    if (date.toDateString() === yesterday.toDateString()) return t("昨天");
    try {
      return new Intl.DateTimeFormat("zh-CN", { year: "numeric", month: "long", day: "numeric" }).format(date);
    } catch (_) {
      return date.toLocaleDateString?.() || t("较早");
    }
  }

  function firstLine(value) {
    return String(value || "").split(/\r?\n/, 1)[0].trim();
  }

  function modelMark(model) {
    const source = String(model?.provider_name || model?.provider_id || model?.model || "").trim();
    if (!source) return "--";
    const words = source.split(/[\s._/-]+/).filter(Boolean);
    const mark = words.length > 1 ? `${words[0][0] || ""}${words[1][0] || ""}` : source.slice(0, 2);
    return mark.toLocaleUpperCase("en-US");
  }

  function modelKey(model) {
    return JSON.stringify([String(model?.provider_id || ""), String(model?.model || "")]);
  }

  function effectiveUsageTotal(usage) {
    if (!usage || typeof usage !== "object") return 0;
    const explicit = asFiniteNumber(usage.total_tokens, 0);
    return explicit > 0 ? explicit : asFiniteNumber(usage.prompt_tokens, 0) + asFiniteNumber(usage.completion_tokens, 0);
  }

  function setConnectionStatus(status) {
    state.connection = status;
    const definitions = {
      online: { sidebar: t("在线"), className: "" },
      connecting: { sidebar: t("重连中"), className: "is-connecting" },
      offline: { sidebar: t("离线"), className: "is-offline" },
      blocked: { sidebar: t("未授权"), className: "is-blocked" }
    };
    const selected = definitions[status] || definitions.connecting;
    elements.sidebarConnectionStatus.textContent = selected.sidebar;
    elements.sidebarStatusDot.classList.remove("is-connecting", "is-offline", "is-blocked");
    if (selected.className) elements.sidebarStatusDot.classList.add(selected.className);
  }

  function updateContext() {
    const tokens = Math.max(0, asFiniteNumber(state.context?.tokens));
    const windowSize = state.context?.window == null ? null : Math.max(0, asFiniteNumber(state.context.window));
    if (elements.contextNumbers) {
      elements.contextNumbers.textContent = windowSize ? `${formatTokens(tokens)} / ${formatTokens(windowSize)}` : `${formatTokens(tokens)} / --`;
    }
    const percent = windowSize > 0 ? Math.min(100, Math.max(0, (tokens / windowSize) * 100)) : 0;
    // 上下文占用画成一个小圆环(比长条优雅,用户反馈原展示不美观):r=9,周长≈56.55,
    // 按占用比例设 dashoffset;高/临界用配色区分。
    if (elements.contextRing) {
      const circ = 2 * Math.PI * 9;
      elements.contextRing.style.strokeDasharray = `${circ.toFixed(2)}`;
      elements.contextRing.style.strokeDashoffset = `${(circ * (1 - percent / 100)).toFixed(2)}`;
    }
    if (elements.contextTrack) {
      elements.contextTrack.setAttribute("aria-valuenow", String(Math.round(percent)));
      elements.contextTrack.setAttribute("aria-label", windowSize ? t("上下文使用 {percent}%", {percent: Math.round(percent)}) : t("上下文 {tokens} tokens", {tokens: formatInteger(tokens)}));
      elements.contextTrack.classList.toggle("is-high", percent >= 75 && percent < 90);
      elements.contextTrack.classList.toggle("is-critical", percent >= 90);
    }
  }

  // 输入框下方信息行的「每秒 toks」「累计」:取最新一轮的样本,回合结束/round_usage 时更新。
  function setComposerUsage({ speed, cumulative } = {}) {
    // undefined = 不动这一项(只想刷累计时别把速度顺手藏了);null/"" = 清空隐藏。
    if (elements.composerSpeed && speed !== undefined) {
      if (speed) {
        elements.composerSpeedValue.textContent = speed;
        elements.composerSpeed.hidden = false;
      } else {
        elements.composerSpeed.hidden = true;
      }
    }
    if (elements.composerCumulative && cumulative !== undefined) {
      if (cumulative) {
        elements.composerCumulativeValue.textContent = cumulative;
        elements.composerCumulative.hidden = false;
      } else {
        elements.composerCumulative.hidden = true;
      }
    }
  }

  // 换会话时把「累计」按会话记的几样清零（见 applySessionView）。
  function resetSessionCumulative() {
    state.cumulativeBase = null;
    state.liveSubagentTokens.clear();
    state.context.cumulative_tokens = 0;
    state.context.cumulative_prompt_tokens = 0;
    state.context.cumulative_cache_read_tokens = 0;
  }

  function updateRuntimeUsage() {}

  function updateCapabilities() {
    const values = [
      [t("会话"), state.capabilities?.multi_conversation ? t("多会话") : t("当前单一对话")],
      [t("附件"), state.capabilities?.attachments ? t("可用") : t("不可用")],
      [t("消息队列"), state.capabilities?.queue ? t("可用") : t("不可用")]
    ];
    elements.capabilityList.replaceChildren();
    for (const [name, value] of values) {
      const row = document.createElement("div");
      const term = document.createElement("dt");
      const description = document.createElement("dd");
      term.textContent = name;
      description.textContent = value;
      row.append(term, description);
      elements.capabilityList.appendChild(row);
    }
  }

  function activeModels() {
    return state.models.filter((model) => model?.active);
  }

  function normalizeModelOverride(value) {
    if (!Array.isArray(value)) return null;
    const models = value
      .map((item) => ({ provider_id: String(item?.provider_id || ""), model: String(item?.model || "") }))
      .filter((item) => item.provider_id && item.model);
    return models.length ? models : null;
  }

  function viewSessionModelOverride() {
    return state.viewSessionId && state.sessionModelOverrideFor === state.viewSessionId
      ? state.sessionModelOverride
      : null;
  }

  function describeOverrideModel(entry) {
    const key = modelKey(entry);
    return state.models.find((model) => modelKey(model) === key) || entry;
  }

  function setSessionModelOverride(sessionId, override) {
    state.sessionModelOverrideFor = String(sessionId || "");
    state.sessionModelOverride = normalizeModelOverride(override);
    updateCurrentModelDisplay();
    if (elements.modelMenu.hidden || state.modelSelectionSubmitting) return;
    // 菜单开着且用户尚未改动暂存选择时，同步为最新覆盖状态。
    if (!state.modelMenuTouched && state.stagedModelKeys instanceof Set) {
      const fresh = viewSessionModelOverride();
      const freshFollow = !fresh;
      const freshKeys = new Set((fresh || []).map(modelKey));
      const unchanged = state.stagedFollowGlobal === freshFollow
        && state.stagedModelKeys.size === freshKeys.size
        && [...freshKeys].every((key) => state.stagedModelKeys.has(key));
      if (!unchanged) {
        const hadFocus = elements.modelMenu.contains(document.activeElement);
        resetModelMenuStaging();
        renderModelMenu();
        if (hadFocus) {
          const focusTarget = elements.modelMenu.querySelector(".model-menu-item.selected:not(:disabled)")
            || elements.modelMenu.querySelector(".model-menu-item:not(:disabled)");
          focusTarget?.focus();
        }
        return;
      }
    }
    updateModelMenuState();
  }

  function setSessionThinkingPins(sessionId, pinned) {
    state.sessionThinkingPinsFor = String(sessionId || "");
    state.sessionThinkingPins = new Map(
      (Array.isArray(pinned) ? pinned : [])
        .filter((pin) => pin?.provider_id && pin?.model && pin?.selected)
        .map((pin) => [modelKey(pin), String(pin.selected)])
    );
    updateCurrentModelDisplay();
    if (!elements.modelMenu.hidden && !state.modelMenuTouched && !state.modelSelectionSubmitting) {
      resetModelMenuStaging();
      renderModelMenu();
    }
  }

  async function refreshSessionThinkingPins(sessionId = state.viewSessionId) {
    const target = String(sessionId || "");
    const token = ++state.sessionThinkingPinsToken;
    if (!target) {
      setSessionThinkingPins("", []);
      return;
    }
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(target)}/thinking-variants`);
      const payload = await response.json();
      if (token !== state.sessionThinkingPinsToken || state.viewSessionId !== target) return;
      setSessionThinkingPins(target, payload?.pinned);
    } catch (_) {
      // 静默失败:按跟随全局显示,下次打开菜单再取。
    }
  }

  /// 这个会话里某个模型实际用的档位:钉了用钉的,没钉跟着全局。
  function sessionPinFor(key) {
    return state.sessionThinkingPinsFor === String(state.viewSessionId || "")
      ? state.sessionThinkingPins.get(key) ?? null
      : null;
  }

  function globalVariantFor(key) {
    const entry = state.thinkingVariantModels.find((model) => modelKey(model) === key);
    return entry ? entry.selected ?? null : null;
  }

  async function refreshSessionModelOverride(sessionId = state.viewSessionId) {
    refreshSessionThinkingPins(sessionId);
    const target = String(sessionId || "");
    const token = ++state.sessionModelOverrideToken;
    if (!target) {
      setSessionModelOverride("", null);
      return;
    }
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(target)}/models`);
      const payload = await response.json();
      if (token !== state.sessionModelOverrideToken || state.viewSessionId !== target) return;
      setSessionModelOverride(target, payload?.model_override);
    } catch (_) {
      // 静默失败：顶栏回退显示全局池，下次打开菜单会再次刷新。
    }
  }

  function updateCurrentModelDisplay() {
    // 设置页摘要始终反映全局激活池。
    const active = activeModels();
    if (active.length === 0) {
      elements.settingsModelMark.textContent = "--";
      elements.settingsModelName.textContent = state.models.length ? t("未选择模型") : t("未配置模型");
      elements.settingsModelProvider.textContent = "--";
    } else if (active.length > 1) {
      elements.settingsModelMark.textContent = "MX";
      elements.settingsModelName.textContent = t("混合模型");
      elements.settingsModelProvider.textContent = t("{count} 个活动端点", {count: active.length});
    } else {
      elements.settingsModelMark.textContent = modelMark(active[0]);
      elements.settingsModelName.textContent = String(active[0].model || "");
      elements.settingsModelProvider.textContent = String(active[0].provider_name || active[0].provider_id || "");
    }

    // 顶栏反映当前会话生效的模型池：有覆盖显示覆盖，否则跟随全局。
    const override = viewSessionModelOverride();
    const pool = override ? override.map(describeOverrideModel) : active;
    const scope = override ? t("本会话固定") : t("跟随全局");
    if (pool.length === 0) {
      elements.modelLabel.textContent = state.models.length ? t("未选择模型") : t("未配置模型");
      elements.modelLabel.title = `${elements.modelLabel.textContent}（${scope}）`;
      return;
    }
    if (pool.length > 1) {
      const title = pool.map((model) => `${model.provider_name || model.provider_id || ""} · ${model.model || ""}`).join("\n");
      elements.modelLabel.textContent = t("混合模型 · {count}", {count: pool.length});
      elements.modelLabel.title = `${scope}\n${title}`;
      return;
    }
    const selected = pool[0];
    // 档位并进按钮文字——它原本有自己的按钮,合并后这里是唯一能看到它的地方。
    const pin = sessionPinFor(modelKey(selected));
    const level = pin === MODEL_DEFAULT_PIN ? null : pin ?? globalVariantFor(modelKey(selected));
    const name = String(selected.model || "");
    elements.modelLabel.textContent = level == null ? name : `${name} · ${thinkingVariantLabel(level, true)}`;
    elements.modelLabel.title = `${selected.provider_name || selected.provider_id || ""} · ${selected.model || ""}（${scope}）`;
  }

  function refreshLiveEndpointVisibility() {
    for (const live of state.liveRuns.values()) {
      if (!live.endpoint) continue;
      const values = [live.providerId, live.model].map((value) => String(value || "").trim()).filter(Boolean);
      live.endpoint.hidden = !state.display?.show_mixed_model_endpoint || values.length === 0;
    }
  }

  function resetModelMenuStaging() {
    const override = viewSessionModelOverride();
    state.stagedFollowGlobal = !override;
    state.stagedModelKeys = new Set((override || []).map(modelKey));
    // 思考档位以前是另一个按钮、另一个浮层,即点即写。现在它和模型选择合成
    // 一个面板,就得跟模型选择一样先暂存,由同一个「确认」一起提交——否则同一
    // 个面板里一半改动立刻生效、一半要按确认,「取消」也说不清取消的是什么。
    // 暂存的是这个会话的钉子:值为 null 就是跟随全局(09-24:effort 做成会话级)。
    state.stagedVariants = new Map(
      state.thinkingVariantModels.map((model) => [modelKey(model), sessionPinFor(modelKey(model))])
    );
    state.expandedLevelKey = null;
    state.modelMenuTouched = false;
    state.modelMenuError = "";
  }

  /// 某个模型可选的档位;没有可配置档位的模型返回空数组(那一行就不长小片)。
  function variantOptionsFor(key) {
    const entry = state.thinkingVariantModels.find((model) => modelKey(model) === key);
    return entry ? entry.variants : [];
  }

  /// 暂存的钉子;null = 跟随全局。
  function stagedVariantFor(key) {
    if (state.stagedVariants instanceof Map && state.stagedVariants.has(key)) {
      return state.stagedVariants.get(key);
    }
    return sessionPinFor(key);
  }

  /// 小片上显示的:钉了显示钉的,没钉显示全局那一档。
  function stagedEffectiveVariant(key) {
    return stagedVariantFor(key) ?? globalVariantFor(key);
  }

  function modelMenuStaging() {
    if (state.stagedModelKeys instanceof Set) {
      return { follow: state.stagedFollowGlobal, keys: state.stagedModelKeys };
    }
    const override = viewSessionModelOverride();
    return { follow: !override, keys: new Set((override || []).map(modelKey)) };
  }

  function renderModelMenu() {
    // 重画整张列表会把滚动位置清零。展开档位、选档位都要重画,不记住就
    // 每次都弹回顶部,而用户正看着列表中间某一行。
    const scrollTop = elements.modelMenu.querySelector(".model-menu-list")?.scrollTop ?? 0;
    elements.modelMenu.replaceChildren();
    const staging = modelMenuStaging();
    const globalKeys = new Set(activeModels().map(modelKey));
    const list = document.createElement("div");
    list.className = "model-menu-list";
    list.setAttribute("role", "group");
    list.setAttribute("aria-label", t("可用模型"));

    const follow = document.createElement("button");
    follow.type = "button";
    follow.className = "model-menu-item model-menu-follow";
    follow.setAttribute("role", "menuitemcheckbox");
    follow.setAttribute("aria-checked", String(staging.follow));
    follow.classList.toggle("selected", staging.follow);
    const followCopy = document.createElement("span");
    followCopy.className = "model-menu-copy";
    const followName = document.createElement("strong");
    followName.textContent = t("跟随全局");
    const followHint = document.createElement("small");
    followHint.textContent = t("使用全局激活模型池");
    followCopy.append(followName, followHint);
    const followCheck = document.createElement("span");
    followCheck.className = "icon-slot check-slot";
    followCheck.setAttribute("aria-hidden", "true");
    if (staging.follow) followCheck.appendChild(createIcon("check"));
    follow.append(followCopy, followCheck);
    follow.addEventListener("click", chooseFollowGlobal);
    list.appendChild(follow);

    for (const model of state.models) {
      if (!model || typeof model !== "object") continue;
      const button = document.createElement("button");
      button.type = "button";
      button.className = "model-menu-item";
      button.setAttribute("role", "menuitemcheckbox");
      button.dataset.modelKey = modelKey(model);
      const checked = staging.follow ? globalKeys.has(button.dataset.modelKey) : staging.keys.has(button.dataset.modelKey);
      const selected = checked && !staging.follow;
      button.setAttribute("aria-checked", String(checked));
      button.classList.toggle("selected", selected);
      button.classList.toggle("from-global", checked && staging.follow);

      const copy = document.createElement("span");
      copy.className = "model-menu-copy";
      const name = document.createElement("strong");
      name.textContent = String(model.model || "");
      const provider = document.createElement("small");
      provider.textContent = String(model.provider_name || model.provider_id || "");
      copy.append(name, provider);
      const check = document.createElement("span");
      check.className = "icon-slot check-slot";
      check.setAttribute("aria-hidden", "true");
      if (checked) check.appendChild(createIcon("check"));
      button.append(copy, check);
      button.addEventListener("click", () => toggleStagedModel(button.dataset.modelKey));

      // 档位小片和展开的档位行都得在这个按钮外面——按钮里套按钮是非法嵌套,
      // 浏览器会把内层拎出去,点击就落到外层的「选中模型」上。
      const key = button.dataset.modelKey;
      const variants = variantOptionsFor(key);
      if (!variants.length) {
        list.appendChild(button);
        continue;
      }
      const row = document.createElement("div");
      row.className = "model-menu-row";
      const chip = document.createElement("button");
      chip.type = "button";
      chip.className = "model-level-chip";
      chip.setAttribute("aria-expanded", String(state.expandedLevelKey === key));
      const pinned = stagedVariantFor(key) != null;
      chip.classList.toggle("is-pinned", pinned);
      chip.title = pinned
        ? t("思考程度：{level}（本会话）", {level: thinkingVariantLabel(stagedEffectiveVariant(key))})
        : t("思考程度：{level}（跟随全局）", {level: thinkingVariantLabel(stagedEffectiveVariant(key))});
      const chipText = document.createElement("span");
      chipText.textContent = thinkingVariantLabel(stagedEffectiveVariant(key), true);
      chip.append(chipText, makeIconSlot("chevron-down"));
      chip.addEventListener("click", (event) => {
        event.stopPropagation();
        if (state.expandedLevelKey === key) closeLevelMenu();
        else openLevelMenu(key, chip, model.model);
      });
      row.append(button, chip);
      list.appendChild(row);
    }

    const footer = document.createElement("footer");
    footer.className = "model-menu-footer";
    footer.setAttribute("role", "none");
    const feedback = document.createElement("span");
    feedback.className = "model-menu-feedback";
    feedback.setAttribute("role", "status");
    feedback.setAttribute("aria-live", "polite");
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "model-cancel";
    cancel.setAttribute("role", "menuitem");
    cancel.textContent = t("取消");
    cancel.addEventListener("click", () => closeModelMenu({ restoreFocus: true }));
    const confirm = document.createElement("button");
    confirm.type = "button";
    confirm.className = "model-confirm";
    confirm.setAttribute("role", "menuitem");
    confirm.textContent = t("确认");
    confirm.addEventListener("click", confirmModelSelection);
    footer.append(feedback, cancel, confirm);
    elements.modelMenu.append(list, footer);
    if (scrollTop) list.scrollTop = scrollTop;
    // 展开/收起档位会改变菜单高度，位置要跟着重算。
    positionModelMenu();
    updateModelMenuState();
    updateCurrentModelDisplay();
    refreshLiveEndpointVisibility();
    updateControlState();
  }

  function updateModelMenuState() {
    const staging = modelMenuStaging();
    const globalKeys = new Set(activeModels().map(modelKey));
    elements.modelMenu.querySelectorAll(".model-menu-item").forEach((button) => {
      const isFollowItem = button.classList.contains("model-menu-follow");
      const key = button.dataset.modelKey || "";
      const checked = isFollowItem
        ? staging.follow
        : (staging.follow ? globalKeys.has(key) : staging.keys.has(key));
      button.classList.toggle("selected", checked && (isFollowItem || !staging.follow));
      button.classList.toggle("from-global", !isFollowItem && checked && staging.follow);
      button.setAttribute("aria-checked", String(checked));
      button.disabled = state.blocked || state.modelSelectionSubmitting;
      const check = button.querySelector(".check-slot");
      if (check) check.replaceChildren(...(checked ? [createIcon("check")] : []));
    });
    const feedback = elements.modelMenu.querySelector(".model-menu-feedback");
    if (feedback) {
      const following = staging.follow || staging.keys.size === 0;
      feedback.textContent = state.modelMenuError
        || (following ? t("跟随全局激活模型池") : t("已选择 {count} 个模型（仅本会话）", {count: formatInteger(staging.keys.size)}));
      feedback.classList.toggle("is-error", Boolean(state.modelMenuError));
    }
    const confirm = elements.modelMenu.querySelector(".model-confirm");
    if (confirm) {
      confirm.textContent = state.modelSelectionSubmitting ? t("正在应用") : t("确认");
      confirm.disabled = state.modelSelectionSubmitting || state.blocked;
    }
    const cancel = elements.modelMenu.querySelector(".model-cancel");
    if (cancel) cancel.disabled = state.modelSelectionSubmitting;
  }

  function chooseFollowGlobal() {
    if (!(state.stagedModelKeys instanceof Set) || state.modelSelectionSubmitting) return;
    state.stagedFollowGlobal = true;
    state.stagedModelKeys = new Set();
    state.modelMenuTouched = true;
    state.modelMenuError = "";
    updateModelMenuState();
  }

  /// 档位选项做成独立浮层,挂在 composer-dock 上。
  ///
  /// 内联铺开会把下面的模型整体往下顶,列表本来就长,一展开就更难找；浮层
  /// 又不能放进 `.model-menu`——那个为了圆角开了 overflow: hidden,列表自己
  /// 还滚动,浮层会被切掉。所以和模型菜单平级,自己算位置。
  function openLevelMenu(key, chip, modelName) {
    const variants = variantOptionsFor(key);
    if (!variants.length) return;
    state.expandedLevelKey = key;
    const menu = elements.modelLevelMenu;
    menu.replaceChildren();
    menu.setAttribute("aria-label", t("{model} 的思考程度", {model: modelName}));
    for (const variant of [null, MODEL_DEFAULT_PIN, ...variants]) {
      const staged = stagedVariantFor(key) === variant;
      const option = document.createElement("button");
      option.type = "button";
      option.className = "model-level-option";
      option.setAttribute("role", "radio");
      option.setAttribute("aria-checked", String(staged));
      option.classList.toggle("selected", staged);
      const global = thinkingVariantLabel(globalVariantFor(key));
      option.textContent = variant == null
        ? t("跟随全局（{level}）", {level: global})
        : thinkingVariantLabel(variant);
      option.title = variant == null
        ? t("不钉档位，用全局默认档")
        : variant === MODEL_DEFAULT_PIN ? t("使用模型默认设置") : String(variant);
      option.addEventListener("click", (event) => {
        event.stopPropagation();
        stageVariant(key, variant);
      });
      menu.appendChild(option);
    }
    menu.hidden = false;
    chip.setAttribute("aria-expanded", "true");
    positionLevelMenu(chip);
  }

  function positionLevelMenu(chip) {
    const menu = elements.modelLevelMenu;
    if (menu.hidden) return;
    const dock = elements.composerDock.getBoundingClientRect();
    const anchor = chip.getBoundingClientRect();
    const margin = 8;
    const width = menu.offsetWidth * UI_SCALE;
    const height = menu.offsetHeight * UI_SCALE;
    // 贴小片右缘往左展开,竖直方向和小片对齐;上下都夹回视口。
    const left = Math.min(
      Math.max(margin, anchor.right - width),
      Math.max(margin, window.innerWidth - width - margin)
    );
    const top = Math.min(
      Math.max(margin, anchor.top - 4),
      Math.max(margin, window.innerHeight - height - margin)
    );
    menu.style.left = `${visualPixelsToLayout(left - dock.left)}px`;
    menu.style.top = `${visualPixelsToLayout(top - dock.top)}px`;
  }

  function closeLevelMenu() {
    if (elements.modelLevelMenu.hidden) return;
    elements.modelLevelMenu.hidden = true;
    state.expandedLevelKey = null;
    elements.modelMenu
      .querySelectorAll('.model-level-chip[aria-expanded="true"]')
      .forEach((chip) => chip.setAttribute("aria-expanded", "false"));
  }

  function stageVariant(key, variant) {
    if (!(state.stagedVariants instanceof Map) || state.modelSelectionSubmitting) return;
    state.stagedVariants.set(key, variant);
    closeLevelMenu();
    state.modelMenuTouched = true;
    state.modelMenuError = "";
    renderModelMenu();
  }

  function toggleStagedModel(key) {
    if (!(state.stagedModelKeys instanceof Set) || state.modelSelectionSubmitting) return;
    if (state.stagedFollowGlobal) {
      // 退出跟随模式：以当前显示的全局激活池为起点继续多选。
      state.stagedFollowGlobal = false;
      state.stagedModelKeys = new Set(activeModels().map(modelKey));
    }
    if (state.stagedModelKeys.has(key)) state.stagedModelKeys.delete(key);
    else state.stagedModelKeys.add(key);
    state.modelMenuTouched = true;
    state.modelMenuError = "";
    updateModelMenuState();
  }

  function newestLiveRun() {
    let latest = null;
    for (const live of state.liveRuns.values()) latest = live;
    return latest;
  }

  function deriveConversationDetails() {
    const live = newestLiveRun();
    if (state.turns.length === 0) {
      const liveUser = live?.userText || state.pendingSubmission?.content || "";
      if (!liveUser) return { title: t("新对话"), snippet: t("尚未开始"), timestamp: null };
      return { title: firstLine(liveUser) || t("新对话"), snippet: firstLine(liveUser), timestamp: new Date() };
    }
    const firstTurn = state.turns[0];
    const lastTurn = state.turns[state.turns.length - 1];
    const followups = Array.isArray(lastTurn?.followups) ? lastTurn.followups : [];
    const lastFollowup = followups[followups.length - 1];
    const assistant = String(lastTurn?.assistant_content || "").trim();
    const liveContent = live ? String(live.userText || "").trim() : "";
    const snippet = firstLine(liveContent || assistant || lastFollowup?.content || lastTurn?.user_content || "");
    const timestamp = liveContent ? live?.startedAt : lastTurn?.assistant_timestamp || lastFollowup?.submitted_at || lastTurn?.user_timestamp;
    return {
      title: firstLine(state.firstUserContent || firstTurn?.user_content) || t("当前对话"),
      snippet: snippet || (lastTurn?.status === "running" ? t("正在回复") : t("对话已开始")),
      timestamp
    };
  }

  function multiSessionEnabled() {
    return Boolean(state.capabilities?.multi_conversation);
  }

  function sessionDisplayName(session) {
    const name = firstLine(session?.name || "");
    return name || t("新会话");
  }

  function findSession(sessionId) {
    const id = String(sessionId || "");
    return state.sessions.find((session) => String(session?.session_id) === id) || null;
  }


  function viewSessionEntry() {
    return state.viewSessionId ? findSession(state.viewSessionId) : null;
  }

  function trackRun(sessionId, runId) {
    const session = String(sessionId || "");
    const run = String(runId || "");
    if (!session || !run) return;
    let runs = state.runsBySession.get(session);
    if (!runs) {
      runs = new Set();
      state.runsBySession.set(session, runs);
    }
    runs.add(run);
  }

  function untrackRun(runId) {
    const run = String(runId || "");
    for (const [sessionId, runs] of state.runsBySession) {
      if (runs.delete(run) && runs.size === 0) state.runsBySession.delete(sessionId);
    }
  }

  function runSessionId(runId) {
    const run = String(runId || "");
    if (!run) return "";
    for (const [sessionId, runs] of state.runsBySession) {
      if (runs.has(run)) return sessionId;
    }
    return "";
  }

  function sessionHasRuns(sessionId) {
    return (state.runsBySession.get(String(sessionId || ""))?.size || 0) > 0;
  }

  function closeSessionMenu() {
    if (!state.sessionMenuFor) return;
    state.sessionMenuFor = null;
    renderSessionList();
  }

  function toggleSessionMenu(sessionId) {
    state.sessionMenuFor = state.sessionMenuFor === sessionId ? null : sessionId;
    renderSessionList();
    if (!state.sessionMenuFor) return;
    const item = elements.sessionItems.querySelector(`.session-item[data-session-id="${CSS.escape(sessionId)}"]`);
    const menu = item?.querySelector(".session-menu");
    if (menu) {
      const menuRect = menu.getBoundingClientRect();
      const listRect = elements.sessionList.getBoundingClientRect();
      if (menuRect.bottom > listRect.bottom - 4) menu.classList.add("open-up");
      window.requestAnimationFrame(() => menu.querySelector("button")?.focus());
    }
  }

  function beginSessionRename(sessionId) {
    state.sessionRenaming = sessionId;
    renderSessionList();
  }

  function cancelSessionRename() {
    state.sessionRenaming = null;
    renderSessionList();
  }

  async function commitSessionRename(sessionId, value) {
    if (state.sessionRenaming !== sessionId) return;
    state.sessionRenaming = null;
    const session = findSession(sessionId);
    const name = String(value || "").trim();
    if (!session || !name || name === String(session.name || "").trim()) {
      renderSessionList();
      return;
    }
    try {
      await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}`, {
        method: "PATCH",
        body: JSON.stringify({ name })
      });
      session.name = name;
      showToast(t("会话已重命名"));
    } catch (error) {
      showToast(error.message || t("重命名失败"), "error");
    }
    renderSessionList();
    if (sessionId === state.viewSessionId) updateConversationChrome();
  }

  function buildSessionMenu(session, isDefault) {
    const id = String(session?.session_id || "");
    const menu = document.createElement("div");
    menu.className = "session-menu";
    menu.setAttribute("role", "menu");
    menu.setAttribute("aria-label", t("会话操作：{name}", {name: sessionDisplayName(session)}));
    // 终端集成会话是固定入口:不可改名、不可删除、不可被顶替,
    // 菜单只留「清空对话」;其余会话不再提供「设为默认」。
    const actions = [];
    if (!isDefault) actions.push({ label: t("重命名"), handler: () => beginSessionRename(id) });
    // 清空对本来只给默认会话（它不能改名/删除，拿这个顶位），可普通会话一样
    // 需要「留着会话、只丢历史」——删掉重建会连模型/工作目录覆盖一起丢。
    actions.push({ label: t("清空对话"), handler: requestClearConversation });
    // 多选(09-24):从这一条开始勾,之后在列表顶上的操作栏里批量删除。
    if (!isDefault && window.MiyuSessionSelect) {
      actions.push({ label: t("多选…"), handler: () => window.MiyuSessionSelect.enter(id) });
    }
    if (!isDefault) actions.push({ label: t("删除"), danger: true, handler: () => deleteSession(id) });
    for (const action of actions) {
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute("role", "menuitem");
      if (action.danger) button.classList.add("is-danger");
      button.textContent = action.label;
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        closeSessionMenu();
        action.handler();
      });
      menu.appendChild(button);
    }
    return menu;
  }

  // 终端集成会话（固定 id "default"）不在侧栏列出：它是 shellhook 那条车道，
  // 由终端驱动，在 WebUI 的会话列表里既不该被误点进去、更不该被误删。真要看
  // 它的历史，用 REPL 的 /session 切过去。
  function isTerminalSession(sessionId) {
    return String(sessionId || "") === "default";
  }

  function buildSessionItem(session) {
    const id = String(session?.session_id || "");
    const isView = Boolean(id) && (id === state.viewSessionId || id === state.switchingToSessionId);
    // 终端集成会话固定为 id "default",不再跟随可变的全局指针。
    const isDefault = id === "default";
    const item = document.createElement("div");
    item.className = `session-item${isView ? " active" : ""}`;
    item.dataset.sessionId = id;

    const renaming = state.sessionRenaming === id;
    const selecting = Boolean(window.MiyuSessionSelect?.active()) && !isDefault;
    // 侧栏拖拽排序(组内):HTML5 DnD,drop 时全量提交新顺序。选择模式下不拖。
    if (!renaming && !selecting) attachSessionDrag(item, session, id);
    const main = document.createElement(renaming ? "div" : "button");
    main.className = `session-item-main${renaming ? " is-renaming" : ""}`;
    if (!renaming) {
      main.type = "button";
      main.title = isView ? sessionDisplayName(session) : t("查看「{name}」", {name: sessionDisplayName(session)});
      // 选择模式下点一行是勾选,不是打开(09-24 批量删除)。
      main.addEventListener("click", () => {
        if (window.MiyuSessionSelect?.active()) window.MiyuSessionSelect.toggle(id);
        else openSessionView(id);
      });
    }
    // 行首那一格只放状态指示器。模式图标搬去了分组标题——同一组里每行都
    // 画一遍相同的图标，重复十几次也说不出新东西，还占着状态该用的位置。
    // 空着的时候格子仍在，文字左缘不会因为有没有指示器而移位。
    const lead = document.createElement("span");
    lead.className = "session-lead";
    if (sessionHasRuns(id)) {
      const spinner = document.createElement("span");
      spinner.className = "session-run-spinner";
      spinner.title = t("有回复正在运行");
      spinner.textContent = BRAILLE_FRAMES[state.brailleFrame % BRAILLE_FRAMES.length];
      lead.appendChild(spinner);
    } else if (state.unreadSessions.has(id)) {
      const dot = document.createElement("span");
      dot.className = "session-unread-dot";
      dot.title = t("有未读的新回复");
      lead.appendChild(dot);
    }
    main.appendChild(lead);
    if (selecting) window.MiyuSessionSelect.decorateItem(item, main, lead, id);

    const copy = document.createElement("span");
    copy.className = "session-copy";
    if (renaming) {
      const input = document.createElement("input");
      input.className = "session-rename-input";
      input.type = "text";
      input.value = String(session?.name || "");
      input.maxLength = 200;
      input.setAttribute("aria-label", t("会话名称"));
      input.addEventListener("click", (event) => event.stopPropagation());
      input.addEventListener("keydown", (event) => {
        event.stopPropagation();
        if (event.key === "Enter") {
          event.preventDefault();
          commitSessionRename(id, input.value);
        } else if (event.key === "Escape") {
          event.preventDefault();
          cancelSessionRename();
        }
      });
      input.addEventListener("blur", () => {
        if (state.sessionRenaming === id) commitSessionRename(id, input.value);
      });
      copy.appendChild(input);
      window.requestAnimationFrame(() => {
        input.focus();
        input.select();
      });
    } else {
      const titleRow = document.createElement("span");
      titleRow.className = "session-title-row";
      const title = document.createElement("strong");
      title.textContent = sessionDisplayName(session);
      titleRow.appendChild(title);
      if (isDefault) {
        const badge = document.createElement("span");
        badge.className = "session-default-badge";
        badge.textContent = t("默认");
        badge.title = t("CLI 与快捷入口的默认会话");
        titleRow.appendChild(badge);
      }
      copy.appendChild(titleRow);
    }

    // Gemini-style list rows: name only; details live in the hover tooltip.
    if (!renaming) {
      const snippet = firstLine(session?.last_user_content || "");
      const sandbox = String(session?.sandbox || "").trim();
      // 只锁写的会话要和读写都锁的区分开，不然悬浮提示里长得一样。
      const sandboxLine = sandbox
        ? t("sandbox: {mode}{note}", {mode: sandbox, note: session?.sandbox_read_all ? t("（只锁写）") : ""})
        : "";
      const details = [snippet, sandboxLine].filter(Boolean).join("\n");
      if (details) {
        main.title = `${sessionDisplayName(session)}\n${details}`;
      }
    }

    main.appendChild(copy);
    item.appendChild(main);

    const trailing = document.createElement("span");
    trailing.className = "session-trailing";

    const menuButton = document.createElement("button");
    menuButton.type = "button";
    menuButton.className = "session-menu-button";
    menuButton.title = t("会话操作");
    menuButton.setAttribute("aria-label", t("会话操作：{name}", {name: sessionDisplayName(session)}));
    menuButton.setAttribute("aria-haspopup", "menu");
    menuButton.setAttribute("aria-expanded", String(state.sessionMenuFor === id));
    menuButton.appendChild(makeIconSlot("ellipsis"));
    menuButton.addEventListener("click", (event) => {
      event.stopPropagation();
      toggleSessionMenu(id);
    });
    menuButton.hidden = selecting;
    trailing.appendChild(menuButton);
    item.appendChild(trailing);

    if (state.sessionMenuFor === id) item.appendChild(buildSessionMenu(session, isDefault));
    return item;
  }

  function buildFallbackSessionItem() {
    const details = deriveConversationDetails();
    const item = document.createElement("div");
    item.className = "session-item active";
    const main = document.createElement("button");
    main.type = "button";
    main.className = "session-item-main";
    main.title = details.title;
    main.appendChild(makeIconSlot("message-circle"));
    const copy = document.createElement("span");
    copy.className = "session-copy";
    const title = document.createElement("strong");
    title.textContent = details.title;
    const snippet = document.createElement("small");
    snippet.className = "session-snippet";
    snippet.textContent = details.snippet;
    snippet.title = details.snippet;
    copy.append(title, snippet);
    main.appendChild(copy);
    main.addEventListener("click", () => {
      closeSidebar();
      scrollToBottom({ force: true, smooth: true });
    });
    item.appendChild(main);
    const trailing = document.createElement("span");
    trailing.className = "session-trailing";
    const time = document.createElement("span");
    time.className = "session-time";
    time.textContent = details.timestamp ? formatRelativeTime(details.timestamp) : "";
    trailing.appendChild(time);
    item.appendChild(trailing);
    return item;
  }

  function renderSessionList() {
    if (!elements.sessionItems) return;
    if (state.sessionRenaming && elements.sessionItems.querySelector(".session-rename-input")) return;
    elements.sessionItems.replaceChildren();
    if (!multiSessionEnabled() || state.sessions.length === 0) {
      elements.sessionItems.appendChild(buildFallbackSessionItem());
      return;
    }
    // 侧栏按会话模式分组(创建时定死)。终端集成会话不列出——它是 shellhook
    // 那条车道,由终端驱动,WebUI 里既不该被误点进去也不该被误删;要看它的
    // 历史用 REPL 的 /session 切过去。
    const normal = state.sessions.filter(
      (session) => !isTerminalSession(session?.session_id) && session?.mode !== "dev"
    );
    const dev = state.sessions.filter(
      (session) => !isTerminalSession(session?.session_id) && session?.mode === "dev"
    );
    if (window.MiyuSessionSelect?.active()) {
      elements.sessionItems.appendChild(window.MiyuSessionSelect.buildBar());
    }
    if (normal.length) {
      elements.sessionItems.appendChild(buildSessionGroupHeader(t("普通模式"), "message-circle"));
      for (const session of normal) elements.sessionItems.appendChild(buildSessionItem(session));
    }
    if (dev.length) {
      elements.sessionItems.appendChild(buildSessionGroupHeader(t("开发模式"), "code"));
      for (const session of dev) elements.sessionItems.appendChild(buildSessionItem(session));
    }
  }

  // 和 REPL 的 `wait_spinner.rs::BRAILLE_FRAMES` 同一组帧。
  const BRAILLE_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

  /// 一个计时器喂所有转圈。
  ///
  /// 每个转圈各起一个 interval 的话,列表一重画就要收拾一批计时器,漏一个就
  /// 是一个永远跑下去的定时器;而且各自起跑点不同,几行并排时相位乱跳。
  /// 共用一个帧号还有个好处:重画时新建的元素直接落在当前帧上,不会从头闪。
  function startBrailleTicker() {
    window.setInterval(() => {
      if (document.hidden) return;
      const spinners = document.querySelectorAll(".session-run-spinner");
      if (!spinners.length) return;
      state.brailleFrame = (state.brailleFrame + 1) % BRAILLE_FRAMES.length;
      const glyph = BRAILLE_FRAMES[state.brailleFrame];
      for (const spinner of spinners) spinner.textContent = glyph;
    }, 90);
  }

  function clearSessionDropMarkers() {
    if (!elements.sessionItems) return;
    for (const el of elements.sessionItems.querySelectorAll(".drop-before, .drop-after")) {
      el.classList.remove("drop-before", "drop-after");
    }
  }

  function attachSessionDrag(item, session, id) {
    item.draggable = true;
    item.addEventListener("dragstart", (event) => {
      state.sessionDragId = id;
      item.classList.add("is-dragging");
      event.dataTransfer.effectAllowed = "move";
      try { event.dataTransfer.setData("text/plain", id); } catch (_) { /* 老内核 */ }
    });
    item.addEventListener("dragend", () => {
      state.sessionDragId = null;
      item.classList.remove("is-dragging");
      clearSessionDropMarkers();
    });
    item.addEventListener("dragover", (event) => {
      const dragId = state.sessionDragId;
      if (!dragId || dragId === id) return;
      // 只在同一分组(普通/dev)内排序,跨组语义(改会话模式)不存在。
      const dragging = findSession(dragId);
      if (!dragging || (dragging?.mode === "dev") !== (session?.mode === "dev")) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "move";
      const rect = item.getBoundingClientRect();
      const before = event.clientY < rect.top + rect.height / 2;
      clearSessionDropMarkers();
      item.classList.add(before ? "drop-before" : "drop-after");
    });
    item.addEventListener("dragleave", (event) => {
      if (event.relatedTarget && item.contains(event.relatedTarget)) return;
      item.classList.remove("drop-before", "drop-after");
    });
    item.addEventListener("drop", (event) => {
      const dragId = state.sessionDragId;
      if (!dragId || dragId === id) return;
      event.preventDefault();
      const before = item.classList.contains("drop-before");
      clearSessionDropMarkers();
      state.sessionDragId = null;
      commitSessionReorder(dragId, id, before);
    });
  }

  async function commitSessionReorder(dragId, targetId, before) {
    const list = state.sessions;
    const from = list.findIndex((s) => String(s?.session_id) === String(dragId));
    if (from < 0) return;
    const [moved] = list.splice(from, 1);
    let to = list.findIndex((s) => String(s?.session_id) === String(targetId));
    if (to < 0) {
      list.splice(from, 0, moved);
      return;
    }
    list.splice(before ? to : to + 1, 0, moved);
    renderSessionList();
    // 全量提交当前顺序(两组按数组序混排;后端按序重写 sort_key,分组是
    // 前端展示层的事)。终端车道会话不参与。
    const ids = list
      .filter((s) => !isTerminalSession(s?.session_id))
      .map((s) => String(s.session_id));
    state.lastReorderIds = ids.join("\n");
    try {
      await apiRequest("/api/sessions/order", {
        method: "PUT",
        body: JSON.stringify({ session_ids: ids })
      });
    } catch (error) {
      showToast(error.message || t("排序保存失败"), "error");
      refreshSessions();
    }
  }

  function buildSessionGroupHeader(label, icon) {
    const header = document.createElement("div");
    header.className = "session-group-header";
    if (icon) header.appendChild(makeIconSlot(icon));
    const text = document.createElement("span");
    text.textContent = label;
    header.appendChild(text);
    return header;
  }

  async function refreshSessions() {
    try {
      const response = await apiRequest("/api/sessions");
      const payload = await response.json();
      state.sessions = Array.isArray(payload?.sessions) ? payload.sessions : [];
      renderSessionList();
      updateConversationChrome();
    } catch (_) {
      // 后续 SSE 或 bootstrap 会补齐会话列表。
    }
  }

  function setSessionBusy(value) {
    state.sessionBusy = Boolean(value);
    updateControlState();
  }

  async function createSession(mode) {
    if (state.blocked || state.sessionBusy || state.adminBusy || state.submitting) return;
    setSessionBusy(true);
    try {
      const response = await apiRequest("/api/sessions", {
        method: "POST",
        body: JSON.stringify(mode === "dev" ? { mode: "dev" } : {})
      });
      const payload = await response.json();
      const record = payload?.session && typeof payload.session === "object" ? payload.session : null;
      const sessionId = String(record?.session_id || "");
      if (sessionId && !findSession(sessionId)) {
        state.sessions.unshift(record);
        renderSessionList();
      }
      if (sessionId) await loadSessionView(sessionId);
      focusComposerIfDesktop();
    } catch (error) {
      showToast(error.message || t("新建会话失败"), "error");
    } finally {
      setSessionBusy(false);
    }
  }

  async function openSessionView(sessionId, { userInitiated = true } = {}) {
    if (!sessionId) return;
    if (sessionId === state.viewSessionId && !state.viewLoading) {
      closeSidebar();
      scrollToBottom({ force: true, smooth: true });
      return;
    }
    await loadSessionView(sessionId, { userInitiated });
  }

  async function loadSessionView(sessionId, { quiet = false, userInitiated = false } = {}) {
    if (!sessionId || (quiet && sessionId !== state.viewSessionId) || (state.viewLoading && !userInitiated)) return;
    // 命令回执是会话内的临时记录，换会话就清掉——否则会串到别的会话里。
    // 回执按会话记账（commands.js），切走再切回来仍在原位，这里不再清空。
    if (state.unreadSessions.delete(sessionId)) renderSessionList();
    const generation = ++state.viewLoadGeneration;
    state.viewLoading = true;
    // 先切后加载:用户点标签的一刻立刻高亮目标会话、收起侧栏、给对话区铺一层
    // 加载动画,大会话拉取期间不再像卡在旧会话上(09-12 用户报)。真正的视图
    // 由下面 applySessionView 拉回后应用。
    if (userInitiated && sessionId !== state.viewSessionId) {
      state.switchingToSessionId = sessionId;
      renderSessionList();
      closeSidebar();
      elements.conversationStage?.classList.add("is-switching");
    }
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/turns?limit=${TURN_PAGE_SIZE}`);
      const payload = await response.json();
      if (generation !== state.viewLoadGeneration) return;
      applySessionView(payload);
      if (!quiet) closeSidebar();
    } catch (error) {
      if (generation !== state.viewLoadGeneration) return;
      if (error.status === 401) showBlockedState(true);
      else if (error.status === 404) {
        showToast(t("会话不存在"), "error");
        refreshSessions();
        if (sessionId === state.viewSessionId) window.setTimeout(() => openFallbackSessionView(sessionId), 0);
      } else showToast(error.message || t("载入会话失败"), "error");
    } finally {
      if (generation === state.viewLoadGeneration) {
        state.viewLoading = false;
        state.switchingToSessionId = "";
        elements.conversationStage?.classList.remove("is-switching");
        updateControlState();
      }
    }
  }

  function disposeAllLiveRuns() {
    for (const live of state.liveRuns.values()) disposeLiveState(live);
    state.liveRuns.clear();
    elements.liveStopRail.replaceChildren();
    elements.liveStopRail.hidden = true;
  }

  // 切换会话不再销毁还在跑的直播状态:事件环只留 4096 条,长回复从 0 重放
  // 必撞 resync,已渲染的内容就永远回不来了。改成离屏保活——DOM 游离但事件
  // 照常写入,切回来原样重挂(reattachLiveArticles)。只清掉已结束的残壳。
  function retireLiveRunsForSwitch() {
    for (const [runId, live] of [...state.liveRuns.entries()]) {
      if (live.ended) {
        disposeLiveState(live);
        state.liveRuns.delete(runId);
      }
    }
    // 停止栏与问题坞都是全局元素,先清空;切回时按会话重挂。
    elements.liveStopRail.replaceChildren();
    elements.liveStopRail.hidden = true;
  }

  function applySessionView(payload) {
    const sessionId = String(payload?.session_id || "");
    if (!sessionId) return;
    if (state.viewSessionId && state.viewSessionId !== sessionId && state.composerAttachments.length) {
      clearComposerAttachments(true);
    }
    retireLiveRunsForSwitch();
    clearViewSyncTimer();
    // 「累计」的基线与子代理估算是按会话记的「只升不降」(#131)：换了会话不清，max 会
    // 把上一条会话的高值锁在这条上——删掉正在看的会话、自动切过去后「累计」还是被删
    // 那条的数（09-23 用户报；点侧栏切换也一样）。这条会话的权威累计由下面的
    // refreshSessionContext 取回。
    if (state.viewSessionId !== sessionId) resetSessionCumulative();
    state.viewSessionId = sessionId;
    // 子代理的会话:输入框上方挂「↑ 主会话」(会话项目第 4 段)。
    window.MiyuSubagents?.viewed(payload);
    // 记住浏览位置，刷新后回到这里而不是跳去终端车道（见 preferredBootSession）。
    if (!isTerminalSession(sessionId)) safeStorageSet(VIEW_SESSION_KEY, sessionId);
    if (state.sessionModelOverrideFor !== sessionId) {
      // 会话切换：先按"跟随全局"显示，再异步取回该会话的覆盖池。
      state.sessionModelOverride = null;
      state.sessionModelOverrideFor = "";
      updateCurrentModelDisplay();
      refreshSessionModelOverride(sessionId);
    }
    // 上下文条跟着看的会话走：不拉的话它一直显示上一个会话的数字，
    // 直到这个会话跑完一轮才被 run 事件纠正。
    refreshSessionContext(sessionId);
    state.turns = Array.isArray(payload?.turns)
      ? payload.turns.sort((a, b) => asFiniteNumber(a?.seq) - asFiniteNumber(b?.seq))
      : [];
    applyTurnPageMeta(payload);
    state.queuedPrompts = Array.isArray(payload?.queued_prompts) ? payload.queued_prompts : [];
    state.redoCandidate = payload?.redo_candidate && typeof payload.redo_candidate === "object"
      ? payload.redo_candidate
      : null;
    closeRevisionEditor();
    state.pendingSubmission = null;
    const runs = (Array.isArray(payload?.runs) ? payload.runs : []).filter((run) => run?.run_id);
    if (runs.length) state.runsBySession.set(sessionId, new Set(runs.map((run) => String(run.run_id))));
    else state.runsBySession.delete(sessionId);
    state.viewRunningTurnId = !runs.length && typeof payload?.running_turn_id === "string" && payload.running_turn_id
      ? payload.running_turn_id
      : null;
    renderConversation({ forceScroll: true });
    renderQueueTray();
    renderJobsStrip();
    restoreLiveRuns(runs);
    updateConversationChrome();
    updateControlState();
    scheduleViewSync();
    fillViewWithOlderTurns();
  }

  // 按页取回合带回来的三样（会话项目第 2 段）。整段取（老接口）时都是空的：
  // 没有更早的、从零累计、标题用第一轮。
  function applyTurnPageMeta(payload) {
    const older = payload?.older;
    state.turnsOlder = Number.isFinite(older) ? older : null;
    const base = payload?.tokens_before;
    state.turnsBase = base && typeof base === "object"
      ? { total: asFiniteNumber(base.total), prompt: asFiniteNumber(base.prompt), cached: asFiniteNumber(base.cache_read) }
      : null;
    state.firstUserContent = typeof payload?.first_user_content === "string" ? payload.first_user_content : "";
  }

  // 往上翻到顶再补更早的一页。补进来之后按离底部的距离把滚动位置拉回原处，
  // 看到的内容不动。
  async function loadOlderTurns() {
    const sessionId = state.viewSessionId;
    const before = state.turnsOlder;
    if (!sessionId || before === null || state.loadingOlderTurns || state.viewLoading) return;
    state.loadingOlderTurns = true;
    try {
      const response = await apiRequest(
        `/api/sessions/${encodeURIComponent(sessionId)}/turns?before=${encodeURIComponent(before)}&limit=${TURN_PAGE_SIZE}`
      );
      const payload = await response.json();
      if (state.viewSessionId !== sessionId || state.turnsOlder !== before) return;
      const older = Array.isArray(payload?.turns)
        ? payload.turns.sort((a, b) => asFiniteNumber(a?.seq) - asFiniteNumber(b?.seq))
        : [];
      const known = new Set(state.turns.map((turn) => String(turn?.id)));
      state.turns = older.filter((turn) => !known.has(String(turn?.id))).concat(state.turns);
      applyTurnPageMeta(payload);
      const scroller = elements.chatScroll;
      const fromBottom = scroller.scrollHeight - scroller.scrollTop;
      renderConversation();
      state.programmaticScroll = true;
      scroller.scrollTop = scroller.scrollHeight - fromBottom;
    } catch (error) {
      if (error.status === 401) showBlockedState(true);
    } finally {
      state.loadingOlderTurns = false;
    }
    fillViewWithOlderTurns();
  }

  // 一页铺不满对话区就没法往上滚，也就永远等不到「翻到顶」：还有更早的就接着补。
  function fillViewWithOlderTurns() {
    const scroller = elements.chatScroll;
    if (state.turnsOlder === null || !scroller) return;
    if (scroller.scrollHeight <= scroller.clientHeight) loadOlderTurns();
  }

  // 运行中每秒同步只取最近一页：往上翻补进来的更早几页留着，按这一页最早那轮的
  // 序号切开，前面的旧页接上这一页。
  function mergeLatestTurnPage(current, page, payload) {
    if (!page.length) {
      applyTurnPageMeta(payload);
      return page;
    }
    const first = asFiniteNumber(page[0]?.seq);
    const kept = current.filter((turn) => asFiniteNumber(turn?.seq) < first);
    // 更早的那几页已经在手上：游标和累计基数还是它们的，只有全靠这一页时才换。
    if (!kept.length) applyTurnPageMeta(payload);
    return kept.concat(page);
  }

  function findUnclaimedRunningTurn() {
    const claimed = new Set();
    for (const live of state.liveRuns.values()) {
      if (live.turnId) claimed.add(String(live.turnId));
    }
    return state.turns.find((turn) => turn?.status === "running" && !claimed.has(String(turn?.id))) || null;
  }

  function createLiveForRun(runId, userText = "", options = {}) {
    const { claimTurn = true, operation = "create", turnId = null, inputId = null } = options;
    const existing = state.liveRuns.get(runId);
    if (existing) return existing;
    const redo = operation === "redo";
    const runningTurn = redo || userText || !claimTurn ? null : findUnclaimedRunningTurn();
    const live = createLiveState(runId, {
      sessionId: options.sessionId,
      turnId: turnId || runningTurn?.id || null,
      userText: userText || runningTurn?.user_content || "",
      userAttachments: runningTurn?.attachments || [],
      startedAt: runningTurn?.user_timestamp || new Date(),
      userRendered: redo || Boolean(runningTurn),
      operation,
      inputId,
      editedContent: options.editedContent
    });
    state.liveRuns.set(runId, live);
    return live;
  }

  function beginRunReplay(runIds = null) {
    // 事件环形缓冲已滚过上限时,after=0 必然触发 resync_required →
    // bootstrap → 又 replay 的循环:短窗口内连续吃到 resync 就放弃从头
    // 重放,live 状态由 bootstrap 快照兜底,增量从当前事件 id 继续。
    const now = Date.now();
    if (state.replayResyncCount >= 2 && now - state.replayResyncAt < 15000) {
      state.replayRunIds = null;
      connectEventSource(state.lastEventId);
      return;
    }
    // 只重放传入的 run(全新空壳);离屏保活的 live 已吃过这些事件,再放
    // 一遍正文就翻倍了。
    state.replayRunIds = runIds ? new Set(runIds) : new Set(state.liveRuns.keys());
    state.replayCutoff = Math.max(state.lastEventId, state.replayCutoff, state.latestEventId);
    state.lastEventId = 0;
    connectEventSource(0);
  }

  // 把落库的这一回合(含回合中途检查点写下的子代理子过程)按实时事件的**同一套
  // handler** 回放进一个 live run:刷新/切会话重连时用它给 live 气泡「播种」,让后续
  // 实时事件无缝接上,不再另起空壳、也不再画重复卡(#5b 重连渲染重做)。用真 handler
  // 回放而不是自己搭 DOM,是为了让 live.tools/live.blocks/正文累计等内部状态和正常
  // 流式时完全一致——尤其正在跑的那次子代理调用不喂 tool.finished,留着让实时续。
  function seedLiveFromPersistedTurn(live, turn) {
    ensureLiveArticle(live);
    // 播种是回放历史,不该喂「累计」的实时子代理估算(否则已完成子代理会和后端基线
    // 重复计;#131)。置旗让 tool.progress 里那段 liveSubagentTokens 更新跳过。
    state.seedingLive = true;
    try {
      seedLiveRounds(live, turn);
    } finally {
      state.seedingLive = false;
    }
    // 播种是一次性灌进一大坨,子过程区停在顶部;若不拉到底,后续实时更新的
    // subStickBottom 会「测得改前不在底」→ 从此不再跟随(#159 刷新后不自动向下滚)。
    // 排在播种自身的 rAF 之后再拉一次底,让在跑的子代理接着贴底跟随。
    window.requestAnimationFrame(() => {
      for (const tool of live.tools.values()) {
        if (tool?.isTask && !tool.finished && tool.blocks) {
          const c = subScrollContainer(tool);
          if (c) c.scrollTop = c.scrollHeight;
        }
      }
    });
  }
  function seedLiveRounds(live, turn) {
    const rounds = Array.isArray(turn?.tool_flow) ? turn.tool_flow : [];
    for (const round of rounds) {
      const reasoning = String(round?.assistant_reasoning || "");
      if (reasoning.trim() && !reasoningHidden()) {
        handleReasoningEvent("reasoning.start", live, {});
        handleReasoningEvent("reasoning.delta", live, { delta: reasoning });
        handleReasoningEvent("reasoning.part_end", live, {});
      }
      const content = String(round?.assistant_content || "");
      if (content.trim()) appendAssistantDelta(live, content);
      for (const call of Array.isArray(round?.calls) ? round.calls : []) {
        handleToolEvent("tool.started", live, {
          tool_id: call?.id, name: call?.name,
          display_name: call?.display_name, arguments: call?.arguments,
        });
        // 在跑的前台子代理:检查点里记着它的子会话(会话项目第 4 段之二),刷新之后卡片照样
        // 点得进去。
        if (isSubagentTool(call?.name) && call?.child_session_id) {
          handleToolEvent("subagent.progress", live, {
            tool_id: call?.id, name: call?.name, session_id: call.child_session_id,
          });
        }
        const output = String(call?.output || "");
        // 有真实输出 = 这次调用已完成才收尾;检查点里在跑的那次 output 是空的(或
        // 派生时的占位「(tool result unavailable)」),不收尾——让它保持运行态,实时
        // 事件到了继续更新同一张卡。
        if (output && output !== "(tool result unavailable)") {
          handleToolEvent("tool.finished", live, {
            tool_id: call?.id, name: call?.name, output, ok: call?.ok !== false,
          });
        }
      }
    }
  }

  function restoreLiveRuns(runs) {
    // 只有全新空壳需要事件重放;离屏保活切回来的 live 内容都在,重放反而
    // 会把正文写两遍。
    const fresh = new Set();
    let seededConnect = false;
    // 正在跑的那条回合:create 的 runs 不带 turn_id,靠回合状态兜底认它。
    const runningTurn = state.turns.find((turn) => turn?.status === "running");
    for (const run of runs) {
      const runId = String(run?.run_id || "");
      if (!runId || state.terminalRunIds.has(runId)) continue;
      const kept = state.liveRuns.has(runId);
      const turnId = String(run?.turn_id || "") || (runningTurn ? String(runningTurn.id) : "");
      const turn = turnId ? state.turns.find((t) => String(t?.id) === turnId) : null;
      const live = createLiveForRun(runId, "", {
        operation: String(run?.operation || "create"),
        turnId: turnId || null,
        inputId: String(run?.input_id || "") || null
      });
      if (live.operation === "redo" && state.turns.some((turn) => {
        return String(turn?.id) === String(live.turnId) && turn?.status === "running";
      })) {
        live.redoCommitted = true;
      }
      // 这条重连回合已被 renderConversation 按落库快照画成了持久气泡,而且快照里有回合
      // 中途检查点写下的内容(#5a 起,子代理子过程也在)。这种情况把内容「播种」进
      // live 气泡、删掉那张持久气泡,而不是另起一个空壳叠上去(#5b:刷新后一个空
      // 「开发中」壳压在有内容的持久泡旁边);也不从 0 重放服务端事件(环缓冲早滚过
      // →resync→bootstrap 死循环,几十秒空白还停不掉——#3)。改增量续上。
      const canSeed = !kept && live.operation !== "redo" && turn && turn.status === "running"
        && ((Array.isArray(turn.tool_flow) && turn.tool_flow.length)
          || String(turn.assistant_content || "").trim());
      if (live.operation === "redo") {
        // redo 走原路(它自己会提交/重挂)。
      } else if (canSeed) {
        const persisted = [...elements.timeline.querySelectorAll(
          `article.assistant-message[data-turn-id="${turnId}"]`
        )].find((n) => !n.classList.contains("live-assistant"));
        ensureLiveArticle(live);
        seedLiveFromPersistedTurn(live, turn);
        showTypingIndicator(live);
        if (persisted) {
          // 把 live 气泡挪到持久气泡原位再删持久气泡,保持时间线顺序。
          if (persisted.parentNode === elements.timeline && live.article) {
            elements.timeline.insertBefore(live.article, persisted);
          }
          persisted.remove();
        }
        seededConnect = true;
      } else {
        // 立刻把气泡建出来,不等下一个事件。停止按钮和等待动效就都回来了。
        ensureLiveArticle(live);
        showTypingIndicator(live);
        if (!kept) fresh.add(runId);
      }
    }
    if (fresh.size) {
      beginRunReplay(fresh);
    } else if (seededConnect) {
      // 播种过、没有需要从 0 重放的空壳:仍要连上事件流看后续与收尾(applySessionView
      // 只在 liveRuns 为空时连,这里已非空)。从当前最新事件增量续上,不撞 resync。
      state.replayRunIds = null;
      state.lastEventId = Math.max(state.lastEventId, state.latestEventId);
      connectEventSource(state.lastEventId);
    }
  }

  async function openFallbackSessionView(excludedSessionId) {
    const excluded = String(excludedSessionId || "");
    if (state.viewSessionId !== excluded) return;
    // deleteSession() 和 session.deleted 事件会各来一次，且到达可能有先后：
    // 只防并发的旗标挡不住"第一次兜底完成后第二次才到"的时序，两边各建一个
    // 新会话，删一个凭空多出两个。按被删会话 id 上一次性闩锁：同一场删除，
    // 兜底只发生一次。
    if (state.fallbackInFlight || state.fallbackDoneFor === excluded) return;
    state.fallbackInFlight = true;
    state.fallbackDoneFor = excluded;
    try {
      await openFallbackSessionViewInner(excluded);
    } finally {
      state.fallbackInFlight = false;
    }
  }

  async function openFallbackSessionViewInner(excluded) {
    // 终端集成会话不能当兜底：它在侧栏里是隐藏的，掉进去看着就像「我的对话
    // 全没了」。一个可见会话都不剩时走 loadBootstrap()，让空状态兜底。
    const fallback = state.currentSessionId
      && state.currentSessionId !== excluded
      && !isTerminalSession(state.currentSessionId)
      ? state.currentSessionId
      : String(state.sessions.find((session) => {
          const id = String(session?.session_id || "");
          return id !== excluded && !isTerminalSession(id);
        })?.session_id || "");
    if (fallback) {
      await loadSessionView(fallback);
      return;
    }
    // 本地列表空了先跟服务端对一次：删最后一个会话时顶替的新会话由服务端建
    // （session.created 先于 session.deleted 广播，DELETE 回执里也带着），这里
    // 通常已经在列表里；SSE 掉过事件才会走到这一步。不这么对一次的话，每个
    // 开着该会话的页面都会自己 POST 一个，删一个多出两个（09-10 复现）。
    await refreshSessions();
    const refreshed = String(state.sessions.find((session) => {
      const id = String(session?.session_id || "");
      return id !== excluded && !isTerminalSession(id);
    })?.session_id || "");
    if (refreshed) {
      await loadSessionView(refreshed);
      return;
    }
    // 一个可见会话都不剩：直接新建一个顶上。落进空状态的话，用户面对的是一个
    // 不在侧栏里的「幽灵视图」，在里面打字实际写进隐藏的终端集成车道。
    // 不走 createSession()——删除流程还举着 sessionBusy，它会直接返回。
    try {
      const response = await apiRequest("/api/sessions", {
        method: "POST",
        body: JSON.stringify({}),
      });
      const record = (await response.json())?.session;
      const sessionId = String(record?.session_id || "");
      if (sessionId) {
        if (!findSession(sessionId)) {
          state.sessions.unshift(record);
          renderSessionList();
        }
        await loadSessionView(sessionId);
        return;
      }
    } catch (_) {
      // 新建失败（离线等）：退回空状态兜底，至少不落进隐藏车道。
    }
    await loadBootstrap();
  }

  async function deleteSession(sessionId) {
    const session = findSession(sessionId);
    if (!window.confirm(t("删除会话「{name}」？此操作无法撤销。", {name: sessionDisplayName(session)}))) return;
    if (state.sessionBusy) return;
    setSessionBusy(true);
    try {
      await deleteSessionRequest(sessionId);
      showToast(t("会话已删除"));
      renderSessionList();
      if (sessionId === state.viewSessionId) await openFallbackSessionView(sessionId);
    } catch (error) {
      showToast(error.message || t("删除失败"), "error");
    } finally {
      setSessionBusy(false);
    }
  }

  /// 删一条并把它从列表里拿掉。删的是最后一个会话时，服务端已经建好顶替的那个并随
  /// 回执带回；事件到达有先后，这里直接收进列表，兜底就不会再去新建。
  async function deleteSessionRequest(sessionId) {
    const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}`, { method: "DELETE" });
    state.sessions = state.sessions.filter((item) => String(item?.session_id) !== String(sessionId));
    const replacement = (await response.json().catch(() => null))?.fallback;
    const replacementId = String(replacement?.session_id || "");
    if (replacementId && !findSession(replacementId)) state.sessions.unshift(replacement);
  }

  /// 批量删除(sessionselect.js 调,09-24):逐个走单删接口、一个等一个——服务端删会话
  /// 要占全局的管理锁,并发发出去除了第一个都会 409。正在看的那条被删了,最后切到剩下
  /// 的某一条(和单删同一个兜底)。
  async function deleteSessions(ids) {
    if (state.sessionBusy || !ids.length) return;
    setSessionBusy(true);
    const viewed = String(state.viewSessionId || "");
    let deleted = 0;
    let failed = 0;
    try {
      for (const [index, id] of ids.entries()) {
        if (ids.length > 3) showToast(t("正在删除 {done}/{total}", {done: index + 1, total: ids.length}));
        try {
          await deleteSessionRequest(id);
          deleted += 1;
        } catch (_) {
          failed += 1;
        }
      }
      renderSessionList();
      if (ids.includes(viewed) && !findSession(viewed)) await openFallbackSessionView(viewed);
    } finally {
      setSessionBusy(false);
    }
    if (failed) showToast(t("删除了 {deleted} 个会话，{failed} 个没删成", {deleted, failed}), "error");
    else showToast(t("已删除 {count} 个会话", {count: deleted}));
  }

  function handleSessionEvent(name, data) {
    if (name === "session.reordered") {
      // 发起端已乐观重排(lastReorderIds 一致就不用刷);其它客户端拉一次。
      const ids = Array.isArray(data?.session_ids) ? data.session_ids.map(String).join("\n") : "";
      if (ids && ids !== state.lastReorderIds) refreshSessions();
      return;
    }
    const sessionId = String(data?.session_id || "");
    if (!sessionId) return;
    if (name === "session.created") {
      if (data?.platform) return;
      if (!findSession(sessionId)) {
        state.sessions.unshift({
          session_id: sessionId,
          name: String(data?.name || ""),
          kind: "",
          sandbox: "",
          sandbox_read_all: false,
          mode: data?.mode === "dev" ? "dev" : "normal",
          created_at: null,
          updated_at: new Date().toISOString(),
          turn_count: 0,
          last_user_content: ""
        });
        renderSessionList();
      }
    } else if (name === "session.renamed") {
      const target = findSession(sessionId);
      if (target) target.name = String(data?.name || "");
      renderSessionList();
      if (sessionId === state.viewSessionId) updateConversationChrome();
    } else if (name === "session.deleted") {
      state.sessions = state.sessions.filter((item) => String(item?.session_id) !== sessionId);
      renderSessionList();
      if (sessionId === state.viewSessionId && !state.bootstrapPromise && !state.viewLoading) {
        openFallbackSessionView(sessionId);
      }
    } else if (name === "session.updated") {
      const target = findSession(sessionId);
      if (target && Object.prototype.hasOwnProperty.call(data || {}, "sandbox")) {
        target.sandbox = String(data?.sandbox || "");
        target.sandbox_read_all = Boolean(data?.sandbox_read_all);
      }
      if (Object.prototype.hasOwnProperty.call(data || {}, "model_override") && sessionId === state.viewSessionId) {
        setSessionModelOverride(sessionId, data.model_override);
      }
      if (Object.prototype.hasOwnProperty.call(data || {}, "thinking_variants") && sessionId === state.viewSessionId) {
        setSessionThinkingPins(sessionId, data.thinking_variants);
      }
      renderSessionList();
      if (sessionId === state.viewSessionId) updateConversationChrome();
    } else if (name === "session.current_changed") {
      // 每视图独立浏览：默认会话只影响侧栏「默认」徽标，不再跟随切换。
      state.currentSessionId = sessionId;
      renderSessionList();
    }
  }

  // 顶栏没了,会话标题和「正在回复 · 工作区」那行副标题跟着没了——侧栏里
  // 本来就高亮着当前会话,标题是第二份;运行状态现在由侧栏的转圈和输入框那排
  // 的指示器表达,比一行小字显眼。剩下的是让侧栏重画。
  function updateConversationChrome() {
    renderSessionList();
  }

  // 离屏保活的 live 属于别的会话,不算「本视图在跑」。
  function liveViewed(live) {
    return !live?.sessionId || String(live.sessionId) === String(state.viewSessionId || "");
  }

  // 这个 turn 是否被本会话某个还在跑的 live 气泡认领:认领中的回合,
  // 持久化渲染只画用户消息——checkpoint 落库的部分正文与气泡是同一份内容,
  // 两边都画就是切回后正文翻倍。
  function liveClaimsTurn(turnId) {
    if (!turnId) return false;
    for (const live of state.liveRuns.values()) {
      if (!live.ended && liveViewed(live) && String(live.turnId) === String(turnId)) return true;
    }
    return false;
  }

  function conversationRunning() {
    for (const live of state.liveRuns.values()) {
      if (liveViewed(live)) return true;
    }
    return Boolean(state.viewRunningTurnId);
  }

  function activeTurnUpdateTarget(sessionId) {
    const runIds = state.runsBySession.get(String(sessionId || ""));
    if (!runIds) return null;
    const candidates = [...runIds]
      .map((runId) => state.liveRuns.get(String(runId)))
      .filter((live) => live && !live.ended && live.turnId);
    if (candidates.length !== 1) return null;
    return { runId: candidates[0].runId, turnId: candidates[0].turnId };
  }

  function hasPendingQuestion() {
    for (const live of state.liveRuns.values()) {
      for (const question of live.questions.values()) {
        if (question.pending) return true;
      }
    }
    return false;
  }

  function countCharacters(value) {
    return Array.from(String(value || "")).length;
  }

  // 触屏设备(手机/平板):没有悬停、指针粗。回车语义与自动聚焦都按它分岔。
  function isTouchComposer() {
    return window.matchMedia("(hover: none), (pointer: coarse)").matches;
  }

  // 触屏设备上程序化聚焦会弹出软键盘挡住内容，只在桌面端自动聚焦
  function focusComposerIfDesktop() {
    if (isTouchComposer()) return;
    elements.composerInput.focus();
  }

  function resizeComposer() {
    const input = elements.composerInput;
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, layoutViewportWidth() <= 760 ? 120 : 146)}px`;
    const count = countCharacters(input.value);
    elements.characterCount.textContent = `${formatInteger(count)} / 20,000`;
    elements.characterCount.hidden = count < 18_000;
    elements.characterCount.classList.toggle("is-error", count > MAX_CONTENT_CHARS);
    updateControlState();
    // 输入框多行增高时,artifact 浮层的让位高度跟着更新(#2)。
    if (state.artifactOpen) syncComposerDockHeight();
    window.requestAnimationFrame(updateJumpButtonOffset);
  }

  function formatFileSize(value) {
    const bytes = Math.max(0, asFiniteNumber(value));
    if (bytes < 1024) return `${Math.round(bytes)} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  function safeAttachmentUrl(value) {
    const raw = String(value || "").trim();
    if (!raw) return null;
    try {
      const url = new URL(raw, window.location.origin);
      if (url.origin !== window.location.origin || !url.pathname.startsWith("/api/attachments/") || url.pathname === "/api/attachments/") return null;
      return url.href;
    } catch (_) {
      return null;
    }
  }

  function attachmentSessionId() {
    return String(state.viewSessionId || state.currentSessionId || "");
  }

  function renderComposerAttachments() {
    const tray = elements.attachmentTray;
    tray.replaceChildren();
    tray.hidden = state.composerAttachments.length === 0;
    for (const item of state.composerAttachments) {
      const isImage = item.kind === "image" && item.previewUrl;
      const entry = document.createElement("div");
      entry.className = `attachment-item ${isImage ? "is-image" : "is-file"} is-${item.status}`;
      entry.title = item.status === "error" ? t("{name}: {error}", {name: item.name, error: item.error || t("上传失败")}) : item.name;
      if (isImage) {
        const image = document.createElement("img");
        image.src = item.previewUrl;
        image.alt = "";
        const fallback = document.createElement("span");
        fallback.className = "attachment-image-fallback";
        fallback.hidden = true;
        fallback.appendChild(makeIconSlot("circle-alert"));
        image.addEventListener("load", () => { fallback.hidden = true; }, { once: true });
        image.addEventListener("error", () => {
          image.hidden = true;
          fallback.hidden = false;
        }, { once: true });
        entry.append(image, fallback);
      } else {
        const icon = document.createElement("span");
        icon.className = "attachment-file-icon";
        const nameParts = String(item.name || "").split(".");
        const extension = nameParts.length > 1 ? nameParts.pop().toUpperCase() : "FILE";
        icon.textContent = extension.slice(0, 4);
        entry.appendChild(icon);
        const copy = document.createElement("span");
        copy.className = "attachment-item-copy";
        const name = document.createElement("strong");
        name.textContent = item.name;
        name.title = item.name;
        const meta = document.createElement("small");
        if (item.status === "uploading") meta.textContent = t("上传中 {percent}%", {percent: Math.round(item.progress || 0)});
        else if (item.status === "error") meta.textContent = item.error || t("上传失败");
        else meta.textContent = formatFileSize(item.size);
        copy.append(name, meta);
        entry.appendChild(copy);
      }
      if (item.status === "uploading") {
        const spinner = makeIconSlot("loader-circle", "attachment-spinner is-spinning");
        entry.appendChild(spinner);
      } else if (item.status === "error") {
        const retry = document.createElement("button");
        retry.type = "button";
        retry.className = "attachment-action";
        retry.title = t("重试上传");
        retry.setAttribute("aria-label", t("重试上传 {name}", {name: item.name}));
        retry.appendChild(makeIconSlot("refresh-cw"));
        retry.addEventListener("click", () => uploadComposerAttachment(item));
        entry.appendChild(retry);
      }
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "attachment-action attachment-remove";
      remove.title = t("移除附件");
      remove.setAttribute("aria-label", t("移除附件 {name}", {name: item.name}));
      remove.appendChild(makeIconSlot("x"));
      remove.addEventListener("click", () => removeComposerAttachment(item));
      entry.appendChild(remove);
      tray.appendChild(entry);
    }
    window.requestAnimationFrame(updateJumpButtonOffset);
  }

  function uploadComposerAttachment(item) {
    if (!item?.file || !item.sessionId) return;
    item.status = "uploading";
    item.progress = 0;
    item.error = "";
    renderComposerAttachments();
    updateControlState();
    const request = new XMLHttpRequest();
    item.request = request;
    request.open("POST", `/api/attachments?session_id=${encodeURIComponent(item.sessionId)}`);
    request.setRequestHeader("Accept", "application/json");
    request.setRequestHeader("Content-Type", item.file.type || "application/octet-stream");
    request.setRequestHeader("X-Miyu-Filename", encodeURIComponent(item.file.name));
    request.upload.addEventListener("progress", (event) => {
      if (!event.lengthComputable || item.request !== request) return;
      item.progress = Math.min(99, Math.round((event.loaded / event.total) * 100));
      renderComposerAttachments();
    });
    request.addEventListener("load", () => {
      if (item.request !== request) return;
      item.request = null;
      let payload = null;
      try { payload = JSON.parse(request.responseText || "null"); } catch (_) {}
      if (request.status >= 200 && request.status < 300 && payload?.id) {
        const uploadedPreview = payload.kind === "image" ? safeAttachmentUrl(payload.url) : null;
        if (uploadedPreview && item.previewUrl?.startsWith("blob:")) URL.revokeObjectURL(item.previewUrl);
        Object.assign(item, payload, {
          previewUrl: uploadedPreview || item.previewUrl,
          status: "ready",
          progress: 100,
          error: ""
        });
      } else {
        item.status = "error";
        item.error = payload?.error?.message || t("上传失败 ({status})", {status: request.status || t("网络错误")});
      }
      renderComposerAttachments();
      updateControlState();
    });
    request.addEventListener("error", () => {
      if (item.request !== request) return;
      item.request = null;
      item.status = "error";
      item.error = t("无法连接上传服务");
      renderComposerAttachments();
      updateControlState();
    });
    request.send(item.file);
  }

  function collectTransferFiles(transfer) {
    const files = [];
    const seen = new Set();
    const add = (file) => {
      if (!(file instanceof File)) return;
      const key = `${file.name}\0${file.size}\0${file.lastModified}\0${file.type}`;
      if (seen.has(key)) return;
      seen.add(key);
      files.push(file);
    };
    for (const item of Array.from(transfer?.items || [])) {
      if (item.kind === "file") add(item.getAsFile());
    }
    for (const file of Array.from(transfer?.files || [])) add(file);
    return files;
  }

  function addComposerFiles(files) {
    if (!state.capabilities?.attachments) return;
    const incoming = Array.isArray(files) ? files : Array.from(files || []);
    if (!incoming.length) return;
    const available = Math.max(0, MAX_ATTACHMENTS - state.composerAttachments.length);
    if (incoming.length > available) {
      showToast(t("每条消息最多添加 {max} 个附件，已忽略 {skipped} 个", {max: MAX_ATTACHMENTS, skipped: incoming.length - available}), "error");
    }
    const accepted = incoming.slice(0, available);
    for (const file of accepted) {
      if (!(file instanceof File) || file.size <= 0) {
        showToast(t("{name} 是空文件", {name: file?.name || t("附件")}), "error");
        continue;
      }
      const image = file.type.startsWith("image/");
      const item = {
        localId: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
        file,
        sessionId: attachmentSessionId(),
        name: file.name,
        mime: file.type,
        kind: image ? "image" : "text",
        size: file.size,
        status: "uploading",
        progress: 0,
        previewUrl: image ? URL.createObjectURL(file) : "",
        request: null,
        error: ""
      };
      state.composerAttachments.push(item);
      uploadComposerAttachment(item);
    }
    renderComposerAttachments();
    updateControlState();
  }

  function removeComposerAttachment(item, deleteRemote = true) {
    item.request?.abort();
    item.request = null;
    state.composerAttachments = state.composerAttachments.filter((candidate) => candidate !== item);
    if (item.previewUrl) URL.revokeObjectURL(item.previewUrl);
    if (deleteRemote && item.id && item.sessionId) {
      apiRequest(`/api/attachments/${encodeURIComponent(item.id)}?session_id=${encodeURIComponent(item.sessionId)}`, { method: "DELETE" }).catch(() => {});
    }
    renderComposerAttachments();
    updateControlState();
  }

  function clearComposerAttachments(deleteRemote = true) {
    for (const item of [...state.composerAttachments]) removeComposerAttachment(item, deleteRemote);
    elements.attachmentInput.value = "";
  }

  function committedComposerAttachments() {
    const attachments = state.composerAttachments.filter((item) => item.status === "ready").map((item) => ({
      id: item.id,
      url: item.url,
      name: item.name,
      mime: item.mime,
      kind: item.kind,
      size: item.size,
      width: item.width || 0,
      height: item.height || 0
    }));
    clearComposerAttachments(false);
    return attachments;
  }

  function updateJumpButtonOffset() {
    elements.jumpBottomButton.style.bottom = `${elements.composerDock.offsetHeight + 10}px`;
  }

  // 语音输入(流式听写):浏览器麦克风 → 16kHz PCM16 → WebSocket
  // /api/voice/stream → daemon → miyu-voice(VAD/分句/识别)→ 识别一句回一句,
  // 逐句填进输入框。按一下开始,再按一下或 Esc 结束;静默 10 秒 daemon 自动收。
  // 按钮只在 daemon 说语音功能已启用时显示;LAN 上的 http 页面拿不到麦克风
  // (浏览器安全策略),这时提示改用本机 REPL 的 /stt。
  /// 麦克风按钮显隐随 daemon 的语音开关;登录前这条 401,登录后要再拿一次。
  function refreshVoiceButton() {
    const button = elements.micButton;
    if (!button) return;
    apiRequest("/api/voice/status")
      .then((response) => response.json())
      .then((status) => {
        // 语音按钮只在 miyu voice 可用时才存在(用户);具体显 mic 还是 send 由
        // updateComposerControls 按有没有输入切换(空+语音可用=麦,有输入=发送)。
        state.voiceEnabled = Boolean(status?.enabled);
        updateControlState();
      })
      .catch(() => { state.voiceEnabled = false; updateControlState(); });
  }

  function wireMicButton() {
    const button = elements.micButton;
    const indicator = elements.voiceIndicator;
    if (!button) return;
    let session = null;
    const MAX_MS = 5 * 60_000;

    refreshVoiceButton();

    function setIndicator(shown) {
      if (indicator) indicator.hidden = !shown;
      if (!shown) setLevel(0);
    }

    function setLevel(level) {
      if (elements.voiceLevel) elements.voiceLevel.style.width = `${Math.round(Math.max(0, Math.min(1, level)) * 100)}%`;
    }

    // 线性重采样到 16kHz,跨块保留相位和上一块末尾采样,块边界不跳变。
    function makeResampler(sourceRate) {
      const ratio = sourceRate / 16000;
      let previous = 0;
      let phase = 0;
      return (input) => {
        if (input.length === 0) return new Float32Array(0);
        const output = [];
        let position = phase;
        while (position <= input.length - 1) {
          const index = Math.floor(position);
          const from = index < 0 ? previous : input[index];
          const to = input[Math.min(index + 1, input.length - 1)];
          output.push(from + (to - from) * (position - index));
          position += ratio;
        }
        phase = position - input.length;
        previous = input[input.length - 1];
        return Float32Array.from(output);
      };
    }

    function insertText(text) {
      const input = elements.composerInput;
      const start = input.selectionStart ?? input.value.length;
      const end = input.selectionEnd ?? input.value.length;
      const before = input.value.slice(0, start);
      const after = input.value.slice(end);
      const glue = before && !/\s$/.test(before) ? " " : "";
      input.value = `${before}${glue}${text}${after}`;
      const cursor = before.length + glue.length + text.length;
      input.setSelectionRange(cursor, cursor);
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.focus();
    }

    function startCapture(current) {
      const { context } = current;
      const source = context.createMediaStreamSource(current.stream);
      const processor = context.createScriptProcessor(4096, 1, 1);
      const resample = makeResampler(context.sampleRate);
      processor.onaudioprocess = (event) => {
        current.lastFrameAt = Date.now();
        if (current.socket.readyState !== WebSocket.OPEN) return;
        const samples = resample(event.inputBuffer.getChannelData(0));
        const pcm = new Int16Array(samples.length);
        let energy = 0;
        for (let i = 0; i < samples.length; i += 1) {
          const clamped = Math.max(-1, Math.min(1, samples[i]));
          pcm[i] = clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff;
          energy += clamped * clamped;
        }
        current.socket.send(pcm.buffer);
        setLevel(Math.sqrt(energy / Math.max(1, samples.length)) * 6);
      };
      source.connect(processor);
      processor.connect(context.destination);
      context.resume().catch(() => {});
      Object.assign(current, { source, processor, lastFrameAt: Date.now() });
      // 看门狗:音频帧断流 3 秒(标签页被挂起、AudioContext 没跑起来)就收,
      // 否则 daemon 那头收不到静音帧,10 秒静默自动结束永远不会触发。
      current.watchdog = setInterval(() => {
        if (Date.now() - current.lastFrameAt > 3000) stop(current, t("麦克风没有音频,听写已停止"));
      }, 1000);
    }

    function stop(current, notice) {
      if (session !== current) return;
      session = null;
      clearTimeout(current.timer);
      clearInterval(current.watchdog);
      try { current.processor?.disconnect(); current.source?.disconnect(); } catch { /* 已断 */ }
      current.stream.getTracks().forEach((track) => track.stop());
      current.context?.close().catch(() => {});
      current.socket.onclose = null;
      current.socket.onmessage = null;
      if (current.socket.readyState === WebSocket.OPEN) {
        try { current.socket.send("stop"); } catch { /* 对端已关 */ }
      }
      try { current.socket.close(); } catch { /* 已关 */ }
      button.classList.remove("is-recording");
      button.setAttribute("aria-pressed", "false");
      button.title = t("语音输入");
      setIndicator(false);
      if (notice) showToast(notice, "info");
    }

    async function start() {
      if (session) return;
      if (!navigator.mediaDevices?.getUserMedia) {
        showToast(t("这个页面拿不到麦克风(需要 https 或 localhost);可在终端 REPL 里用 /stt 听写"), "error");
        return;
      }
      const context = new (window.AudioContext || window.webkitAudioContext)();
      let stream;
      try {
        stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true } });
      } catch (error) {
        context.close().catch(() => {});
        showToast(t("麦克风不可用:{detail}", {detail: error?.message || error}), "error");
        return;
      }
      const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/voice/stream`;
      const socket = new WebSocket(url);
      socket.binaryType = "arraybuffer";
      const current = { socket, stream, context, source: null, processor: null, timer: null, watchdog: null, ready: false, lastFrameAt: 0 };
      session = current;
      button.classList.add("is-recording");
      button.setAttribute("aria-pressed", "true");
      button.title = t("点击结束听写(Esc 也可以)");
      socket.onmessage = (event) => {
        let message;
        try { message = JSON.parse(event.data); } catch { return; }
        if (message.type === "ready") {
          current.ready = true;
          startCapture(current);
          setIndicator(true);
        } else if (message.type === "dictation") {
          if (message.text) insertText(String(message.text));
        } else if (message.type === "ended") {
          stop(current, t("听写结束"));
        } else if (message.type === "error") {
          showToast(t("语音听写不可用:{detail}", {detail: message.message || t("未知错误")}), "error");
          stop(current, null);
        }
      };
      socket.onclose = () => {
        if (session !== current) return;
        if (!current.ready) showToast(t("语音听写连接失败"), "error");
        stop(current, current.ready ? t("听写结束") : null);
      };
      current.timer = setTimeout(() => stop(current, t("听写超时结束")), MAX_MS);
    }

    button.addEventListener("click", () => {
      if (session) stop(session, t("听写已停止"));
      else start();
    });
    elements.composerInput.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && session) {
        event.preventDefault();
        stop(session, t("听写已停止"));
      }
    });
  }

  function updateControlState() {
    syncRunIndicator();
    const running = conversationRunning();
    const busy = state.adminBusy || state.submitting;
    const locked = state.blocked || state.adminBusy || state.modeChooserOpen;
    const inputCount = countCharacters(elements.composerInput.value.trim());
    const attachmentUploading = state.composerAttachments.some((item) => item.status === "uploading");
    const attachmentError = state.composerAttachments.some((item) => item.status === "error");
    const attachmentReady = state.composerAttachments.some((item) => item.status === "ready");

    elements.composerInput.disabled = locked;
    elements.composerForm.classList.toggle("is-disabled", locked);
    elements.attachButton.disabled = locked || state.submitting || !state.capabilities?.attachments || state.composerAttachments.length >= MAX_ATTACHMENTS;
    elements.micButton.disabled = locked || state.submitting;
    elements.newChatButton.disabled = state.blocked || busy || state.sessionBusy || state.viewLoading;
    // 会话级模型覆盖允许在回复进行中调整，下一轮生效。
    elements.modelButton.disabled = state.blocked || state.models.length === 0;
    elements.promptGrid.querySelectorAll("button").forEach((button) => {
      button.disabled = state.blocked || running || busy;
    });
    updateModelMenuState();

    elements.sendButton.classList.remove("is-cancel");
    elements.sendButton.querySelector(".icon-slot").replaceChildren(createIcon("arrow-up"));
    elements.sendButton.title = running ? t("加入队列") : t("发送消息");
    elements.sendButton.setAttribute("aria-label", elements.sendButton.title);
    elements.sendButton.disabled = state.blocked || state.adminBusy || state.submitting || hasPendingQuestion()
      || (inputCount === 0 && !attachmentReady) || inputCount > MAX_CONTENT_CHARS || attachmentUploading || attachmentError;
    // 语音与发送合并成同一个位置(用户):miyu voice 可用、且没有输入、且不在排队/运行时
    // 显麦克风(点了走语音),否则显发送。voice 不可用就永远是发送。
    const hasDraft = inputCount > 0 || attachmentReady;
    const showMic = state.voiceEnabled === true && !hasDraft && !running && !state.submitting;
    elements.micButton.hidden = !showMic;
    elements.sendButton.hidden = showMic;
    document.querySelectorAll(".edit-action, .redo-action").forEach((button) => {
      button.disabled = !revisionEligible();
    });

    if (state.blocked) elements.composerState.textContent = t("未授权");
    // 被问问题时不再在输入框页脚重复「等待回答」——问题卡自己就写着,页脚这份多余
    // 且被模型芯片/速度挤成竖排(#3)。留空即可。
    else if (hasPendingQuestion()) elements.composerState.textContent = "";
    else if (attachmentUploading) elements.composerState.textContent = t("正在上传");
    else if (attachmentError) elements.composerState.textContent = t("附件上传失败");
    else if (busy) elements.composerState.textContent = state.submitting ? (running ? t("正在加入队列") : t("正在发送")) : t("正在处理");
    else if (inputCount > MAX_CONTENT_CHARS) elements.composerState.textContent = t("消息不能超过 20,000 个字符");
    else elements.composerState.textContent = "";
    elements.composerState.classList.toggle("is-error", inputCount > MAX_CONTENT_CHARS || attachmentError);
    updateSettingsControls();
  }

  function isNearBottom() {
    const distance = elements.chatScroll.scrollHeight - elements.chatScroll.scrollTop - elements.chatScroll.clientHeight;
    return distance <= NEAR_BOTTOM_PX;
  }

  function isAtBottom() {
    const distance = elements.chatScroll.scrollHeight - elements.chatScroll.scrollTop - elements.chatScroll.clientHeight;
    return distance <= 2;
  }

  function suspendOutputFollowing() {
    state.followOutput = false;
    elements.jumpBottomButton.hidden = false;
  }

  /// 同一帧里的多次滚动请求合并成一个 rAF。
  ///
  /// 以前每次调用都 `++scrollRequestId`,后一次会把前一次已排队的 rAF 作废;
  /// 流稳定时渲染回调与滚动回调挤在同一帧里,前一个请求被后一个作废、后一个
  /// 又被下一条 delta 作废,滚动被连续饿死,某一帧放过去就整段下跳。现在排队
  /// 的是「这一帧要不要滚」这件事本身,重复请求只是把 smooth 抬上去。
  let scrollFrame = 0;
  let scrollFrameSmooth = false;
  let scrollFrameForce = false;
  let programmaticScrollTimer = 0;
  // smooth 动画会连发多条 scroll 事件,守卫不能只吃第一条——否则第二条就被
  // 当成用户上滚,把「回到底部」的动画中途关掉跟随。
  let programmaticScrollSmooth = false;

  function scrollToBottom({ force = false, smooth = false } = {}) {
    if (!force && !state.followOutput) {
      elements.jumpBottomButton.hidden = false;
      return;
    }
    if (force) state.followOutput = true;
    scrollFrameSmooth = scrollFrameSmooth || smooth;
    // 已排队的非 force 请求不能把后来的 force 吞掉(点「回到底部」时若同一帧
    // 里正好有一条跟随请求在排队,它记的 force 是 false)。
    scrollFrameForce = scrollFrameForce || force;
    if (scrollFrame) return;
    scrollFrame = window.requestAnimationFrame(() => {
      const smoothNow = scrollFrameSmooth;
      const forceNow = scrollFrameForce;
      scrollFrame = 0;
      scrollFrameSmooth = false;
      scrollFrameForce = false;
      if (!forceNow && !state.followOutput) return;
      state.programmaticScroll = true;
      programmaticScrollSmooth = smoothNow;
      elements.chatScroll.scrollTo({ top: elements.chatScroll.scrollHeight, behavior: smoothNow ? "smooth" : "auto" });
      state.nearBottom = true;
      elements.jumpBottomButton.hidden = true;
      // 守卫由紧随其后的 scroll 事件解除。两种情况没有那条事件可吃:smooth
      // 期间事件被守卫吃掉、动画停下后没有下一条;auto 时视口本来就在底,
      // scrollTo 没动就不派发。两边都补兜底超时,只是长短不同。
      window.clearTimeout(programmaticScrollTimer);
      programmaticScrollTimer = window.setTimeout(() => {
        state.programmaticScroll = false;
        programmaticScrollSmooth = false;
      }, smoothNow ? PROGRAMMATIC_SCROLL_MS : PROGRAMMATIC_SCROLL_AUTO_MS);
    });
  }

  // anchor(可选):live 对象或 DOM 节点。离屏保活的 live(别会话)或已游离
  // 的节点长内容,不该滚动当前视图。
  function contentAdded(anchor) {
    if (anchor) {
      if (anchor.nodeType) {
        if (!anchor.isConnected) return;
      } else if (anchor.runId && !liveViewed(anchor)) return;
    }
    if (state.followOutput) scrollToBottom();
    else elements.jumpBottomButton.hidden = false;
  }

  async function copyText(text) {
    const value = String(text || "");
    if (!value) return false;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(value);
        showToast(t("已复制"));
        return true;
      }
    } catch (_) {
      // Use the selection fallback below.
    }
    const textarea = document.createElement("textarea");
    textarea.value = value;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.left = "-9999px";
    textarea.style.top = "0";
    document.body.appendChild(textarea);
    textarea.select();
    textarea.setSelectionRange(0, textarea.value.length);
    let copied = false;
    try {
      copied = document.execCommand("copy");
    } catch (_) {
      copied = false;
    }
    textarea.remove();
    showToast(copied ? t("已复制") : t("复制失败"), copied ? "info" : "error");
    return copied;
  }

  function makeCopyButton(textProvider, label = t("复制")) {
    const button = document.createElement("button");
    button.type = "button";
    button.title = label;
    button.setAttribute("aria-label", label);
    button.appendChild(makeIconSlot("copy"));
    button.addEventListener("click", () => copyText(typeof textProvider === "function" ? textProvider() : textProvider));
    return button;
  }

  function makeMessageAction(icon, label, handler) {
    const button = document.createElement("button");
    button.type = "button";
    button.title = label;
    button.setAttribute("aria-label", label);
    button.appendChild(makeIconSlot(icon));
    button.addEventListener("click", handler);
    return button;
  }

  function revisionEligible(candidate = state.redoCandidate) {
    if (!candidate || !state.capabilities?.redo) return false;
    // AI 输出中也允许改上一条 prompt(09-12 用户报):submitRedo 会先掐掉正在跑
    // 的那轮再重发,所以这里不再拿 conversationRunning() 挡着。
    return !state.blocked && !state.viewLoading && !state.resyncing
      && !state.submitting && !state.revisionSubmitting
      && !state.adminBusy && !state.sessionBusy && !hasPendingQuestion()
      && state.queuedPrompts.length === 0;
  }

  function closeRevisionEditor({ restoreFocus = false } = {}) {
    const editor = state.revisionEditor;
    if (!editor) return;
    editor.form.remove();
    editor.bubble.hidden = editor.wasHidden;
    state.revisionEditor = null;
    if (restoreFocus) editor.opener?.focus();
  }

  function openRevisionEditor(article, bubble, content, candidate, opener) {
    if (!revisionEligible(candidate)) return;
    closeRevisionEditor();
    const form = document.createElement("form");
    form.className = "revision-editor";
    form.setAttribute("aria-label", t("编辑最后一条消息"));
    const textarea = document.createElement("textarea");
    textarea.value = String(content || "");
    textarea.maxLength = MAX_CONTENT_CHARS;
    textarea.setAttribute("aria-label", t("消息内容"));
    const error = document.createElement("div");
    error.className = "revision-editor-error";
    error.setAttribute("role", "alert");
    error.hidden = true;
    const footer = document.createElement("div");
    footer.className = "revision-editor-footer";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.textContent = t("取消");
    const submit = document.createElement("button");
    submit.type = "submit";
    submit.textContent = t("发送");
    footer.append(cancel, submit);
    form.append(textarea, error, footer);
    const wasHidden = bubble.hidden;
    bubble.hidden = true;
    article.insertBefore(form, article.querySelector(".message-actions"));
    state.revisionEditor = { form, textarea, error, submit, bubble, wasHidden, opener, candidate };
    cancel.addEventListener("click", () => closeRevisionEditor({ restoreFocus: true }));
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const draft = textarea.value.trim();
      if (!draft && !article.querySelector(".user-attachments")) {
        error.textContent = t("消息不能为空");
        error.hidden = false;
        return;
      }
      if (countCharacters(draft) > MAX_CONTENT_CHARS) {
        error.textContent = t("消息不能超过 20,000 个字符");
        error.hidden = false;
        return;
      }
      await submitRedo(candidate, draft);
    });
    textarea.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeRevisionEditor({ restoreFocus: true });
      } else if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.isComposing) {
        event.preventDefault();
        form.requestSubmit();
      }
    });
    window.requestAnimationFrame(() => {
      textarea.focus();
      textarea.setSelectionRange(textarea.value.length, textarea.value.length);
      form.scrollIntoView({ block: "nearest" });
    });
  }

  async function submitRedo(candidate, editedContent = null) {
    if (!revisionEligible(candidate)) return;
    const sessionId = state.viewSessionId;
    if (!sessionId) return;
    state.revisionSubmitting = true;
    const editor = state.revisionEditor;
    if (editor) {
      editor.form.setAttribute("aria-busy", "true");
      editor.textarea.disabled = true;
      editor.submit.disabled = true;
      editor.error.hidden = true;
    }
    updateControlState();
    // AI 还在输出时改 prompt:先掐掉正在跑的那轮(redo 后端遇到 session_has_runs
    // 会 409),等它收尾再重发。最多等 ~4s,到点就交给下面的 409 重试兜底。
    if (conversationRunning()) {
      for (const live of [...state.liveRuns.values()]) {
        if (live && !live.ended) {
          try { await cancelLiveRun(live); } catch { /* 尽力而为 */ }
        }
      }
      for (let i = 0; i < 40 && conversationRunning(); i += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
    }
    try {
      const body = {
        expected_revision: candidate.revision,
        input_id: candidate.input_id
      };
      if (editedContent != null) body.content = editedContent;
      const response = await apiRequest(
        `/api/sessions/${encodeURIComponent(sessionId)}/turns/${encodeURIComponent(candidate.turn_id)}/redo`,
        { method: "POST", body: JSON.stringify(body) }
      );
      const payload = await response.json();
      const runId = String(payload?.run_id || "");
      if (!runId) throw new ApiError(t("服务未返回运行标识"), response.status);
      trackRun(sessionId, runId);
      createLiveForRun(runId, "", {
        claimTurn: false,
        operation: "redo",
        turnId: candidate.turn_id,
        inputId: candidate.input_id,
        editedContent
      });
      state.redoCandidate = null;
      renderSessionList();
      updateConversationChrome();
    } catch (error) {
      if (editor && state.revisionEditor === editor) {
        editor.error.textContent = error.status === 409 ? t("会话已变化，请重新操作") : error.message;
        editor.error.hidden = false;
      }
      showToast(error.status === 409 ? t("会话状态已更新") : error.message, "error");
      if (error.status === 409) await loadSessionView(sessionId, { quiet: true });
    } finally {
      state.revisionSubmitting = false;
      if (editor && state.revisionEditor === editor) {
        editor.form.removeAttribute("aria-busy");
        editor.textarea.disabled = false;
        editor.submit.disabled = false;
      }
      updateControlState();
    }
  }

  // 认得的协议。file:// 在里面是因为模型会用它指本地路径(MCP 服务器目录之类),
  // 以前不认,整条 [label](file://…) 就原样漏成 Markdown 源码。
  function validLinkUrl(value) {
    const raw = String(value || "").trim();
    if (!/^(?:https?|file):\/\//i.test(raw)) return null;
    try {
      const url = new URL(raw);
      return ["http:", "https:", "file:"].includes(url.protocol) ? url.href : null;
    } catch (_) {
      return null;
    }
  }

  function isFileUrl(href) {
    return /^file:/i.test(String(href || ""));
  }

  function filePathOf(href) {
    try {
      return decodeURIComponent(String(href).replace(/^file:\/\//i, "")) || String(href);
    } catch (_) {
      return String(href).replace(/^file:\/\//i, "");
    }
  }

  // 一个链接节点。file:// 单独一条路:浏览器从 http 页面导航到 file:// 会被安全
  // 策略**静默**拦下——做成真链接的话点了什么都不会发生,比不成链更让人困惑。
  // 所以本地路径改成「点一下把路径复制走」,hover 看完整路径。
  function createLink(href) {
    const link = document.createElement("a");
    link.href = href;
    link.rel = "noopener noreferrer";
    if (isFileUrl(href)) {
      const path = filePathOf(href);
      link.classList.add("path-link");
      link.title = path;
      link.addEventListener("click", (event) => {
        event.preventDefault();
        copyText(path);
      });
    } else {
      link.target = "_blank";
    }
    return link;
  }

  // 裸链接自动成链。模型经常直接把 URL 写进正文而不套 [](),以前这些只是纯文本,
  // 点不动。识别到句尾标点要吐回去:"见 https://a.com。" 里的句号不属于地址。
  const BARE_URL_TAIL = "。，、；：！？…～\"'`,.;:!?’”»›|*_~";
  const BARE_URL_PAIRS = { ")": "(", "]": "[", "}": "{", "》": "《", "」": "「", "』": "『", "】": "【" };

  function trimUrlTail(raw) {
    let value = raw;
    while (value.length) {
      const last = value[value.length - 1];
      const opener = BARE_URL_PAIRS[last];
      if (opener) {
        // 括号只在成对时留下:GitHub/维基的地址本身就带括号。
        const opens = value.split(opener).length - 1;
        const closes = value.split(last).length - 1;
        if (closes <= opens) break;
        value = value.slice(0, -1);
        continue;
      }
      if (BARE_URL_TAIL.includes(last)) {
        value = value.slice(0, -1);
        continue;
      }
      break;
    }
    return value;
  }

  function bareUrlAt(text, index) {
    // 前一个字符是字母数字时不认:避开 "xhttps://" 这类粘连。
    if (index > 0 && /[A-Za-z0-9]/.test(text[index - 1])) return null;
    // 中日韩标点一个都不能进地址。只在末尾修剪不够:「…archlinux.org、AUR」里
    // 顿号后面还跟着字母,末尾修剪碰不到它,整段会被 new URL() 当成域名的一部分
    // punycode 掉(实测变成 xn--orgaur-kr3e)。汉字本身不排除——维基那种带中文
    // 路径的地址是合法的。
    const matched = /^(?:https?|file):\/\/[^\s<>"'`\u00a0\u2000-\u206f\u3000-\u303f\uff00-\uffef]+/i.exec(
      text.slice(index)
    );
    if (!matched) return null;
    const raw = trimUrlTail(matched[0]);
    if (!raw) return null;
    const href = validLinkUrl(raw);
    return href ? { raw, href } : null;
  }

  function insideAnchor(node) {
    let cursor = node;
    while (cursor) {
      if (cursor.tagName === "A") return true;
      cursor = cursor.parentElement;
    }
    return false;
  }

  function appendAutoLink(parent, raw, href) {
    const link = createLink(href);
    link.classList.add("auto-link");
    link.textContent = raw;
    parent.appendChild(link);
  }

  // 「标题 (地址)」独占一行:模型给参考资料就是这么写的,标题是纯文本,于是以前
  // 只有括号里那半截像链接,读起来像标题和地址是两码事。整行命中时标题进同一个
  // <a>,点标题和点地址都能走。规则跟终端那边(src/render/link.rs)是同一套。
  const TITLE_URL_LINE = /^([ \t]*)(\S[^\n]*?)([ \t]*)([（(])[ \t]*((?:https?|file):\/\/[^\s)）]+)[ \t]*([)）])[ \t]*$/;

  function titleUrlLineAt(text, index) {
    const lineEnd = text.indexOf("\n", index);
    const line = lineEnd < 0 ? text.slice(index) : text.slice(index, lineEnd);
    const matched = TITLE_URL_LINE.exec(line);
    if (!matched) return null;
    const [, indent, title, gap, open, url, close] = matched;
    // 标题里再有地址就不是「标题 (地址)」;结尾是 ] 说明这其实是 [label](url),
    // 那条本来就有自己的分支——不拦住的话整条 Markdown 会被当成标题原样漏出来。
    if (title.includes("://") || title.endsWith("]")) return null;
    // 标题是一句话、不是一段话:句中有句号/问号/叹号/分号或长得离谱,就只让地址成链
    // (09-11 手机端实测一整段中文正文被下划线包成一个链接)。
    // 判据:中文句末标点直接算散文;英文只认「句点/问号/叹号 + 空格 + 大写或汉字」这种
    // 句界——"vs." 这类缩写后面跟小写,不算。
    if ([...title].length > 120 || /[。！？；]/.test(title) || /[.!?]\s+[A-Z\u4e00-\u9fff]/.test(title)) return null;
    const href = validLinkUrl(url);
    return href ? { length: line.length, indent, title, gap, open, url, close, href } : null;
  }

  function appendTitleUrlLine(parent, hit, appendTitle) {
    if (hit.indent) parent.appendChild(document.createTextNode(hit.indent));
    const link = createLink(hit.href);
    link.classList.add("auto-link", "title-link");
    const title = document.createElement("span");
    title.className = "link-title";
    appendTitle(title, hit.title);
    link.appendChild(title);
    link.appendChild(document.createTextNode(`${hit.gap}${hit.open}`));
    const address = document.createElement("span");
    address.className = "link-url";
    address.textContent = hit.url;
    link.appendChild(address);
    link.appendChild(document.createTextNode(hit.close));
    parent.appendChild(link);
  }

  // 无属性的 <br>/<br/>/<br />(大小写不限)是正文里唯一放行的 HTML 标签:表格单元格
  // 内换行只能靠它。带属性的 <br class=x>、以及任何别的标签继续当纯文本走,既不解码
  // &lt;br&gt; 再当标签认,也不给 innerHTML 开口子。粘性匹配,直接从游标处试。
  const BREAK_TAG = /<br[ \t]*\/?>/iy;

  function appendInline(parent, source, depth = 0) {
    const text = String(source || "");
    if (depth > 8) {
      parent.appendChild(document.createTextNode(text));
      return;
    }
    let index = 0;
    let plainStart = 0;
    const flushPlain = (end) => {
      if (end > plainStart) parent.appendChild(document.createTextNode(text.slice(plainStart, end)));
    };
    while (index < text.length) {
      if ((index === 0 || text[index - 1] === "\n") && !insideAnchor(parent)) {
        const titled = titleUrlLineAt(text, index);
        if (titled) {
          flushPlain(index);
          appendTitleUrlLine(parent, titled, (node, source) =>
            appendInline(node, source, depth + 1));
          index += titled.length;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "\\" && text[index + 1] === "(") {
        const end = text.indexOf("\\)", index + 2);
        if (end > index + 1) {
          flushPlain(index);
          renderMathInto(parent, text.slice(index + 2, end), false);
          index = end + 2;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "\\" && index + 1 < text.length && "\\`*_[]|~$".includes(text[index + 1])) {
        flushPlain(index);
        parent.appendChild(document.createTextNode(text[index + 1]));
        index += 2;
        plainStart = index;
        continue;
      }
      if (text[index] === "$") {
        if (text[index + 1] === "$") {
          const end = text.indexOf("$$", index + 2);
          if (end > index + 1) {
            flushPlain(index);
            renderMathInto(parent, text.slice(index + 2, end), false);
            index = end + 2;
            plainStart = index;
            continue;
          }
        } else {
          // 行内 $…$:内容非空、不跨行、两端非空格,右 $ 后不紧跟数字(避开价格写法)。
          const end = text.indexOf("$", index + 1);
          const inner = end > index ? text.slice(index + 1, end) : "";
          if (
            end > index + 1
            && inner.length
            && !inner.includes("\n")
            && !/^\s/.test(inner)
            && !/\s$/.test(inner)
            && !/^\d/.test(text.slice(end + 1))
          ) {
            flushPlain(index);
            renderMathInto(parent, inner, false);
            index = end + 1;
            plainStart = index;
            continue;
          }
        }
      }
      if (text[index] === "\n") {
        flushPlain(index);
        parent.appendChild(document.createElement("br"));
        index += 1;
        plainStart = index;
        continue;
      }
      if (text[index] === "`") {
        const end = text.indexOf("`", index + 1);
        if (end > index + 1) {
          flushPlain(index);
          const code = document.createElement("code");
          code.textContent = text.slice(index + 1, end);
          parent.appendChild(code);
          index = end + 1;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "[") {
        const labelEnd = text.indexOf("](", index + 1);
        const urlEnd = labelEnd >= 0 ? text.indexOf(")", labelEnd + 2) : -1;
        if (labelEnd > index + 1 && urlEnd > labelEnd + 2) {
          const href = validLinkUrl(text.slice(labelEnd + 2, urlEnd));
          if (href) {
            flushPlain(index);
            const link = createLink(href);
            appendInline(link, text.slice(index + 1, labelEnd), depth + 1);
            parent.appendChild(link);
            index = urlEnd + 1;
            plainStart = index;
            continue;
          }
        }
      }
      // <br> 排在 ` 之后:行内代码里的 <br> 仍是字面量。
      if (text[index] === "<") {
        BREAK_TAG.lastIndex = index;
        const br = BREAK_TAG.exec(text);
        if (br) {
          flushPlain(index);
          parent.appendChild(document.createElement("br"));
          index += br[0].length;
          plainStart = index;
          continue;
        }
      }
      // <https://…> 与裸链接。放在 ` 与 [](…) 之后:行内代码和 md 链接先被吃掉,
      // 这里看不到它们的内容。已经在 <a> 里(md 链接的标签)就不再套一层。
      if (text[index] === "<" && !insideAnchor(parent)) {
        const end = text.indexOf(">", index + 1);
        const href = end > index + 1 ? validLinkUrl(text.slice(index + 1, end)) : null;
        if (href) {
          flushPlain(index);
          appendAutoLink(parent, text.slice(index + 1, end), href);
          index = end + 1;
          plainStart = index;
          continue;
        }
      }
      if ("hHfF".includes(text[index]) && !insideAnchor(parent)) {
        const bare = bareUrlAt(text, index);
        if (bare) {
          flushPlain(index);
          appendAutoLink(parent, bare.raw, bare.href);
          index += bare.raw.length;
          plainStart = index;
          continue;
        }
      }
      if (text.startsWith("~~", index)) {
        const end = text.indexOf("~~", index + 2);
        if (end > index + 2 && text.slice(index + 2, end).trim()) {
          flushPlain(index);
          const deletion = document.createElement("del");
          appendInline(deletion, text.slice(index + 2, end), depth + 1);
          parent.appendChild(deletion);
          index = end + 2;
          plainStart = index;
          continue;
        }
      }
      const strongMarker = text.startsWith("**", index) ? "**" : text.startsWith("__", index) ? "__" : null;
      if (strongMarker && !(strongMarker === "__" && isWordChar(text[index - 1]))) {
        const end = strongMarker === "__" ? underscoreCloser(text, index + 2, "__") : text.indexOf(strongMarker, index + 2);
        if (end > index + 2 && text.slice(index + 2, end).trim()) {
          flushPlain(index);
          const strong = document.createElement("strong");
          appendInline(strong, text.slice(index + 2, end), depth + 1);
          parent.appendChild(strong);
          index = end + 2;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "*" || (text[index] === "_" && !isWordChar(text[index - 1]))) {
        const marker = text[index];
        const end = marker === "_" ? underscoreCloser(text, index + 1, "_") : text.indexOf(marker, index + 1);
        if (end > index + 1 && text.slice(index + 1, end).trim()) {
          flushPlain(index);
          const emphasis = document.createElement("em");
          appendInline(emphasis, text.slice(index + 1, end), depth + 1);
          parent.appendChild(emphasis);
          index = end + 1;
          plainStart = index;
          continue;
        }
      }
      index += 1;
    }
    flushPlain(text.length);
  }

  // 已经渲染过的图：同一段源码不重复往服务端跑（流式重绘会把同一块反复重建）。
  const mermaidCache = new Map();

  /** ```mermaid 卡片：图直接画在回复里，源码折在后面。
   *
   * 图在**服务端**渲染（`POST /api/mermaid`，与终端同一个 Rust 渲染器），
   * 前端不 vendor mermaid.js：终端那边本来就需要 Rust 渲染器，两边共用才不会
   * 出图不一致，也省掉 800KB 脚本。画不出来就退回普通代码块——和终端一个规矩。
   */
  function mermaidBlock(codeText) {
    const wrapper = document.createElement("div");
    wrapper.className = "code-block mermaid-block";
    const toolbar = document.createElement("div");
    toolbar.className = "code-toolbar";
    const label = document.createElement("span");
    label.textContent = t("图表");
    const sourceToggle = document.createElement("button");
    sourceToggle.type = "button";
    // 不蹭 `.code-copy-button`：那是给图标用的 25×23 定宽格子，塞中文会断成两行。
    sourceToggle.className = "mermaid-source-toggle";
    sourceToggle.textContent = t("源码");
    const copy = makeCopyButton(codeText, t("复制源码"));
    copy.className = "code-copy-button";
    // 两个按钮成一组靠右：工具栏是 space-between，散着放中间那个会飘到正中。
    const actions = document.createElement("div");
    actions.className = "mermaid-actions";
    actions.append(sourceToggle, copy);
    toolbar.append(label, actions);

    const figure = document.createElement("div");
    figure.className = "mermaid-figure";
    figure.textContent = t("正在画图…");

    const source = plainCodeBlock("mermaid", codeText);
    source.hidden = true;
    source.classList.add("mermaid-source");
    // 图和源码是同一块地方的两个视图，**互相替换**而不是源码追加在图下面
    // （用户 09-20）。按钮上写的是「点了会切到哪儿」。
    sourceToggle.addEventListener("click", () => {
      const showSource = source.hidden;
      source.hidden = !showSource;
      figure.hidden = showSource;
      sourceToggle.textContent = showSource ? t("图表") : t("源码");
      sourceToggle.classList.toggle("is-active", showSource);
      sourceToggle.setAttribute("aria-expanded", String(showSource));
    });

    wrapper.append(toolbar, figure, source);

    const paint = (svg) => {
      figure.textContent = "";
      // 服务端来的 SVG 当 HTML 插：它是我们自己的渲染器产出的，不是模型原文。
      figure.innerHTML = svg;
      const node = figure.querySelector("svg");
      if (!node) return;
      // `width`/`height` 留着：渲染器给的是图的**自然尺寸**，摘掉之后 SVG 会
      // 撑满卡片宽度——一张 200px 宽的小流程图被放大四倍，字大得离谱。
      // 只加一道上限，宽过卡片才等比缩。
      node.style.maxWidth = "100%";
      node.style.height = "auto";
      // 点图放大：走和图片一样的灯箱，SVG 包成 blob 给它。
      figure.classList.add("is-zoomable");
      figure.tabIndex = 0;
      figure.setAttribute("role", "button");
      figure.setAttribute("aria-label", t("放大查看图表"));
      const open = () => {
        const url = URL.createObjectURL(new Blob([svg], { type: "image/svg+xml" }));
        window.MiyuLightbox?.open({ url, name: t("图表") });
      };
      figure.addEventListener("click", open);
      // 卡片限了高（见 styles.css），高图会被缩着放。缩了就在工具栏说一句，
      // 否则「这张图其实不是原尺寸、点开能看全」没人知道。
      requestAnimationFrame(() => {
        const natural = parseFloat(node.getAttribute("height") || "0");
        if (natural > 0 && node.getBoundingClientRect().height < natural - 2) {
          wrapper.classList.add("is-scaled");
          label.textContent = t("图表 · 点开看原图");
        }
      });
      figure.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          open();
        }
      });
    };

    const fallback = () => {
      // 画不出来就只剩源码：把卡片整个换成那块普通代码块。
      //
      // `replaceWith` 对**还没插进 DOM** 的节点是静默无效的，所以这条路只留给
      // 异步失败（那时卡片已经在页面上了）；「缓存里已知画不出来」在下面直接
      // 返回代码块，不再造一个卡片出来再拆——那样会剩一个空壳裹着源码（09-20
      // 走查抓到）。
      source.hidden = false;
      source.classList.remove("mermaid-source");
      if (wrapper.parentNode) wrapper.replaceWith(source);
    };

    const cached = mermaidCache.get(codeText);
    if (cached === null) {
      return plainCodeBlock("mermaid", codeText);
    } else if (cached) {
      paint(cached);
    } else {
      apiRequest("/api/mermaid", { method: "POST", body: JSON.stringify({ source: codeText }) })
        .then((response) => response.json())
        .then((data) => {
          if (!data || typeof data.svg !== "string") throw new Error("no svg");
          mermaidCache.set(codeText, data.svg);
          paint(data.svg);
        })
        .catch(() => {
          mermaidCache.set(codeText, null);
          fallback();
        });
    }
    return wrapper;
  }

  function codeBlock(language, codeText, settled = true) {
    // 闭合的 ```mermaid 走图表卡片；还在流的时候先当代码块画，
    // 半截语法渲染出来的图只会闪。
    if (settled && String(language || "").trim().toLowerCase() === "mermaid" && codeText.trim()) {
      return mermaidBlock(codeText);
    }
    return plainCodeBlock(language, codeText, settled);
  }

  /** 带语法高亮的代码块本体。
   *
   * 和 `codeBlock` 分开，是因为 mermaid 卡片里折着的那份源码也要用它——走
   * `codeBlock` 的话会被那条 mermaid 分支再接回卡片，无限递归。
   */
  function plainCodeBlock(language, codeText, settled = true) {
    const wrapper = document.createElement("div");
    wrapper.className = "code-block";
    const toolbar = document.createElement("div");
    toolbar.className = "code-toolbar";
    const label = document.createElement("span");
    label.textContent = language || t("代码");
    const copy = makeCopyButton(codeText, t("复制代码"));
    copy.className = "code-copy-button";
    toolbar.append(label, copy);
    const pre = document.createElement("pre");
    const code = document.createElement("code");
    if (language) code.className = `language-${language}`;
    code.textContent = codeText;
    // 语法高亮。纯 DOM 上色,不认识的语言/分词出岔子一律保持这份纯文本
    // (见 highlight.js);settled=false 表示围栏还没闭合,这一轮先不上色。
    window.MiyuHighlight?.paint(code, language, codeText, settled);
    pre.appendChild(code);
    wrapper.append(toolbar, pre);
    return wrapper;
  }

  function parseTableRow(line) {
    const text = String(line || "").trim();
    const cells = [];
    let cell = "";
    let codeFenceLength = 0;
    let hasSeparator = false;
    let endedWithSeparator = false;
    for (let index = 0; index < text.length;) {
      if (text[index] === "\\" && index + 1 < text.length) {
        cell += text.slice(index, index + 2);
        index += 2;
        endedWithSeparator = false;
        continue;
      }
      if (text[index] === "`") {
        let end = index + 1;
        while (end < text.length && text[end] === "`") end += 1;
        const runLength = end - index;
        if (!codeFenceLength) codeFenceLength = runLength;
        else if (codeFenceLength === runLength) codeFenceLength = 0;
        cell += text.slice(index, end);
        index = end;
        endedWithSeparator = false;
        continue;
      }
      if (text[index] === "|" && !codeFenceLength) {
        cells.push(cell.trim());
        cell = "";
        hasSeparator = true;
        endedWithSeparator = true;
        index += 1;
        continue;
      }
      cell += text[index];
      endedWithSeparator = false;
      index += 1;
    }
    cells.push(cell.trim());
    if (text.startsWith("|")) cells.shift();
    if (endedWithSeparator) cells.pop();
    return { cells, hasSeparator };
  }

  function tableAlignments(line) {
    const row = parseTableRow(line);
    if (!row.hasSeparator || !row.cells.length) return null;
    const alignments = [];
    for (const cell of row.cells) {
      const marker = cell.match(/^(:)?-{3,}(:)?$/);
      if (!marker) return null;
      alignments.push(marker[1] && marker[2] ? "center" : marker[2] ? "right" : marker[1] ? "left" : "");
    }
    return alignments;
  }

  function isTableStart(lines, index) {
    if (index + 1 >= lines.length) return false;
    const header = parseTableRow(lines[index]);
    const alignments = tableAlignments(lines[index + 1]);
    return Boolean(alignments && header.hasSeparator && header.cells.length === alignments.length);
  }

  function isHorizontalRule(line) {
    const text = String(line || "").trim();
    return /^(?:\*\s*){3,}$/.test(text) || /^(?:-\s*){3,}$/.test(text) || /^(?:_\s*){3,}$/.test(text);
  }

  function markdownTable(lines, startIndex) {
    const headers = parseTableRow(lines[startIndex]).cells;
    const alignments = tableAlignments(lines[startIndex + 1]);
    const wrapper = document.createElement("div");
    wrapper.className = "markdown-table-scroll";
    const table = document.createElement("table");
    const head = document.createElement("thead");
    const headRow = document.createElement("tr");
    headers.forEach((content, column) => {
      const cell = document.createElement("th");
      cell.scope = "col";
      if (alignments[column]) cell.className = `align-${alignments[column]}`;
      appendInline(cell, content);
      headRow.appendChild(cell);
    });
    head.appendChild(headRow);
    table.appendChild(head);

    const body = document.createElement("tbody");
    let index = startIndex + 2;
    while (index < lines.length && lines[index].trim()) {
      const row = parseTableRow(lines[index]);
      if (!row.hasSeparator) break;
      const tableRow = document.createElement("tr");
      for (let column = 0; column < headers.length; column += 1) {
        const cell = document.createElement("td");
        if (alignments[column]) cell.className = `align-${alignments[column]}`;
        appendInline(cell, row.cells[column] || "");
        tableRow.appendChild(cell);
      }
      body.appendChild(tableRow);
      index += 1;
    }
    if (body.children.length) table.appendChild(body);
    wrapper.appendChild(table);
    return { node: wrapper, nextIndex: index };
  }

  function isMarkdownBlockStart(lines, index) {
    const line = lines[index];
    return /^\s*```/.test(line) || /^#{1,6}\s+/.test(line) || /^\s*[-*+]\s+/.test(line) || /^\s*\d+[.)]\s+/.test(line) || /^\s*>/.test(line) || isHorizontalRule(line) || isTableStart(lines, index) || /^\s*\$\$/.test(line) || /^\s*\\\[\s*$/.test(line) || Boolean(videoSourceFor(line));
  }

  /* ── 视频消息:整行只有一个视频 URL / 本地路径(或指向它的 markdown 链接)
     时升级为播放器。本地文件经 /api/media 流式端点(带 HTTP Range)。 ── */
  const VIDEO_SOURCE_PATTERN = /\.(mp4|m4v|webm|mov|mkv|ogv)(\?[^\s)]*)?$/i;
  function videoSourceFor(rawLine) {
    const trimmed = String(rawLine || "").trim();
    if (!trimmed || trimmed.length > 2048) return null;
    const link = trimmed.match(/^\[([^\]]*)\]\(([^)\s]+)\)$/);
    const target = link ? link[2] : trimmed;
    if (/\s/.test(target) || !VIDEO_SOURCE_PATTERN.test(target)) return null;
    if (/^https?:\/\//i.test(target)) {
      return { src: target, label: link?.[1] || target };
    }
    if (target.startsWith("/") || target.startsWith("~/")) {
      return {
        src: `/api/media?path=${encodeURIComponent(target)}`,
        label: link?.[1] || target.split("/").pop() || target,
      };
    }
    return null;
  }

  function videoNode(source) {
    const card = document.createElement("div");
    card.className = "video-card";
    const shell = document.createElement("div");
    shell.className = "video-shell";
    const video = document.createElement("video");
    video.controls = true;
    video.preload = "metadata";
    video.playsInline = true;
    video.src = source.src;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "vfs-btn";
    button.title = t("网页全屏");
    button.setAttribute("aria-label", t("网页全屏"));
    button.innerHTML =
      '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3"/></svg>';
    button.addEventListener("click", () => shell.classList.toggle("webfs"));
    shell.append(video, button);
    const caption = document.createElement("div");
    caption.className = "video-caption";
    caption.textContent = source.label;
    card.append(shell, caption);
    return card;
  }

  /* ── LaTeX 公式(KaTeX):块级 $$…$$ / \[…\],行内 $…$ / \(…\)。
     katex 未就绪或语法错误时原样降级;流式期间未闭合的定界符保持原文,
     闭合后的下一次重渲染自动升级成公式。 */
  function renderMathInto(parent, tex, displayMode) {
    const trimmed = tex.trim();
    if (trimmed && window.katex && typeof window.katex.render === "function") {
      const node = document.createElement(displayMode ? "div" : "span");
      node.className = displayMode ? "math-display" : "math-inline";
      try {
        window.katex.render(trimmed, node, { displayMode, throwOnError: false, strict: "ignore" });
        parent.appendChild(node);
        return;
      } catch (_) { /* 落到原样文本 */ }
    }
    parent.appendChild(document.createTextNode(displayMode ? `$$${tex}$$` : `$${tex}$`));
  }

  function matchMathBlock(lines, index) {
    const trimmed = lines[index].trim();
    for (const [open, close] of [["$$", "$$"], ["\\[", "\\]"]]) {
      if (!trimmed.startsWith(open)) continue;
      const rest = trimmed.slice(open.length);
      if (rest.length > close.length && rest.endsWith(close)) {
        return { tex: rest.slice(0, rest.length - close.length), nextIndex: index + 1 };
      }
      const body = rest && rest !== close ? [rest] : [];
      let cursor = index + 1;
      while (cursor < lines.length) {
        const candidate = lines[cursor].trim();
        if (candidate === close || candidate.endsWith(close)) {
          if (candidate !== close) body.push(candidate.slice(0, candidate.length - close.length));
          return { tex: body.join("\n"), nextIndex: cursor + 1 };
        }
        body.push(lines[cursor]);
        cursor += 1;
      }
      return null; // 未闭合:保持原文(流式中)
    }
    return null;
  }

  // 字母、数字、下划线算词内字符:下划线强调两头都不能挨着它们(CommonMark 的 intraword 规则)
  function isWordChar(ch) {
    return Boolean(ch) && /[\p{L}\p{N}_]/u.test(ch);
  }

  // 找下划线强调的闭合位:闭合的 _ 后面不能紧跟词内字符,否则继续往后找
  function underscoreCloser(text, from, marker) {
    let end = text.indexOf(marker, from);
    while (end !== -1) {
      if (!isWordChar(text[end + marker.length])) return end;
      end = text.indexOf(marker, end + 1);
    }
    return -1;
  }

  function renderMarkdown(container, source) {
    const lines = String(source || "").replace(/\r\n?/g, "\n").split("\n");
    const fragment = document.createDocumentFragment();
    let index = 0;
    while (index < lines.length) {
      const line = lines[index];
      if (!line.trim()) {
        index += 1;
        continue;
      }
      const fence = line.match(/^\s*```\s*([\w.+-]*)\s*$/);
      if (fence) {
        const codeLines = [];
        index += 1;
        while (index < lines.length && !/^\s*```\s*$/.test(lines[index])) {
          codeLines.push(lines[index]);
          index += 1;
        }
        // 收尾围栏还没到 = 这块代码正流式写着,内容随时会变,先不上色。
        const closed = index < lines.length;
        if (closed) index += 1;
        const language = /^[\w.+-]{1,40}$/.test(fence[1] || "") ? fence[1] : "";
        fragment.appendChild(codeBlock(language, codeLines.join("\n"), closed));
        continue;
      }
      const video = videoSourceFor(line);
      if (video) {
        fragment.appendChild(videoNode(video));
        index += 1;
        continue;
      }
      if (/^\s*(\$\$|\\\[)/.test(line)) {
        const math = matchMathBlock(lines, index);
        if (math) {
          const wrapper = document.createElement("div");
          wrapper.className = "math-block";
          renderMathInto(wrapper, math.tex, true);
          fragment.appendChild(wrapper);
          index = math.nextIndex;
          continue;
        }
      }
      if (isTableStart(lines, index)) {
        const rendered = markdownTable(lines, index);
        fragment.appendChild(rendered.node);
        index = rendered.nextIndex;
        continue;
      }
      if (isHorizontalRule(line)) {
        fragment.appendChild(document.createElement("hr"));
        index += 1;
        continue;
      }
      const heading = line.match(/^(#{1,6})\s+(.+)$/);
      if (heading) {
        const level = Math.min(6, heading[1].length + 1);
        const node = document.createElement(`h${level}`);
        appendInline(node, heading[2]);
        fragment.appendChild(node);
        index += 1;
        continue;
      }
      const unordered = line.match(/^\s*[-*+]\s+(.+)$/);
      if (unordered) {
        const list = document.createElement("ul");
        let hasTask = false;
        while (index < lines.length) {
          const itemMatch = lines[index].match(/^\s*[-*+]\s+(.+)$/);
          if (!itemMatch) break;
          const item = document.createElement("li");
          const task = itemMatch[1].match(/^\[([ xX])\]\s+(.*)$/);
          if (task) {
            hasTask = true;
            item.className = "task-list-item";
            const checkbox = document.createElement("input");
            checkbox.type = "checkbox";
            checkbox.checked = task[1].toLowerCase() === "x";
            checkbox.disabled = true;
            const content = document.createElement("span");
            appendInline(content, task[2]);
            item.append(checkbox, content);
          } else {
            appendInline(item, itemMatch[1]);
          }
          list.appendChild(item);
          index += 1;
        }
        if (hasTask) list.classList.add("task-list");
        fragment.appendChild(list);
        continue;
      }
      const ordered = line.match(/^\s*\d+[.)]\s+(.+)$/);
      if (ordered) {
        const list = document.createElement("ol");
        while (index < lines.length) {
          const itemMatch = lines[index].match(/^\s*\d+[.)]\s+(.+)$/);
          if (!itemMatch) break;
          const item = document.createElement("li");
          appendInline(item, itemMatch[1]);
          list.appendChild(item);
          index += 1;
        }
        fragment.appendChild(list);
        continue;
      }
      if (/^\s*>/.test(line)) {
        const quoteLines = [];
        while (index < lines.length) {
          const quote = lines[index].match(/^\s*>\s?(.*)$/);
          if (!quote) break;
          quoteLines.push(quote[1]);
          index += 1;
        }
        const blockquote = document.createElement("blockquote");
        appendInline(blockquote, quoteLines.join("\n"));
        fragment.appendChild(blockquote);
        continue;
      }
      const paragraphLines = [line];
      index += 1;
      while (index < lines.length && lines[index].trim() && !isMarkdownBlockStart(lines, index)) {
        paragraphLines.push(lines[index]);
        index += 1;
      }
      const paragraph = document.createElement("p");
      appendInline(paragraph, paragraphLines.join("\n"));
      fragment.appendChild(paragraph);
    }
    container.replaceChildren(fragment);
    // 独占一行的链接升级成卡片。这里只是排队:流式期间每来一段都会重渲染,
    // 真正的抓取要等最后一次渲染安顿下来(见 linkcards.js 的防抖)。
    window.MiyuLinkCards?.scan(container);
    // 没闭合的围栏这一轮空着,等这块正文不再变了再补上色(同样是防抖)。
    window.MiyuHighlight?.settle(container);
  }

  /// daemon 自己合成的轮，不是任何人敲的：后台任务唤醒、目标续轮。
  ///
  /// 判据收口在这里——原来两处各写一遍前缀列表，加一种合成轮就得记得改两个
  /// 地方，漏一个的表现是「时间线里画成用户气泡、但滚动到底又不算用户消息」。
  function isSyntheticTurnContent(raw) {
    const text = String(raw || "");
    return text.startsWith("[后台任务完成]") // i18n-allow: 后端写进消息体的合成轮前缀,按原文比对,不是界面文案
      || text.startsWith("[后台命令完成]") // i18n-allow: 后端写进消息体的合成轮前缀,按原文比对,不是界面文案
      || text.startsWith("[目标续轮]") // i18n-allow: 后端写进消息体的合成轮前缀,按原文比对,不是界面文案
      || text.startsWith("<background-job-report>")
      || text.startsWith("<goal_round>")
      || text.startsWith("<cross-session-message")
      || text.startsWith("<service-restart");
  }

  /// daemon 重启后接着跑的那一轮（09-24）：第几次续跑；不是续跑消息给 0。
  function serviceRestartAttempt(raw) {
    const match = /^<service-restart attempt="(\d+)"[^>]*>/.exec(String(raw || "").trimStart());
    return match ? Number(match[1]) : 0;
  }

  /// `createUserMessage` 对目标续轮返回 null（那一轮在时间线里不画）。
  /// 每个调用点各写一遍判空太容易漏，统一走这里。
  function appendUserMessage(parent, content, timestamp, attributes = {}) {
    const node = createUserMessage(content, timestamp, attributes);
    if (node) parent.appendChild(node);
    return node;
  }

  /**
   * 自己发出去的消息:只渲染代码块、行内代码和链接,别的一律原样。
   *
   * 不做完整 markdown 是有意的(09-09 用户拍板)。把 `*星号*` 变成斜体、`# 井号`
   * 变成标题,等于把人原样打进去的字改掉了——而她收到的仍是原文,两边对不上。
   * 代码块没有这个问题:``` 围栏本来就是「这段原样看」的意思;链接同理,地址
   * 文字一个字都不变,只是变成可点的。
   */
  function renderUserText(container, source) {
    const text = String(source || "");
    const lines = text.split("\n");
    const fragment = document.createDocumentFragment();
    let buffer = [];
    const flushText = () => {
      if (!buffer.length) return;
      const chunk = buffer.join("\n");
      buffer = [];
      // 围栏之间的空行不值得单独占一段。
      if (!chunk.trim()) return;
      const paragraph = document.createElement("p");
      appendUserInline(paragraph, chunk);
      fragment.appendChild(paragraph);
    };
    let index = 0;
    while (index < lines.length) {
      const fence = lines[index].match(/^\s*```\s*([\w.+-]*)\s*$/);
      if (!fence) {
        buffer.push(lines[index]);
        index += 1;
        continue;
      }
      flushText();
      index += 1;
      const body = [];
      while (index < lines.length && !/^\s*```\s*$/.test(lines[index])) {
        body.push(lines[index]);
        index += 1;
      }
      // 收尾围栏可能没打,那也照样当代码块渲染——半截的围栏更该原样看。
      index += 1;
      fragment.appendChild(codeBlock(fence[1] || "", body.join("\n")));
    }
    flushText();
    container.replaceChildren(fragment);
  }

  /** 行内:反引号、<url>、裸地址,其余原样。 */
  function appendUserInline(parent, source) {
    const text = String(source || "");
    let index = 0;
    let plainStart = 0;
    const flushPlain = (end) => {
      if (end > plainStart) parent.appendChild(document.createTextNode(text.slice(plainStart, end)));
    };
    while (index < text.length) {
      if (index === 0 || text[index - 1] === "\n") {
        const titled = titleUrlLineAt(text, index);
        if (titled) {
          flushPlain(index);
          appendTitleUrlLine(parent, titled, appendUserInline);
          index += titled.length;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "\n") {
        flushPlain(index);
        parent.appendChild(document.createElement("br"));
        index += 1;
        plainStart = index;
        continue;
      }
      if (text[index] === "`") {
        const end = text.indexOf("`", index + 1);
        if (end > index + 1) {
          flushPlain(index);
          const code = document.createElement("code");
          code.textContent = text.slice(index + 1, end);
          parent.appendChild(code);
          index = end + 1;
          plainStart = index;
          continue;
        }
      }
      if (text[index] === "<") {
        const end = text.indexOf(">", index + 1);
        const href = end > index + 1 ? validLinkUrl(text.slice(index + 1, end)) : null;
        if (href) {
          flushPlain(index);
          appendAutoLink(parent, text.slice(index + 1, end), href);
          index = end + 1;
          plainStart = index;
          continue;
        }
      }
      if ("hHfF".includes(text[index])) {
        const bare = bareUrlAt(text, index);
        if (bare) {
          flushPlain(index);
          appendAutoLink(parent, bare.raw, bare.href);
          index += bare.raw.length;
          plainStart = index;
          continue;
        }
      }
      index += 1;
    }
    flushPlain(text.length);
  }

  function createUserMessage(content, timestamp, attributes = {}) {
    // 系统自动触发的后台任务跟进不是真实用户输入，渲染为居中系统事件而不是用户气泡。
    const rawContent = String(content || "");
    // 目标续轮在时间线里什么都不画：输入框上方的状态行已经在说「进行中 ·
    // 第 N 轮」，对话流里每轮再来一条居中提示只是噪声，几十轮下来会把真正
    // 的内容淹掉。AI 的输出照常显示。
    if (rawContent.startsWith("[目标续轮]") || rawContent.startsWith("<goal_round>")) { // i18n-allow: 后端协议前缀,按原文比对
      return null;
    }
    // 目标变更通知走的是排队消息管线（步间送达、随回合持久化），但它是一次
    // 操作的回执，不是用户说的话——画成居中提示而不是用户气泡。
    if (rawContent.startsWith("[目标已变更] ")) { // i18n-allow: 后端协议前缀(含尾空格),按原文比对
      const notice = document.createElement("div");
      notice.className = "system-event is-command-result";
      if (attributes.turnId) notice.dataset.turnId = attributes.turnId;
      const label = document.createElement("span");
      label.textContent = t("目标已变更：{detail}", {detail: rawContent.slice("[目标已变更] ".length)}); // i18n-allow: slice 掉的是后端协议前缀,不是文案
      label.title = formatDateTime(timestamp);
      notice.appendChild(label);
      return notice;
    }
    // 子代理会话的第一轮是主会话派的任务(会话项目第 4 段):和终端一样画成「来自主会话
    // 的任务」那一块,露几行、点开看全文,不是用户气泡。
    if (attributes.fromParent && window.MiyuCrossSession) {
      return window.MiyuCrossSession.createReceived({ body: rawContent }, {
        headline: t("来自主会话的任务"),
        icon: "arrow-down",
        timestamp,
        turnId: attributes.turnId
      });
    }
    // 另一个会话里的 AI 发来的那条(09-23):铃铛 + 「从 xxx 收到消息」,底下露正文。
    // 实时插进来的、唤醒起的一轮、刷新回看都走这里。
    const crossSession = window.MiyuCrossSession?.parse(rawContent);
    if (crossSession) {
      return window.MiyuCrossSession.createReceived(crossSession, {
        timestamp,
        turnId: attributes.turnId,
        followupId: attributes.followupId
      });
    }
    // daemon 重启后接着跑的那一轮(09-24):一行提示,不画成用户气泡。
    const restartAttempt = serviceRestartAttempt(rawContent);
    if (restartAttempt) {
      const notice = document.createElement("div");
      notice.className = "system-event is-restart-resume";
      if (attributes.turnId) notice.dataset.turnId = attributes.turnId;
      const label = document.createElement("span");
      const headline = restartAttempt > 1
        ? t("又重启了，接着上一轮继续（第 {n} 次）", {n: restartAttempt})
        : t("重启了，接着上一轮继续");
      label.textContent = `↻ ${headline}`;
      label.title = formatDateTime(timestamp);
      notice.appendChild(label);
      return notice;
    }
    if (isSyntheticTurnContent(rawContent)) {
      const notice = document.createElement("div");
      notice.className = "system-event";
      if (attributes.turnId) notice.dataset.turnId = attributes.turnId;
      const label = document.createElement("span");
      let labelText = "";
      if (rawContent.startsWith("[后台任务完成]")) { // i18n-allow: 后端协议前缀,按原文比对
        labelText = rawContent.replace(/^\[后台任务完成\]\s*/, "");
      } else if (rawContent.startsWith("[后台命令完成]")) { // i18n-allow: 后端协议前缀,按原文比对
        const stripped = rawContent.replace(/^\[后台命令完成\]\s*/, "");
        labelText = t("命令完成 {summary}", {summary: stripped.split(" · ").slice(0, 2).join(" · ")});
      } else {
        const inner = (rawContent.match(/「(.*?)」/)?.[1] || "").trim();
        labelText = inner ? t("任务完成 {name}", {name: inner}) : t("后台任务完成");
      }
      label.textContent = `⚙ ${labelText}`;
      label.title = rawContent;
      label.title = formatDateTime(timestamp);
      notice.appendChild(label);
      return notice;
    }
    const article = document.createElement("article");
    article.className = "message user-message";
    article.dataset.role = "user";
    if (attributes.turnId) article.dataset.turnId = attributes.turnId;
    if (attributes.runId) article.dataset.runId = attributes.runId;
    if (attributes.followupId) article.dataset.followupId = attributes.followupId;
    if (attributes.inputId) article.dataset.inputId = attributes.inputId;
    const bubble = document.createElement("div");
    bubble.className = "user-bubble";
    const textContent = String(content || "");
    renderUserText(bubble, textContent);
    bubble.hidden = !textContent.trim();
    const attachments = createUserAttachments(attributes.attachments);
    if (attributes.queued) {
      // 排队的消息:直接画在对话末尾,像一条已经发出去的,只是左边挂一枚「排队中」小签
      // 和一个撤下按钮。轮到它时 consumeLiveQueue 会画真的那条,这条随之撤掉。
      article.classList.add("is-queued");
      article.dataset.queueId = String(attributes.queueId || "");
      const badge = document.createElement("span");
      badge.className = "queue-badge";
      const label = document.createElement("span");
      label.textContent = t("排队中");
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "queue-remove";
      remove.title = t("撤回这条排队消息");
      remove.setAttribute("aria-label", t("撤回这条排队消息"));
      remove.appendChild(makeIconSlot("undo-2"));
      remove.addEventListener("click", () => removeQueuedPrompt(attributes.queueId));
      badge.append(label, remove);
      if (attachments) article.appendChild(attachments);
      article.append(badge, bubble);
      return article;
    }
    const actions = document.createElement("div");
    actions.className = "message-actions";
    if (attributes.revisionTarget) {
      const edit = makeMessageAction("square-pen", t("编辑最后一条消息"), () => {
        openRevisionEditor(article, bubble, textContent, attributes.revisionTarget, edit);
      });
      edit.className = "edit-action";
      actions.appendChild(edit);
    }
    if (textContent.trim()) actions.appendChild(makeCopyButton(textContent, t("复制消息")));
    if (attachments) article.appendChild(attachments);
    article.append(bubble, actions);
    return article;
  }

  /**
   * 附件芯片的图标。全都画成 file-text 的话，一段视频和一份 md 长得一模一样,
   * 扫一眼分不出哪个是哪个(09-09 用户实拍)。按 MIME 优先、拿不到再看扩展名。
   */
  const ATTACHMENT_EXTENSION_ICONS = {
    md: "file-markdown", markdown: "file-markdown",
    json: "file-json", jsonc: "file-json",
    pdf: "file-pdf",
    zip: "file-archive", tar: "file-archive", gz: "file-archive", xz: "file-archive",
    zst: "file-archive", "7z": "file-archive", rar: "file-archive",
    js: "file-code", mjs: "file-code", ts: "file-code", tsx: "file-code", jsx: "file-code",
    py: "file-code", rs: "file-code", go: "file-code", c: "file-code", h: "file-code",
    cpp: "file-code", hpp: "file-code", java: "file-code", rb: "file-code", php: "file-code",
    sh: "file-code", bash: "file-code", zsh: "file-code", fish: "file-code", lua: "file-code",
    toml: "file-code", yaml: "file-code", yml: "file-code", ini: "file-code", css: "file-code",
    html: "file-code", xml: "file-code", sql: "file-code", nix: "file-code",
  };

  function attachmentIconName(attachment) {
    const mime = String(attachment?.mime || "").toLowerCase();
    if (attachment?.kind === "image" || mime.startsWith("image/")) return "image";
    if (mime.startsWith("video/")) return "file-video";
    if (mime.startsWith("audio/")) return "file-audio";
    if (mime === "application/pdf") return "file-pdf";
    const extension = String(attachment?.name || "").split(".").pop()?.toLowerCase() || "";
    return ATTACHMENT_EXTENSION_ICONS[extension] || "file-text";
  }

  function createUserAttachments(values) {
    const attachments = Array.isArray(values) ? values : [];
    if (!attachments.length) return null;
    const list = document.createElement("div");
    list.className = "user-attachments";
    for (const attachment of attachments) {
      const url = safeAttachmentUrl(attachment?.url);
      if (!url) continue;
      const name = String(attachment?.name || t("附件"));
      if (attachment?.kind === "image" || String(attachment?.mime || "").startsWith("image/")) {
        const link = document.createElement("a");
        link.className = "user-attachment-image";
        link.href = url;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
        link.title = name;
        // 会话里的图点开是放大预览，自己发的图没道理反而是「跳走一个新标签
        // 页」。按住 Ctrl/⌘ 或中键仍然走链接原本的行为。
        link.addEventListener("click", (event) => {
          if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
          if (!window.MiyuLightbox) return;
          event.preventDefault();
          window.MiyuLightbox.open({ url, name });
        });
        const image = document.createElement("img");
        image.src = url;
        image.alt = name;
        image.loading = "lazy";
        image.decoding = "async";
        const width = validAssetDimension(attachment?.width);
        const height = validAssetDimension(attachment?.height);
        if (width) image.width = width;
        if (height) image.height = height;
        link.appendChild(image);
        list.appendChild(link);
        continue;
      }
      // 能预览的芯片：整块是「看看是什么」，右边箭头单独负责下载。不能预览的
      // 二进制维持原样，整块就是下载链接。
      const previewable = Boolean(window.MiyuPreview?.canPreview(attachment));
      const chip = document.createElement(previewable ? "div" : "a");
      chip.className = "user-attachment-file";
      if (previewable) {
        chip.classList.add("is-previewable");
        chip.tabIndex = 0;
        chip.setAttribute("role", "button");
        chip.title = t("预览 {name}", {name: name});
        const openPreview = () => window.MiyuPreview.open({ ...attachment, url, name });
        chip.addEventListener("click", openPreview);
        chip.addEventListener("keydown", (event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          openPreview();
        });
      } else {
        chip.href = url;
        chip.setAttribute("download", "");
        chip.title = t("下载 {name}", {name: name});
      }
      chip.appendChild(makeIconSlot(attachmentIconName(attachment)));
      const copy = document.createElement("span");
      const strong = document.createElement("strong");
      strong.textContent = name;
      const small = document.createElement("small");
      small.textContent = formatFileSize(attachment?.size);
      copy.append(strong, small);
      chip.appendChild(copy);
      if (previewable) {
        const download = document.createElement("a");
        download.className = "user-attachment-download";
        download.href = url;
        download.setAttribute("download", "");
        download.title = t("下载 {name}", {name: name});
        download.setAttribute("aria-label", t("下载 {name}", {name: name}));
        download.addEventListener("click", (event) => event.stopPropagation());
        download.appendChild(makeIconSlot("download"));
        chip.appendChild(download);
      } else {
        chip.appendChild(makeIconSlot("download"));
      }
      list.appendChild(chip);
    }
    return list.childElementCount ? list : null;
  }

  function safeAssetUrl(value) {
    const raw = String(value || "").trim();
    if (!raw) return null;
    try {
      const url = new URL(raw, window.location.origin);
      if (url.origin !== window.location.origin || !url.pathname.startsWith("/api/assets/") || url.pathname === "/api/assets/") return null;
      return url.href;
    } catch (_) {
      return null;
    }
  }

  function safeArtifactUrl(value) {
    const raw = String(value || "").trim();
    if (!raw) return null;
    try {
      const url = new URL(raw, window.location.origin);
      const allowed = ["/api/assets/", "/api/artifacts/"].some((prefix) => url.pathname.startsWith(prefix) && url.pathname !== prefix);
      return url.origin === window.location.origin && allowed ? url.href : null;
    } catch (_) {
      return null;
    }
  }

  function artifactName(source) {
    return String(source?.name || source?.alt || t("预览资源")).trim() || t("预览资源");
  }

  function normalizeArtifact(source, fallbackKind = "file") {
    if (!source || typeof source !== "object") return null;
    const url = safeArtifactUrl(source.url);
    if (!url) return null;
    const mime = String(source.mime || "application/octet-stream").toLowerCase();
    return {
      ...source,
      id: String(source.id || url),
      url,
      name: artifactName(source),
      type_label: String(source.type_label || "").trim().toUpperCase(),
      mime,
      kind: String(source.kind || (mime.startsWith("image/") ? "image" : fallbackKind))
    };
  }

  function artifactSupportsPreview(artifact) {
    // svg 的 mime 是 image/svg+xml,靠下面这条命中图片通道——`<img>` 里的 SVG
    // 浏览器强制禁脚本禁外链,既安全又白捡了缩放平移。
    return artifact?.kind === "image"
      || artifact?.mime?.startsWith("image/")
      || ["markdown", "html", "pdf", "csv"].includes(artifact?.kind);
  }

  function artifactSupportsSource(artifact) {
    // svg 是图片也是文本,两个视图都要给:光能看不能读,改起来无从下手。
    return ["markdown", "html", "text", "code", "json", "csv", "svg"].includes(artifact?.kind)
      || artifact?.mime?.startsWith("text/")
      || artifact?.mime?.startsWith("application/json");
  }

  function defaultArtifactMode(artifact) {
    return artifactSupportsPreview(artifact) ? "preview" : "source";
  }

  function artifactWidthPixels() {
    const viewportWidth = Math.max(320, layoutViewportWidth());
    return Math.min(viewportWidth - 20, Math.max(320, viewportWidth * state.artifactWidthRatio));
  }

  function syncComposerDockHeight() {
    // 非分栏的桌面浮层态里,artifact 面板是绝对定位、bottom 贴到 10px,会盖住输入框
    // 页脚(#2)。把页脚实际高度喂给 CSS,浮层的 bottom 就停在页脚上方、页脚照常可用。
    const height = elements.composerDock?.offsetHeight || 0;
    if (height) elements.mainStage.style.setProperty("--composer-dock-height", `${Math.round(height)}px`);
    // 开面板当下量的是旧布局的页脚高度(面板一开正文列变窄、页脚里模型芯片会换行
    // 变高),reflow 之后再量一次才对——否则「刚开盖住、跑一轮才正常」(#2 用户实测)。
    window.requestAnimationFrame(() => {
      const settled = elements.composerDock?.offsetHeight || 0;
      if (settled) elements.mainStage.style.setProperty("--composer-dock-height", `${Math.round(settled)}px`);
    });
  }

  function syncArtifactLayout() {
    const width = artifactWidthPixels();
    elements.mainStage.style.setProperty("--artifact-width", `${Math.round(width)}px`);
    syncComposerDockHeight();
    const roomForConversation = elements.mainStage.clientWidth - width - 10;
    const split = state.artifactOpen && !state.artifactMaximized && layoutViewportWidth() > 760 && roomForConversation >= 320;
    elements.mainStage.classList.toggle("artifact-split", split);
    elements.mainStage.classList.toggle("artifact-maximized", state.artifactOpen && state.artifactMaximized);
    // 常驻任务面板的宽度闸。原先靠 .main-stage 上的容器查询,而容器查询容器会
    // 让 WebKit 在后代 replaceChildren 时归零 scrollTop(见 styles.css 注释),
    // 改成这里挂类,量的是同一个宽度。
    elements.mainStage.classList.toggle("is-wide", elements.mainStage.clientWidth >= STAGE_WIDE_PX);
    syncSidebarSpace();
  }

  function closeArtifactResourceMenu() {
    elements.artifactResourceMenu.hidden = true;
    elements.artifactTitleButton.setAttribute("aria-expanded", "false");
  }

  function setArtifactWorkspaceOpen(open) {
    const hasArtifacts = state.artifacts.length > 0;
    state.artifactOpen = Boolean(open && hasArtifacts);
    if (!state.artifactOpen) state.artifactMaximized = false;
    elements.artifactWorkspace.hidden = !state.artifactOpen;
    elements.artifactWorkspace.setAttribute("aria-hidden", String(!state.artifactOpen));
    elements.mainStage.classList.toggle("artifact-open", state.artifactOpen);
    closeArtifactResourceMenu();
    syncArtifactLayout();
    elements.artifactToggleButton.setAttribute("aria-pressed", String(state.artifactOpen));
    if (state.artifactOpen) {
      elements.artifactToggleButton.classList.remove("has-new-artifact");
      renderArtifactWorkspace();
    }
  }

  /// artifact 的归属会话。预览面板永远只画当前正在看的那个会话。
  function artifactScope() {
    return String(state.viewSessionId || state.currentSessionId || "");
  }

  function pinnedArtifactsForScope() {
    const scope = artifactScope();
    let pinned = state.pinnedArtifacts.get(scope);
    if (!pinned) {
      pinned = new Map();
      state.pinnedArtifacts.set(scope, pinned);
    }
    return pinned;
  }

  function dismissedArtifactsForScope() {
    const scope = artifactScope();
    let dismissed = state.dismissedArtifactIds.get(scope);
    if (!dismissed) {
      dismissed = new Set();
      state.dismissedArtifactIds.set(scope, dismissed);
    }
    return dismissed;
  }

  function registerArtifact(source, { autoOpen = false } = {}) {
    const artifact = normalizeArtifact(source, source?.kind || "file");
    if (!artifact) return;
    pinnedArtifactsForScope().set(artifact.id, artifact);
    dismissedArtifactsForScope().delete(artifact.id);
    const index = state.artifacts.findIndex((item) => item.id === artifact.id);
    if (index >= 0) state.artifacts[index] = artifact;
    else state.artifacts.push(artifact);
    state.artifactSourceCache.delete(artifact.id);
    state.selectedArtifactId = artifact.id;
    state.artifactMode = defaultArtifactMode(artifact);
    state.artifactZoom = 1;
    state.artifactPanX = 0;
    state.artifactPanY = 0;
    elements.artifactToggleButton.hidden = false;
    if (autoOpen && layoutViewportWidth() > 760) setArtifactWorkspaceOpen(true);
    else if (!state.artifactOpen) elements.artifactToggleButton.classList.add("has-new-artifact");
    if (state.artifactOpen) renderArtifactWorkspace();
  }

  /// 常驻任务面板：当前会话的待办。
  ///
  /// 两条更新路径。进会话/刷新走 `GET /api/sessions/{id}/todos`——工具事件
  /// 只在 `todowrite` 跑的那一刻发生一次,不问一次就只有空面板；回合里 AI
  /// 改了待办则直接吃 `tool.finished` 的输出,不必再往返一趟。
  function renderStageTodos(todos) {
    state.stageTodos = todos?.length ? todos : null;
    const panel = elements.stageTodos;
    panel.replaceChildren();
    const card = state.stageTodos ? window.MiyuTodos?.renderList(state.stageTodos) : null;
    if (!card) {
      panel.hidden = true;
      return;
    }
    panel.appendChild(card);
    panel.hidden = false;
  }

  const GOAL_PHASE_LABELS = Object.freeze({
    active: t("进行中"),
    paused: t("已暂停"),
    blocked: t("受阻"),
    complete: t("已完成"),
  });

  /// 目标状态行。
  ///
  /// 目标是会话级的长期状态，不该只在对话流里闪一条消息就没了——那条消息会
  /// 被后面几十轮顶到看不见的地方。贴在输入框上方，随状态刷新，能直接操作。
  function renderGoalBar() {
    const bar = elements.goalBar;
    bar.replaceChildren();
    const goal = state.goal;
    // 完成的目标不再占位：那一行的作用是「它还在做这件事」，做完了就该让开。
    // 想回顾结果，AI 的结案陈词就在对话流里。
    if (!goal || goal.phase === "complete") {
      bar.hidden = true;
      return;
    }
    bar.hidden = false;
    bar.dataset.phase = String(goal.phase || "");

    const mark = document.createElement("span");
    mark.className = "goal-bar-mark";
    mark.appendChild(makeIconSlot("target"));

    // 一行装下：目标 + 状态。轮数上限不显示——256 是防跑飞的兜底，不是进度
    // 条的分母，写出来只会让人以为要跑 256 轮。
    const objective = document.createElement("strong");
    objective.className = "goal-bar-objective";
    objective.textContent = String(goal.objective || "");
    objective.title = t("点击修改目标");
    objective.tabIndex = 0;
    objective.setAttribute("role", "button");
    const startEdit = () => beginGoalEdit(objective, goal);
    objective.addEventListener("click", startEdit);
    objective.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        startEdit();
      }
    });

    const meta = document.createElement("small");
    meta.className = "goal-bar-meta";
    // active 但没武装 = 目标还在、只是不会自己往前跑了（被打断过或重启过）。
    const phase = goal.phase === "active" && !goal.armed
      ? t("已停下")
      : GOAL_PHASE_LABELS[goal.phase] || goal.phase;
    meta.textContent = t("{phase} · 第 {round} 轮", {phase: phase, round: goal.rounds_started});
    if (goal.blocked_message) meta.title = goal.blocked_message;

    const actions = document.createElement("span");
    actions.className = "goal-bar-actions";
    // 按钮跟着阶段变：暂停的目标不该还挂着「暂停」。
    // 编辑排在最前：点文字也能改，但一个明确的按钮才看得出「这行可以改」。
    const edit = document.createElement("button");
    edit.type = "button";
    edit.className = "goal-bar-button";
    edit.title = t("修改目标");
    edit.setAttribute("aria-label", t("修改目标"));
    edit.append(makeIconSlot("square-pen"));
    edit.addEventListener("click", startEdit);
    actions.appendChild(edit);
    const buttons = goal.phase === "active" && goal.armed
      ? [["pause", t("暂停"), "pause"], ["clear", t("清除"), "x"]]
      : [["resume", t("继续"), "play"], ["clear", t("清除"), "x"]];
    for (const [action, label, icon] of buttons) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "goal-bar-button";
      button.title = label;
      button.setAttribute("aria-label", label);
      button.append(makeIconSlot(icon));
      button.addEventListener("click", () => runGoalAction(action));
      actions.appendChild(button);
    }
    bar.append(mark, objective, meta, actions);
  }

  /// 就地改目标：点一下文字变输入框，回车提交，Esc 放弃。
  function beginGoalEdit(node, goal) {
    // 多行文本框(09-12 用户报单行不好写不好看):自动撑高,回车提交、
    // Shift+回车换行、Esc 放弃。
    const input = document.createElement("textarea");
    input.className = "goal-bar-edit";
    input.rows = 1;
    input.value = String(goal.objective || "");
    input.setAttribute("aria-label", t("修改目标"));
    const autosize = () => {
      input.style.height = "auto";
      input.style.height = `${Math.min(input.scrollHeight, 220)}px`;
    };
    // `finish` 会被回车和失焦各触发一次——提交时把输入框换掉，那一下又会
    // 触发 blur。没有这个闸就会连发两次 edit。
    let settled = false;
    const finish = (commit) => {
      if (settled) return;
      settled = true;
      const next = input.value.trim();
      if (commit && next && next !== goal.objective) runGoalAction(`edit ${next}`);
      else renderGoalBar();
    };
    input.addEventListener("keydown", (event) => {
      event.stopPropagation();
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        finish(true);
      } else if (event.key === "Escape") {
        event.preventDefault();
        finish(false);
      }
    });
    input.addEventListener("input", autosize);
    input.addEventListener("blur", () => finish(true));
    node.replaceWith(input);
    input.focus();
    input.select();
    autosize();
  }

  async function runGoalAction(action) {
    try {
      const response = await apiRequest("/api/goal", {
        method: "POST",
        body: JSON.stringify({ session_id: state.viewSessionId, input: action }),
      });
      // 服务端把「拒绝」也当成一次成功的命令执行（HTTP 200 + 一段说明文字），
      // 所以不能只看 HTTP 状态——不弹出来的话，改目标失败时状态行只是悄悄
      // 变回原样，看着像点了没反应。
      const text = String((await response.json())?.text || "");
      if (/^(用法|\/goal |本会话)/.test(text)) showToast(text.split("\n")[0], "error");
      // edit 命中正在跑的续轮时，daemon 会掐掉旧轮、按新目标重开一轮——
      // 中断和新气泡就是时间线上的反馈，这里只补一个轻量确认。
      else if (action.startsWith("edit ")) showToast(t("目标已变更：{detail}", {detail: text.split("\n")[1] || ""}));
    } catch (error) {
      showToast(error?.message || t("目标操作失败"), "error");
    }
    loadGoal(state.viewSessionId);
  }

  /// `/pop`（无参数）的轮次多选器：列出可弹出的轮次（最旧在前，与按数量
  /// 弹出同一口径），勾选后按 turn_ids 弹出。
  async function openPopPicker() {
    const sessionId = state.viewSessionId;
    if (!sessionId) return;
    let turns = [];
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/poppable`);
      turns = (await response.json())?.turns || [];
    } catch (error) {
      showToast(error.message || t("读取可弹出轮次失败"), "error");
      return;
    }
    const list = elements.popDialogList;
    list.replaceChildren();
    elements.popDialogAll.checked = false;
    if (!turns.length) {
      const empty = document.createElement("div");
      empty.className = "pop-dialog-empty";
      empty.textContent = t("当前上下文没有可弹出的轮次");
      list.appendChild(empty);
    }
    const boxes = [];
    for (const turn of turns) {
      const row = document.createElement("label");
      row.className = "pop-dialog-row";
      const box = document.createElement("input");
      box.type = "checkbox";
      box.value = String(turn?.turn_id || "");
      const preview = document.createElement("span");
      preview.className = "pop-row-preview";
      preview.textContent = String(turn?.preview || "").trim() || t("（空消息）");
      const meta = document.createElement("span");
      meta.className = "pop-row-meta";
      const tokens = asFiniteNumber(turn?.tokens);
      meta.textContent = [formatTime(turn?.timestamp), tokens ? formatTokens(tokens) : ""]
        .filter(Boolean)
        .join(" · ");
      row.append(box, preview, meta);
      list.appendChild(row);
      boxes.push(box);
    }
    const refresh = () => {
      const selected = boxes.filter((box) => box.checked).length;
      elements.popConfirmButton.disabled = selected === 0;
      elements.popConfirmButton.textContent = selected ? t("弹出所选（{count}）", {count: selected}) : t("弹出所选");
      elements.popDialogAll.checked = boxes.length > 0 && selected === boxes.length;
    };
    // onchange 直接赋值而不是 addEventListener：每次打开都重建列表，
    // 累加监听器会让旧闭包一直陪跑。
    boxes.forEach((box) => { box.onchange = refresh; });
    elements.popDialogAll.onchange = () => {
      boxes.forEach((box) => { box.checked = elements.popDialogAll.checked; });
      refresh();
    };
    elements.popConfirmButton.onclick = async () => {
      const turnIds = boxes.filter((box) => box.checked).map((box) => box.value);
      if (!turnIds.length) return;
      elements.popConfirmButton.disabled = true;
      try {
        const response = await apiRequest("/api/conversation/pop", {
          method: "POST",
          body: JSON.stringify({ session_id: sessionId, turn_ids: turnIds }),
        });
        const removed = (await response.json())?.result?.turns || 0;
        elements.popDialog.close();
        await loadSessionView(sessionId, { quiet: true });
        showToast(t("已从上下文弹出 {count} 轮", {count: removed}));
      } catch (error) {
        elements.popConfirmButton.disabled = false;
        showToast(error.message || t("弹出失败"), "error");
      }
    };
    refresh();
    if (typeof elements.popDialog.showModal === "function") elements.popDialog.showModal();
    else elements.popDialog.setAttribute("open", "");
  }

  // 命令回执的锚点回合。优先锚到正在流式输出的那一轮：它落盘后 id 不变，
  // 回执就一直钉在它后面；只认「最后一个已落盘回合」的话，运行中敲的命令
  // 会因为这一轮还没落盘而没有锚点，被顶到时间线最前面。
  function commandAnchorTurnId() {
    const live = [...state.liveRuns.values()].find((entry) => entry && !entry.ended && entry.turnId);
    if (live) return String(live.turnId);
    return state.turns.length ? String(state.turns[state.turns.length - 1]?.id || "") : "";
  }

  async function refreshSessionContext(sessionId) {
    const scope = String(sessionId || "");
    if (!scope) return;
    const generation = (state.contextGeneration = (state.contextGeneration || 0) + 1);
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(scope)}/context`);
      const payload = await response.json();
      // 用户可能在响应回来之前又切走了：旧响应不许覆盖新会话的数字。
      if (generation !== state.contextGeneration || state.viewSessionId !== scope) return;
      state.context.tokens = Math.max(0, asFiniteNumber(payload?.context_tokens));
      state.context.window = payload?.context_window == null
        ? null
        : Math.max(0, asFiniteNumber(payload.context_window));
      // 「累计」按这条会话的权威值（含子代理花销）起算：基线只升不降，换会话时已清零。
      const cumulative = Math.max(0, asFiniteNumber(payload?.cumulative_tokens));
      state.context.cumulative_tokens = cumulative;
      state.context.cumulative_prompt_tokens = Math.max(0, asFiniteNumber(payload?.cumulative_prompt_tokens));
      state.context.cumulative_cache_read_tokens = Math.max(0, asFiniteNumber(payload?.cumulative_cache_read_tokens));
      if (cumulative > 0 && !(asFiniteNumber(state.cumulativeBase?.total) >= cumulative)) {
        state.cumulativeBase = {
          total: cumulative,
          prompt: state.context.cumulative_prompt_tokens,
          cached: state.context.cumulative_cache_read_tokens,
        };
      }
      updateContext();
      refreshComposerCumulative();
    } catch (_) {
      // 拉不到就保持现状，等 run 事件里的增量。
    }
  }

  async function loadGoal(sessionId) {
    const scope = String(sessionId || "");
    if (!scope) {
      state.goal = null;
      renderGoalBar();
      return;
    }
    const generation = ++state.goalGeneration;
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(scope)}/goal`);
      const payload = await response.json();
      if (generation !== state.goalGeneration) return;
      state.goal = payload?.goal || null;
    } catch (_) {
      if (generation !== state.goalGeneration) return;
      state.goal = null;
    }
    renderGoalBar();
  }

  async function loadStageTodos(sessionId) {
    const scope = String(sessionId || "");
    if (!scope) {
      renderStageTodos(null);
      return;
    }
    const generation = ++state.stageTodosGeneration;
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(scope)}/todos`);
      const payload = await response.json();
      if (generation !== state.stageTodosGeneration) return;
      renderStageTodos(window.MiyuTodos?.normalize(payload?.todos) || null);
    } catch (_) {
      // 面板是附带信息,拿不到就空着,不打扰对话。
      if (generation === state.stageTodosGeneration) renderStageTodos(null);
    }
  }

  /// 新版 todowrite 输出不含清单本体,实时卡片与舞台面板改从会话 API 取。
  /// 拿不到就静默放弃——面板是附带信息,不打扰对话。
  async function attachLiveTodoPanel(tool, live, sameSession) {
    const scope = runSessionId(live.runId);
    if (!scope) return;
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(scope)}/todos`);
      const payload = await response.json();
      const todos = window.MiyuTodos?.normalize(payload?.todos) || null;
      if (sameSession) renderStageTodos(todos);
      const panel = todos ? window.MiyuTodos.renderList(todos) : null;
      tool.card.querySelector(".todo-panel")?.remove();
      if (panel) tool.card.appendChild(panel);
    } catch (_) {}
  }

  function syncArtifactsFromTurns(turns) {
    let artifacts = [];
    for (const turn of turns) {
      // 只收真正的 artifact。`turn.assets` 是对话里内联显示的图片（打印/生成
      // 的图），它们已经在气泡里画出来了，再塞进 artifact 面板等于同一张图占
      // 两个位置，还会把面板自动切到图片上、盖住用户正在看的东西。
      // 要把图当 artifact 展示，走 present_artifact/create_artifact —— 那条
      // 路产出的就是 turn.artifacts。
      for (const source of Array.isArray(turn?.artifacts) ? turn.artifacts : []) {
        const artifact = normalizeArtifact(source, "file");
        if (artifact && !artifacts.some((item) => item.id === artifact.id)) artifacts.push(artifact);
      }
    }
    // 手动送进来的补在后面：它们不属于任何回合，只活在这份 state 里。
    for (const artifact of pinnedArtifactsForScope().values()) {
      if (!artifacts.some((item) => item.id === artifact.id)) artifacts.push(artifact);
    }
    const dismissed = dismissedArtifactsForScope();
    state.artifacts = artifacts.filter((item) => !dismissed.has(item.id));
    artifacts = state.artifacts;
    if (!artifacts.some((item) => item.id === state.selectedArtifactId)) {
      state.selectedArtifactId = artifacts.at(-1)?.id || null;
      state.artifactMode = defaultArtifactMode(artifacts.at(-1));
    }
    const knownIds = new Set(artifacts.map((artifact) => artifact.id));
    for (const id of state.artifactSourceCache.keys()) {
      if (!knownIds.has(id)) state.artifactSourceCache.delete(id);
    }
    elements.artifactToggleButton.hidden = artifacts.length === 0;
    if (!artifacts.length) setArtifactWorkspaceOpen(false);
    else if (state.artifactOpen) renderArtifactWorkspace();
    else if (window.location.hash.includes("artifact")) {
      // 深链 #artifact:载入后自动展开预览工作区(与 #console 同一约定)。
      window.location.hash = "";
      setArtifactWorkspaceOpen(true);
    }
  }

  function artifactIconName(artifact) {
    if (artifact?.kind === "image" || artifact?.mime?.startsWith("image/")) return "image";
    if (artifact?.kind === "markdown") return "file-markdown";
    if (artifact?.kind === "json") return "file-json";
    if (artifact?.kind === "code" || artifact?.kind === "html") return "file-code";
    if (artifact?.kind === "csv") return "layout-grid";
    return "file-text";
  }

  function artifactTypeLabel(artifact) {
    if (artifact?.type_label) return artifact.type_label;
    if (artifact?.kind === "markdown") return "MD";
    if (artifact?.kind === "json") return "JSON";
    if (artifact?.kind === "html") return "HTML";
    if (artifact?.kind === "code") return "CODE";
    if (artifact?.kind === "pdf") return "PDF";
    if (artifact?.kind === "image") return String(artifact.mime || "IMAGE").split("/").pop().toUpperCase();
    return "FILE";
  }

  function renderArtifactImage(artifact) {
    const stage = document.createElement("div");
    stage.className = "artifact-image-stage";
    const image = document.createElement("img");
    image.src = artifact.url;
    image.alt = artifact.name;
    const applyTransform = () => {
      image.style.transform = `translate(${state.artifactPanX}px, ${state.artifactPanY}px) scale(${state.artifactZoom})`;
      stage.classList.toggle("is-zoomed", state.artifactZoom > 1);
    };
    applyTransform();
    stage.addEventListener("wheel", (event) => {
      event.preventDefault();
      const nextZoom = Math.min(4, Math.max(0.25, state.artifactZoom * (event.deltaY < 0 ? 1.12 : 0.89)));
      state.artifactZoom = nextZoom;
      if (nextZoom <= 1) {
        state.artifactZoom = 1;
        state.artifactPanX = 0;
        state.artifactPanY = 0;
      }
      applyTransform();
      updateArtifactImageControls();
    }, { passive: false });
    stage.addEventListener("pointerdown", (event) => {
      if (state.artifactZoom <= 1 || event.button !== 0) return;
      event.preventDefault();
      stage.classList.add("is-dragging");
      stage.setPointerCapture(event.pointerId);
      stage.dataset.panStartX = String(event.clientX);
      stage.dataset.panStartY = String(event.clientY);
      stage.dataset.panOriginX = String(state.artifactPanX);
      stage.dataset.panOriginY = String(state.artifactPanY);
    });
    stage.addEventListener("pointermove", (event) => {
      if (!stage.classList.contains("is-dragging")) return;
      state.artifactPanX = Number(stage.dataset.panOriginX)
        + visualPixelsToLayout(event.clientX - Number(stage.dataset.panStartX));
      state.artifactPanY = Number(stage.dataset.panOriginY)
        + visualPixelsToLayout(event.clientY - Number(stage.dataset.panStartY));
      applyTransform();
    });
    const finishPan = () => stage.classList.remove("is-dragging");
    stage.addEventListener("pointerup", finishPan);
    stage.addEventListener("pointercancel", finishPan);
    stage.appendChild(image);
    return stage;
  }

  function updateArtifactImageControls() {
    const isImage = state.artifacts.find((item) => item.id === state.selectedArtifactId)?.kind === "image";
    if (!isImage) return;
    elements.artifactImageZoomOutButton.disabled = state.artifactZoom <= 0.25;
    elements.artifactImageZoomInButton.disabled = state.artifactZoom >= 4;
  }

  async function loadArtifactSource(artifact) {
    const version = `${artifact.url}|${artifact.updated_at || ""}`;
    const cached = state.artifactSourceCache.get(artifact.id);
    if (cached?.version === version) return cached.text;
    const response = await fetch(artifact.url, { credentials: "same-origin", cache: "no-store" });
    if (!response.ok) throw new Error(t("文件载入失败"));
    const text = await response.text();
    state.artifactSourceCache.set(artifact.id, { version, text });
    return text;
  }

  function artifactLoadingNode() {
    const loading = document.createElement("div");
    loading.className = "artifact-loading";
    loading.append(makeIconSlot("loader-circle", "is-spinning"));
    return loading;
  }

  function renderArtifactFailure(error, token) {
    if (token !== state.artifactRenderToken) return;
    const failure = document.createElement("div");
    failure.className = "artifact-failure";
    failure.append(makeIconSlot("circle-alert"), document.createTextNode(error?.message || t("文件载入失败")));
    elements.artifactView.replaceChildren(failure);
  }

  /**
   * 源码视图的高亮语言。Prism 只打包了那十来门（拼装顺序写在
   * `web/vendor/prism/prism.min.js` 头部），认不出来的传空字符串，
   * `paint` 会原样留纯文本——正文缺一块颜色无所谓，缺一个字不行。
   */
  const ARTIFACT_SOURCE_LANGUAGES = {
    html: "markup", htm: "markup", xml: "markup", svg: "markup",
    css: "css", scss: "css",
    js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "javascript",
    ts: "typescript", tsx: "typescript",
    json: "json", jsonl: "json",
    md: "markdown", markdown: "markdown",
    rs: "rust", py: "python", go: "go", lua: "lua", sql: "sql",
    c: "c", h: "c", cpp: "cpp", cc: "cpp", hpp: "cpp",
    sh: "bash", bash: "bash", zsh: "bash", fish: "bash",
    toml: "toml", yaml: "yaml", yml: "yaml", diff: "diff"
  };

  function artifactSourceLanguage(artifact) {
    const name = String(artifact?.name || "");
    const extension = name.includes(".") ? name.split(".").pop().toLowerCase() : "";
    return ARTIFACT_SOURCE_LANGUAGES[extension] || "";
  }

  /** 表格最多画这么多行。再多浏览器就卡了,剩下的让她去看源码或下载。 */
  const MAX_TABLE_ROWS = 2000;

  /**
   * 拆 CSV/TSV。只认最基本的那套规矩：双引号包住的字段里分隔符和换行都算正文，
   * 连着两个双引号是一个字面量引号。够读她导出的表了，不做各家方言兼容。
   */
  function parseDelimited(text, delimiter) {
    const rows = [];
    let row = [];
    let field = "";
    let quoted = false;
    for (let index = 0; index < text.length; index += 1) {
      const char = text[index];
      if (quoted) {
        if (char !== '"') { field += char; continue; }
        if (text[index + 1] === '"') { field += '"'; index += 1; continue; }
        quoted = false;
        continue;
      }
      if (char === '"') { quoted = true; continue; }
      if (char === delimiter) { row.push(field); field = ""; continue; }
      if (char === "\r") continue;
      if (char === "\n") { row.push(field); rows.push(row); row = []; field = ""; continue; }
      field += char;
    }
    // 最后一行没有换行收尾也要算,否则整张表少一行。
    if (field.length > 0 || row.length > 0) { row.push(field); rows.push(row); }
    return rows;
  }

  /**
   * CSV/TSV 画成表格。**纯 DOM，一个 HTML 字符串都不产生**——面板里这份内容
   * 同样出自模型之手，和聊天正文一个待遇（理由见 highlight.js 头注释）。
   * 外壳复用 markdown-body，表格样式就不用再写一套。
   */
  function buildArtifactTable(artifact, text) {
    const tabbed = /\.tsv$/i.test(artifact.name) || artifact.mime.includes("tab-separated");
    const rows = parseDelimited(text, tabbed ? "\t" : ",").filter(
      (row) => row.length > 1 || (row[0] || "").trim() !== ""
    );
    const article = document.createElement("article");
    article.className = "markdown-body artifact-markdown";
    if (rows.length === 0) {
      const note = document.createElement("p");
      note.textContent = t("这份表是空的。");
      article.appendChild(note);
      return article;
    }
    const clipped = rows.length > MAX_TABLE_ROWS + 1;
    const body = rows.slice(1, clipped ? MAX_TABLE_ROWS + 1 : rows.length);
    const columns = rows.reduce((most, row) => Math.max(most, row.length), 0);
    const table = document.createElement("table");
    const head = document.createElement("thead");
    const headRow = document.createElement("tr");
    for (let index = 0; index < columns; index += 1) {
      const cell = document.createElement("th");
      cell.textContent = rows[0][index] ?? "";
      headRow.appendChild(cell);
    }
    head.appendChild(headRow);
    const tbody = document.createElement("tbody");
    for (const row of body) {
      const line = document.createElement("tr");
      for (let index = 0; index < columns; index += 1) {
        const cell = document.createElement("td");
        cell.textContent = row[index] ?? "";
        line.appendChild(cell);
      }
      tbody.appendChild(line);
    }
    table.append(head, tbody);
    article.appendChild(table);
    if (clipped) {
      const note = document.createElement("p");
      note.className = "artifact-table-note";
      note.textContent = t("表太长，只画了前 {shown} 行，共 {total} 行。完整内容看源码或下载。", {shown: MAX_TABLE_ROWS, total: rows.length - 1});
      article.appendChild(note);
    }
    return article;
  }

  async function renderArtifactSource(artifact, token) {
    let text = await loadArtifactSource(artifact);
    if (token !== state.artifactRenderToken) return;
    if (artifact.kind === "json" || artifact.mime.startsWith("application/json") || /\.json$/i.test(artifact.name)) {
      try { text = JSON.stringify(JSON.parse(text), null, 2); } catch (_) {}
    }
    const source = document.createElement("div");
    source.className = "artifact-source";
    const gutter = document.createElement("div");
    gutter.className = "artifact-line-numbers";
    const lines = text.split("\n");
    for (let index = 0; index < lines.length; index += 1) {
      const number = document.createElement("span");
      number.textContent = String(index + 1);
      gutter.appendChild(number);
    }
    const pre = document.createElement("pre");
    pre.className = "artifact-code";
    const code = document.createElement("code");
    code.textContent = text;
    // 聊天正文里的代码块一直有高亮,这边却是一片纯白——同一个组件接上就是了。
    // 这份内容已经完整(不是流式),所以 settled=true,当场上色。
    window.MiyuHighlight?.paint(code, artifactSourceLanguage(artifact), text, true);
    pre.appendChild(code);
    source.append(gutter, pre);
    elements.artifactView.replaceChildren(source);
  }

  async function renderArtifactPreview(artifact, token) {
    if (artifact.kind === "image" || artifact.mime.startsWith("image/")) {
      elements.artifactView.replaceChildren(renderArtifactImage(artifact));
      return;
    }
    if (artifact.kind === "pdf") {
      const frame = document.createElement("iframe");
      frame.className = "artifact-frame";
      frame.src = artifact.url;
      frame.title = artifact.name;
      elements.artifactView.replaceChildren(frame);
      return;
    }
    if (artifact.kind === "html") {
      const frame = document.createElement("iframe");
      frame.className = "artifact-frame";
      frame.src = artifact.url;
      frame.title = artifact.name;
      /*
       * 她写的页面要能动——图表、按钮、切换,不放开脚本这些全是死的。放开的同时
       * 靠这两样把它关在箱子里:
       *   · 不给 allow-same-origin：iframe 拿不透明源,cookie / localStorage /
       *     父页面 DOM 一律 SecurityError。别家(Claude、ChatGPT、LibreChat)给了
       *     same-origin,所以不得不再买个独立域名来隔离 cookie;我们不给,也就
       *     不需要独立域。代价是 artifact 里存不住状态,刷新即归零。
       *   · 不给 allow-popups / allow-forms / allow-top-navigation：这三个各自是
       *     一条外带通道(window.open、表单提交、top.location),**CSP 管不了,
       *     只有 sandbox 管得了**。LibreChat 的 CVE-2026-54025 就死在第三条上。
       * 出站那一半由后端的 CSP 掐(见 assets.rs 的 artifact_csp)。两道各管一半:
       * sandbox 管权限,CSP 管外泄。
       */
      frame.setAttribute("sandbox", "allow-scripts allow-modals");
      elements.artifactView.replaceChildren(frame);
      return;
    }
    if (artifact.kind === "csv") {
      const text = await loadArtifactSource(artifact);
      if (token !== state.artifactRenderToken) return;
      elements.artifactView.replaceChildren(buildArtifactTable(artifact, text));
      return;
    }
    if (artifact.kind === "markdown") {
      const text = await loadArtifactSource(artifact);
      if (token !== state.artifactRenderToken) return;
      const article = document.createElement("article");
      article.className = "markdown-body artifact-markdown";
      renderMarkdown(article, text);
      elements.artifactView.replaceChildren(article);
      return;
    }
    throw new Error(t("此格式不支持预览"));
  }

  function renderArtifactResourceMenu(artifact) {
    elements.artifactResourceMenu.replaceChildren();
    for (const item of state.artifacts) {
      const row = document.createElement("div");
      row.className = "artifact-resource-row";
      const button = document.createElement("button");
      button.type = "button";
      button.role = "menuitem";
      button.className = item.id === artifact.id ? "active" : "";
      const label = document.createElement("span");
      label.textContent = item.name;
      const type = document.createElement("small");
      type.textContent = artifactTypeLabel(item);
      button.append(makeIconSlot(artifactIconName(item)), label, type);
      if (item.id === artifact.id) button.appendChild(makeIconSlot("check"));
      button.addEventListener("click", () => {
        state.selectedArtifactId = item.id;
        state.artifactMode = defaultArtifactMode(item);
        state.artifactZoom = 1;
        state.artifactPanX = 0;
        state.artifactPanY = 0;
        closeArtifactResourceMenu();
        renderArtifactWorkspace();
      });
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "icon-button artifact-resource-remove";
      remove.title = t("从列表移除");
      remove.setAttribute("aria-label", t("从列表移除 {name}", {name: item.name}));
      remove.appendChild(makeIconSlot("x"));
      remove.addEventListener("click", (event) => {
        event.stopPropagation();
        dismissArtifact(item.id);
      });
      row.append(button, remove);
      elements.artifactResourceMenu.appendChild(row);
    }
    // 只有一个 artifact 时也要能开这个菜单——删除按钮在菜单里，禁掉就等于
    // 「最后一个删不掉」。当初禁它是因为菜单只用来切换，一个项目没得切。
    elements.artifactTitleButton.disabled = state.artifacts.length === 0;
  }

  /// 从列表里拿掉一个 artifact。回合产出的那些下次同步会重新长出来，所以
  /// 得把 id 记进 dismissed 才删得掉。
  function dismissArtifact(id) {
    dismissedArtifactsForScope().add(id);
    pinnedArtifactsForScope().delete(id);
    state.artifactSourceCache.delete(id);
    state.artifacts = state.artifacts.filter((item) => item.id !== id);
    if (state.selectedArtifactId === id) {
      const next = state.artifacts.at(-1);
      state.selectedArtifactId = next?.id || null;
      state.artifactMode = defaultArtifactMode(next);
      state.artifactZoom = 1;
      state.artifactPanX = 0;
      state.artifactPanY = 0;
    }
    if (!state.artifacts.length) {
      closeArtifactResourceMenu();
      setArtifactWorkspaceOpen(false);
      elements.artifactToggleButton.hidden = true;
      elements.artifactToggleButton.classList.remove("has-new-artifact");
      return;
    }
    renderArtifactWorkspace();
    renderArtifactResourceMenu(state.artifacts.find((item) => item.id === state.selectedArtifactId));
  }

  function renderArtifactWorkspace() {
    if (!state.artifactOpen) return;
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId) || state.artifacts.at(-1);
    if (!artifact) return;
    state.selectedArtifactId = artifact.id;
    const canPreview = artifactSupportsPreview(artifact);
    const canSource = artifactSupportsSource(artifact);
    const isImage = artifact.kind === "image" || artifact.mime.startsWith("image/");
    if ((state.artifactMode === "preview" && !canPreview) || (state.artifactMode === "source" && !canSource)) {
      state.artifactMode = defaultArtifactMode(artifact);
    }
    elements.artifactTitle.textContent = artifact.name;
    elements.artifactTitle.title = artifact.name;
    elements.artifactTypeLabel.textContent = artifactTypeLabel(artifact);
    // ?download=1 → 后端强制 attachment,markdown/pdf 也直接落盘而不是再开预览。
    elements.artifactDownloadButton.href = `${artifact.url}?download=1`;
    // 两个视图都在才需要切换器。原来这里按「是不是图片」判断,svg 一来就露馅了:
    // 它既是图片又是文本,两个视图都有,却因为 mime 是 image/* 被整组藏掉,
    // 源码根本点不到。判据换成「有没有得切」,和具体类型脱钩。
    const showPicture = isImage && state.artifactMode === "preview";
    elements.artifactPreviewButton.parentElement.hidden = !(canPreview && canSource);
    elements.artifactImageActions.hidden = !showPicture;
    elements.artifactImageExternalButton.href = showPicture ? artifact.url : "";
    elements.artifactImageZoomOutButton.disabled = !showPicture || state.artifactZoom <= 0.25;
    elements.artifactImageZoomInButton.disabled = !showPicture || state.artifactZoom >= 4;
    elements.artifactPreviewButton.hidden = !canPreview;
    elements.artifactSourceButton.hidden = !canSource;
    elements.artifactPreviewButton.classList.toggle("active", state.artifactMode === "preview");
    elements.artifactSourceButton.classList.toggle("active", state.artifactMode === "source");
    elements.artifactPreviewButton.setAttribute("aria-pressed", String(state.artifactMode === "preview"));
    elements.artifactSourceButton.setAttribute("aria-pressed", String(state.artifactMode === "source"));
    elements.artifactCopyButton.disabled = !canSource && artifact.kind === "pdf";
    // 图片没有文本可复制,但 svg 有——同样不能只看 mime。
    elements.artifactCopyButton.hidden = isImage && !canSource;
    elements.artifactMaximizeButton.replaceChildren(makeIconSlot(state.artifactMaximized ? "minimize-2" : "maximize-2"));
    elements.artifactMaximizeButton.title = state.artifactMaximized ? t("退出全屏") : t("全屏显示");
    elements.artifactMaximizeButton.setAttribute("aria-label", elements.artifactMaximizeButton.title);
    renderArtifactResourceMenu(artifact);
    const token = ++state.artifactRenderToken;
    elements.artifactView.replaceChildren(artifactLoadingNode());
    const render = state.artifactMode === "source"
      ? renderArtifactSource(artifact, token)
      : renderArtifactPreview(artifact, token);
    render.catch((error) => renderArtifactFailure(error, token));
  }

  async function copySelectedArtifact() {
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId);
    if (!artifact) return;
    try {
      if (artifactSupportsSource(artifact)) {
        await navigator.clipboard.writeText(await loadArtifactSource(artifact));
      } else if (artifact.kind === "image" && window.ClipboardItem) {
        const response = await fetch(artifact.url, { credentials: "same-origin" });
        if (!response.ok) throw new Error(t("图片载入失败"));
        const blob = await response.blob();
        await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
      } else {
        await navigator.clipboard.writeText(artifact.url);
      }
      showToast(t("已复制"), "success");
    } catch (error) {
      showToast(error.message || t("复制失败"), "error");
    }
  }

  function setArtifactMode(mode) {
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId);
    if (!artifact || (mode === "preview" ? !artifactSupportsPreview(artifact) : !artifactSupportsSource(artifact))) return;
    state.artifactMode = mode;
    renderArtifactWorkspace();
  }

  function toggleArtifactMaximized() {
    if (!state.artifactOpen) return;
    state.artifactMaximized = !state.artifactMaximized;
    syncArtifactLayout();
    renderArtifactWorkspace();
  }

  function changeArtifactImageZoom(delta) {
    const artifact = state.artifacts.find((item) => item.id === state.selectedArtifactId);
    if (!artifact || !(artifact.kind === "image" || artifact.mime.startsWith("image/"))) return;
    state.artifactZoom = Math.min(4, Math.max(0.25, (state.artifactZoom || 1) + delta));
    if (state.artifactZoom <= 1) {
      state.artifactZoom = 1;
      state.artifactPanX = 0;
      state.artifactPanY = 0;
    }
    const image = elements.artifactView.querySelector(".artifact-image-stage > img");
    if (image) {
      image.style.transform = `translate(${state.artifactPanX}px, ${state.artifactPanY}px) scale(${state.artifactZoom})`;
      image.closest(".artifact-image-stage")?.classList.toggle("is-zoomed", state.artifactZoom > 1);
    }
    updateArtifactImageControls();
  }

  function validAssetDimension(value) {
    const number = Number(value);
    return Number.isInteger(number) && number > 0 && number <= 100_000 ? number : null;
  }

  function createConversationMedia(asset, { eager = false } = {}) {
    const source = asset && typeof asset === "object" ? asset : {};
    const url = safeAssetUrl(source.url);
    const mime = String(source.mime || "").trim().toLowerCase();
    const imageMime = !mime || mime.startsWith("image/");
    const width = validAssetDimension(source.width);
    const height = validAssetDimension(source.height);
    const alt = String(source.alt || "").trim() || t("Miyu 生成的图片");

    const figure = document.createElement("figure");
    figure.className = "conversation-media";
    if (source.id != null) figure.dataset.assetId = String(source.id);
    const visual = document.createElement("div");
    visual.className = "conversation-media-visual";
    if (width && height) {
      const ratio = width / height;
      if (ratio >= 0.05 && ratio <= 20) {
        visual.classList.add("has-aspect");
        visual.style.aspectRatio = `${width} / ${height}`;
      }
    }
    const fallback = document.createElement("div");
    fallback.className = "conversation-media-fallback";
    fallback.appendChild(makeIconSlot("circle-alert"));
    const fallbackText = document.createElement("span");
    fallbackText.textContent = url && imageMime ? t("图片载入失败") : t("图片地址不可用");
    fallback.appendChild(fallbackText);

    if (url && imageMime) {
      const image = document.createElement("img");
      image.alt = alt;
      image.loading = eager ? "eager" : "lazy";
      image.decoding = "async";
      if (width) image.width = width;
      if (height) image.height = height;
      fallback.hidden = true;
      image.addEventListener("error", () => {
        image.remove();
        fallback.hidden = false;
        figure.classList.add("is-error");
        if (eager) contentAdded(figure);
      }, { once: true });
      // 只有实时流的新图(eager)加载完才跟随滚动;历史重建(刷新)的图不该在
      // 逐张加载时把视图一路拉到底——那正是「打印图片刷新后跳到 AI 输出尾部」
      // 的原因(09-12 #19)。有 aspect-ratio 占位,历史图加载也不跳。
      if (eager) image.addEventListener("load", contentAdded, { once: true });
      image.src = url;
      visual.append(image, fallback);
    } else {
      visual.appendChild(fallback);
    }

    // 图下面既不挂文件名也不挂按钮——每张图多占一行、还把气泡撑得很吵。
    // 名字(表情包是描述)和那三个按钮都跟着灯箱走(web/lightbox.js)。
    // `alt` 仍然写在 img 上,读屏和图裂时靠它。
    if (url && imageMime) {
      visual.classList.add("is-zoomable");
      visual.tabIndex = 0;
      visual.setAttribute("role", "button");
      visual.setAttribute("aria-label", t("放大预览 {alt}", {alt: alt}));
      const openLightbox = () => {
        window.MiyuLightbox?.open({
          url,
          name: alt,
          onOpenInWorkspace: () => {
            registerArtifact({ ...source, url, name: alt, kind: "image" });
            setArtifactWorkspaceOpen(true);
          },
        });
      };
      visual.addEventListener("click", openLightbox);
      visual.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          openLightbox();
        }
      });
    }
    figure.appendChild(visual);
    return figure;
  }

  /*
   * 09-17 起「隐藏」那一档在配置里没有了(用户拍板删掉),后端只剩「展开/收起」
   * 一位布尔。这个保底判断留着是给**老 daemon**用的:网页和 daemon 可以不同版本,
   * 旧的那边仍可能报 `hidden`。WebUI 是否渲染仍以「有没有思考内容」为准;
   * 默认展开/收起由本地偏好 miyu.web.reasoningExpanded 决定。
   */
  function reasoningHidden() {
    return state.display?.reasoning === "hidden";
  }

  function normalizeReasoningTitle(value) {
    const title = String(value || "").trim().replace(/^[*#\s]+|[*#\s]+$/g, "");
    if (!title || /^正在(?:思考)?(?:\.{3}|…+)?$/u.test(title)) return "";
    return title;
  }

  function splitReasoningText(value) {
    const raw = String(value || "").trim();
    const bold = raw.match(/^\*\*([^\n*]{1,160})\*\*(?:\r?\n){0,2}([\s\S]*)$/);
    if (bold) return { title: normalizeReasoningTitle(bold[1]), body: bold[2].trim() };
    const heading = raw.match(/^#{1,6}\s+([^\n]{1,160})(?:\r?\n)+([\s\S]*)$/);
    if (heading) return { title: normalizeReasoningTitle(heading[1]), body: heading[2].trim() };
    return { title: "", body: raw };
  }

  // 窥视槽只放尾巴:换行折成空格,取最后 160 字,够撑满一行还不至于每个 delta 都重排一大段
  function reasoningPeekText(text) {
    return String(text || "").replace(/\s+/g, " ").trimEnd().slice(-160);
  }

  // 「思考滚动显示行数」(display.thinking_scroll_lines):思考收着时,正在想的那一行
  // 底下开一扇窗,滚着露最近几行正文(和终端那扇窗一个意思,用户 09-17:「思考行
  // 不是单行窥视,而是有滚动」)。0 = 不开窗,只留标题行里那截窥视。
  function thinkingWindowLines() {
    const n = Number(state.display?.thinking_scroll_lines);
    return Number.isFinite(n) && n > 0 ? Math.min(Math.floor(n), 200) : 0;
  }

  // 窗里只放尾巴那一截(按每行 240 字估),免得每个 delta 重排整段;写完滚到底。
  function setReasoningWindow(block, text) {
    const node = block?.window;
    if (!node) return;
    const lines = Number(node.style.getPropertyValue("--think-window-lines")) || 10;
    node.textContent = String(text || "").slice(-(lines * 240));
    node.scrollTop = node.scrollHeight;
  }

  // 写入窥视文字并量一下:放得下就左对齐紧跟着时间;放不下才切到尾部可见 + 左侧渐隐
  function setReasoningPeek(peek, text) {
    if (!peek) return;
    peek.textContent = reasoningPeekText(text);
    const slot = peek.parentElement;
    if (slot) slot.classList.toggle("is-overflow", peek.scrollWidth > slot.clientWidth + 1);
  }

  // ── 子代理进度:把中转来的标记流解析成结构化事件 ───────────────────
  // 子代理内部的思考/工具活动经父回合的 tool.progress 通道以标记串上来。
  // Summary 档只有纯文本行(用于标题窥视);Full 档带 __subtool_call__ /
  // __subtool_result__ / __subagent_reasoning__(用于展开后的子过程时间线)。
  const SUBAGENT_MARKERS = {
    reasoning: "__subagent_reasoning__",
    content: "__subagent_content__",
    call: "__subtool_call__",
    result: "__subtool_result__",
    stats: "__subagent_stats__",
    // daemon 的中继发这个(`显示串\t数\t人话`);直连模式的老 runner 发上面那个 stats。
    metric: "__subagent_metric__",
    detach: "__subagent_detach__",
    brief: "__subagent_brief__"
  };
  // 认不出的老标记(`__subagent_reasoning_done__`、`__subtool_preparing__`……)一律吞掉:
  // 落进 plain 分支就原样写进窥视那一行(09-25 走查抓到 `__subagent_metric__` 漏在卡片上)。
  const UNKNOWN_SUBAGENT_MARKER = /^__sub[a-z_]*__/;

  // 子代理任务简介 DOM(展开区最上方):标题 + 整段 prompt。前台从工具参数直接建;
  // 后台经 __subagent_brief__ marker 建(后台事件流里没有参数,09-12 #9)。
  function buildSubagentBrief(title, prompt) {
    // 别叫 t:会遮住翻译函数 t(),标题空着时下面那句 t("任务 prompt") 就抛 TypeError。
    const heading = String(title || "").trim();
    const p = String(prompt || "").trim();
    if (!heading && !p) return null;
    // prompt 做成默认收起的可展开 tag:子代理自动展开活区域时整段 prompt 会刷屏,
    // 收成一行「任务标题」,想看再点开(用户反馈)。
    const brief = document.createElement("details");
    brief.className = "subagent-brief";
    const summary = document.createElement("summary");
    summary.className = "subagent-brief-title";
    // 节点:和时间线其它步同一列、坐在细线上(它是时间线的开头,不再是分离的一块)。
    // 平时显 📋,鼠标悬浮时原地换成展开箭头(不在右侧另起一个,用户要求)。
    const marker = document.createElement("span");
    marker.className = "subagent-brief-marker";
    marker.append(
      makeIconSlot("clipboard", "subagent-brief-icon"),
      makeIconSlot("chevron-right", "subagent-brief-chevron"),
    );
    const label = document.createElement("span");
    label.className = "subagent-brief-name";
    label.textContent = heading || t("任务 prompt");
    summary.append(marker, label);
    brief.appendChild(summary);
    if (p) {
      const body = document.createElement("div");
      body.className = "subagent-brief-prompt";
      body.textContent = p;
      brief.appendChild(body);
    }
    return brief;
  }

  function parseSubagentEvent(message) {
    const text = String(message || "");
    if (text.startsWith(SUBAGENT_MARKERS.reasoning)) {
      // 不要 trim:逐 token 的 reasoning delta 前后的空格是词间空格,trim 掉就成了
      // 「Actuallythetails」这种连成一坨(09-12 #8 思考内容没空格没换行的真因)。
      return { kind: "reasoning", text: text.slice(SUBAGENT_MARKERS.reasoning.length) };
    }
    if (text.startsWith(SUBAGENT_MARKERS.content)) {
      // 同 reasoning:不 trim,逐 token 的正文 delta 词间空格要留住。
      return { kind: "content", text: text.slice(SUBAGENT_MARKERS.content.length) };
    }
    if (text.startsWith(SUBAGENT_MARKERS.call)) {
      try {
        const payload = JSON.parse(text.slice(SUBAGENT_MARKERS.call.length));
        const args = typeof payload.args === "string" ? payload.args : JSON.stringify(payload.args ?? {});
        return { kind: "call", name: String(payload.name || ""), display: String(payload.display || ""), args, subject: toolSubject(payload.name, args) };
      } catch {
        return { kind: "plain", text: text.slice(SUBAGENT_MARKERS.call.length).trim() };
      }
    }
    if (text.startsWith(SUBAGENT_MARKERS.result)) {
      try {
        const payload = JSON.parse(text.slice(SUBAGENT_MARKERS.result.length));
        const args = typeof payload.args === "string" ? payload.args : JSON.stringify(payload.args ?? {});
        return { kind: "result", name: String(payload.name || ""), display: String(payload.display || ""), args, ok: payload.ok !== false, output: String(payload.output ?? "") };
      } catch {
        return { kind: "plain", text: text.slice(SUBAGENT_MARKERS.result.length).trim() };
      }
    }
    if (text.startsWith(SUBAGENT_MARKERS.stats)) return { kind: "stats", text: text.slice(SUBAGENT_MARKERS.stats.length).trim() };
    if (text.startsWith(SUBAGENT_MARKERS.metric)) {
      const [tokens = ""] = text.slice(SUBAGENT_MARKERS.metric.length).split("\t");
      return { kind: "metric", tokens: tokens.replace(/\s+/g, "") };
    }
    if (text.startsWith(SUBAGENT_MARKERS.brief)) {
      try {
        const p = JSON.parse(text.slice(SUBAGENT_MARKERS.brief.length));
        return { kind: "brief", description: String(p.description || ""), prompt: String(p.prompt || "") };
      } catch {
        return { kind: "plain", text: "" };
      }
    }
    if (text.startsWith(SUBAGENT_MARKERS.detach)) return { kind: "plain", text: text.slice(SUBAGENT_MARKERS.detach.length).trim() };
    if (UNKNOWN_SUBAGENT_MARKER.test(text)) return { kind: "ignored" };
    return { kind: "plain", text: text.trim() };
  }

  // 子过程时间线:子代理自己的思考与工具流,复用主对话同一套渲染——proc-line
  // 细线时间线 + createReasoningBlock(思考:累加、可展开、有窥视、动画)+
  // createPersistedToolCard(完成的工具卡,与主流工具卡同构)。sink.blocks 承载
  // proc-line;sink.think 是当前正累加的思考块。
  function subEndReasoning(sink) {
    if (sink.think) {
      // 冲掉未触发的 rAF,把最终全文渲一遍,再释放帧句柄给下一个思考块。
      if (sink.thinkFrame) {
        window.cancelAnimationFrame(sink.thinkFrame);
        sink.thinkFrame = null;
      }
      const finalText = sink.think.__acc != null ? sink.think.__acc : sink.thinkAccum;
      sink.think.body.textContent = finalText || "";
      setReasoningPeek(sink.think.peek, finalText || "");
      sink.think.title.textContent = t("已思考");
      sink.think.element.classList.remove("is-live");
      sink.think.element.classList.remove("has-window");
      if (sink.think.window) {
        sink.think.window.remove();
        sink.think.window = null;
      }
      // 冻结读秒(09-12 #4:子过程思考读秒一直停在 0s)。startedAt 是创建时的
      // performance.now();收尾时算出最终耗时定格,ticker 靠 is-live 判活,收尾即停。
      const ls = sink.think.liveStatus;
      if (ls && sink.think.startedAt != null) {
        ls.textContent = `${((performance.now() - sink.think.startedAt) / 1000).toFixed(1)}s`;
      }
      sink.think = null;
      sink.thinkAccum = "";
    }
  }

  // 子代理正文段收尾:把当前正在累加的正文块定格(内容留在时间线里),
  // 下一段正文会另起一块,中间穿插思考/工具卡——和主对话的交错渲染同构。
  function subEndContent(sink) {
    if (sink.contentBlock) {
      // 收尾时把最终全文渲一遍(可能有帧还没触发),再释放帧句柄,让下一段正文能重新调度。
      if (sink.contentFrame) {
        window.cancelAnimationFrame(sink.contentFrame);
        sink.contentFrame = null;
      }
      renderMarkdown(sink.contentBlock, sink.contentBlock.__subAcc || sink.contentAccum || "");
      sink.contentBlock = null;
      sink.contentAccum = "";
    }
  }

  // 输入框那排的「用时」跑着的时候半秒刷一次(一秒一刷的相位对不上,读数最多晚一秒);
  // 停了它就不在了。
  setInterval(() => {
    if (document.hidden || !state.turnClock) return;
    renderTurnClock();
  }, 500);

  // 子过程时间线里「正在思考」的读秒 ticker:主对话那份有各自的 live 计时器,
  // 子过程这份没有,所以读秒永远停在 0s。这个全局 ticker 按 is-live 更新所有
  // 子过程思考块的读秒(用 dataset.subStart 存的起点)。
  setInterval(() => {
    if (document.hidden) return;
    const nodes = document.querySelectorAll(".sub-blocks .reasoning-block.is-live .reasoning-live-status[data-sub-start]");
    for (const ls of nodes) {
      const start = Number(ls.dataset.subStart);
      if (!Number.isFinite(start)) continue;
      ls.textContent = `${Math.max(0, Math.floor((performance.now() - start) / 1000))}s`;
    }
    // 前台子代理行的读秒(09-12 #6):跑着时逐秒走,卡片进入成功/失败即定格。
    for (const el of document.querySelectorAll(
      ".tool-card.is-task .tool-task-seconds[data-task-start]," +
        " .tool-card.is-command .tool-command-seconds[data-task-start]"
    )) {
      const start = Number(el.dataset.taskStart);
      if (!Number.isFinite(start)) continue;
      const card = el.closest(".tool-card");
      const done = card && (card.classList.contains("is-success") || card.classList.contains("is-failure"));
      const secs = (performance.now() - start) / 1000;
      if (done) {
        el.textContent = formatJobDuration(secs);
        delete el.dataset.taskStart;
      } else {
        el.textContent = formatJobDuration(secs);
      }
    }
  }, 1000);

  // 子过程时间线增长时自动滚到底(09-12 #7:展开后 timeline 继续长不自动滚)。
  // 滚的是最近的可滚容器(前台=.sub-blocks 本身,后台=外层 .job-stream-panel);
  // 只有用户本来就贴着底才跟随,往上翻了就不抢。
  function subScrollContainer(sink) {
    const el = sink && sink.blocks;
    if (!el) return null;
    let c = el;
    while (c && c !== document.body) {
      const style = window.getComputedStyle(c);
      if (/(auto|scroll)/.test(style.overflowY) && c.scrollHeight > c.clientHeight + 1) break;
      c = c.parentElement;
    }
    if (!c || c === document.body) c = el;
    return c;
  }
  // 贴底跟随:先量「改内容之前是不是贴着底」,mutate 完只在原本贴底时才拉回底。
  // 不靠区分程序/用户滚动(那套 pinnedUp 会被程序自己的归位清掉、导致往上翻又被拽回 #139),
  // 就一条:你原本在底我才跟,你往上翻了(改前就不在底)我一步都不动。
  function subStickBottom(sink, mutate) {
    const c = subScrollContainer(sink);
    const atBottom = c ? (c.scrollHeight - c.scrollTop - c.clientHeight) < 30 : false;
    mutate();
    if (c && atBottom) c.scrollTop = c.scrollHeight;
  }
  // 往子过程区加一个块(思考块头/工具卡)必须走「加之前先量在不在底,加完只在原本
  // 贴底时才拉回底」——直接 procLineAttach 会把容器撑高却不滚,一次没滚就把整条贴底
  // 跟随链打断,之后逐 token 的 subStickBottom 全测得「改前不在底」再不跟(#159/#160,
  // 前台后台同此)。「加完再量」那种跟法(原来的 subAutoScroll,09-25 删了)块一高就已经离底 >30px,修不回来。
  function subAttach(sink, el) {
    subStickBottom(sink, () => procLineAttach(sink.blocks, el));
  }

  function renderSubagentProgress(sink, message) {
    const ev = parseSubagentEvent(message);
    if (ev.kind === "ignored") return;
    if (ev.kind === "metric") {
      // daemon 模式下子代理的词元只从这儿来:只喂卡片上那个小标。输入框的「累计」那套
      // (liveSubagentTokens 的 done/基线)是照直连模式的 stats 写的,接上可能重复计。
      if (ev.tokens) {
        sink.tokenText = ev.tokens;
        if (sink.taskToken) sink.taskToken.textContent = sink.tokenText;
      }
      return;
    }
    if (ev.kind === "stats") {
      // stats 文本形如「工具调用 3 次　消耗词元 ≈1.2k」/「tool calls: 3　token cost: 1.2k」,
      // 每步更新一次。抠出 token 数(可能带 ≈ 前缀),喂给任务条那行的 token 显示(09-12 item 4)。
      const m = ev.text.match(/(?:词元|cost)\s*[：:]?\s*(≈?\s*[\d.]+\s*[kKmMbB万]?)/);
      if (m) {
        sink.tokenText = m[1].replace(/\s+/g, "");
        if (sink.taskToken) sink.taskToken.textContent = sink.tokenText;
        // 前台工具卡 / 后台任务条(job)都从这里过:把这次子代理的实时 token 估算按
        // usageKey 汇进输入框那个「累计」(#131,后台子代理同样接上)。回放/播种时不接
        // (那是历史,会和后端基线重复计)。
        const key = sink.usageKey || (sink.id != null ? sink.id : null);
        const n = tokensFromCount(sink.tokenText);
        if (key != null && n != null && !state.seedingLive) {
          // 保留已有的 done/baseAtDone:子代理收尾还可能再来一条 stats,别把完成态覆盖没了。
          const prev = state.liveSubagentTokens.get(key);
          state.liveSubagentTokens.set(key, { tokens: n, done: prev?.done || false, baseAtDone: prev?.baseAtDone });
          refreshComposerCumulative();
        }
      }
      return;
    }
    if (!sink.blocks) return;
    if (ev.kind === "brief") {
      // 后台子代理展开区顶部补任务简介(09-12 #9;前台已在 createTool 里建好并置
      // sink.brief,不会重复)。插在子过程时间线容器之前。
      if (!sink.brief) {
        const brief = buildSubagentBrief(ev.description, ev.prompt);
        if (brief) {
          attachSubBrief(sink.blocks, brief);
          sink.brief = true;
        }
      }
      return;
    }
    if (ev.kind === "content") {
      // 空 delta 直接丢:否则会造一个空正文块并 procLineBreak 切断时间线(「串」的根)。
      if (!ev.text) return;
      // 子代理正文逐 token 增量(#6:光有 timeline,正文没流出来)。先收思考,再把
      // 正文累加到一个活的正文块;新起一段正文时切断当前时间线,正文落在段间,
      // 之后的工具/思考会另起一条 proc-line——和主对话交错渲染同构。
      if (!sink.contentBlock) {
        subStickBottom(sink, () => {
          subEndReasoning(sink);
          procLineBreak(sink.blocks);
          const div = document.createElement("div");
          div.className = "sub-content markdown-body";
          sink.blocks.appendChild(div);
          sink.contentBlock = div;
          sink.contentAccum = "";
        });
      }
      sink.contentAccum += ev.text;
      // markdown 渲染按 rAF 合并:逐 token 全量重解析太费,一帧渲一次就够顺。
      // 累加文本挂在块元素上,帧触发时读它当前值(而非调度那刻的旧值),避免同一
      // 帧内后到的 token 被丢。
      const block = sink.contentBlock;
      block.__subAcc = sink.contentAccum;
      if (!sink.contentFrame) {
        sink.contentFrame = window.requestAnimationFrame(() => {
          sink.contentFrame = null;
          subStickBottom(sink, () => renderMarkdown(block, block.__subAcc || ""));
        });
      }
      sink.peekLine = sink.contentAccum;
      if (sink.taskPeek) setReasoningPeek(sink.taskPeek, sink.contentAccum);
      return;
    }
    if (ev.kind === "reasoning") {
      // 空 delta 直接丢:否则会造一个空思考块(「串」尤其是思考的根)。
      if (!ev.text) return;
      // 思考逐 token 增量,累加到一个活的思考块(不能覆盖,否则只剩最后一个 token)。
      subEndContent(sink);
      if (!sink.think) {
        sink.think = createReasoningBlock("", t("正在思考"), true);
        sink.thinkAccum = "";
        // 读秒 ticker 靠这个起点更新(见 subEndReasoning 上方的 setInterval)。
        if (sink.think.liveStatus && sink.think.startedAt != null) {
          sink.think.liveStatus.dataset.subStart = String(sink.think.startedAt);
        }
        subAttach(sink, sink.think.element);
      }
      sink.thinkAccum += ev.text;
      sink.think.raw = sink.thinkAccum;
      // 正文体逐 token 全量重写 textContent,展开态下每个 token 都重排,长思考会卡死
      // (#114:打开正在思考的行特别卡)。按 rAF 合并:一帧只写一次当前全文。
      const think = sink.think;
      think.__acc = sink.thinkAccum;
      if (!sink.thinkFrame) {
        sink.thinkFrame = window.requestAnimationFrame(() => {
          sink.thinkFrame = null;
          subStickBottom(sink, () => {
            think.body.textContent = think.__acc || "";
            setReasoningPeek(think.peek, think.__acc || "");
            setReasoningWindow(think, think.__acc || "");
            // 行窥视也合进这一帧:每 token 各测一次 scrollWidth 会引发同步重排,连带把
            // 已完成的「已思考」行窥视一起抖(#3 疯狂抖动)。一帧只测一次。
            if (sink.taskPeek) setReasoningPeek(sink.taskPeek, think.__acc || "");
          });
        });
      }
      sink.peekLine = sink.thinkAccum;
      return;
    }
    if (ev.kind === "call") {
      subEndReasoning(sink);
      subEndContent(sink);
      // Full 档:call 先记着,result 到了再落一张完成卡(带 args + output)。
      sink.pendingCall = { name: ev.name, display: ev.display, args: ev.args, subject: ev.subject };
      // 窥视也用友好显示名(#7:展开是「运行命令」,窥视却还是裸的 run_command)。
      const callLabel = ev.display || ev.name;
      sink.peekLine = ev.subject ? callLabel + " · " + ev.subject : t("调用 ") + callLabel;
      if (sink.taskPeek) setReasoningPeek(sink.taskPeek, sink.peekLine);
      return;
    }
    if (ev.kind === "result") {
      subEndReasoning(sink);
      subEndContent(sink);
      const call = sink.pendingCall || { name: ev.name, display: ev.display, args: ev.args };
      sink.pendingCall = null;
      const card = createPersistedToolCard({ name: call.name, display_name: call.display, arguments: call.args != null ? call.args : ev.args, output: ev.output, ok: ev.ok });
      subAttach(sink, card);
      sink.peekLine = (call.display || call.name) + " " + (ev.ok ? t("完成") : t("出错"));
      if (sink.taskPeek) setReasoningPeek(sink.taskPeek, sink.peekLine);
      return;
    }
    if (ev.kind === "plain" && ev.text) {
      // Summary 档没有结构化标记(WebUI 回合强制 Full,一般走不到这):只有
      // `工具 #N：名字 · 主语 运行中/ok/err`。运行中不落卡(没 args/output),
      // ok/err 时落一张完成卡。
      const match = ev.text.match(/^(?:工具|tool)\s*#(\d+)[:：]?\s*(.*)$/i);
      if (!match) {
        sink.peekLine = ev.text;
        if (sink.taskPeek) setReasoningPeek(sink.taskPeek, ev.text);
        return;
      }
      const rest = match[2].trim();
      const running = /(?:运行中|running)$/i.test(rest);
      const errored = /(?:\berr\b|错误|失败)$/i.test(rest);
      const finished = !running && /(?:\bok\b|\berr\b|完成|失败|错误)$/i.test(rest);
      const label = rest.replace(/\s*(?:运行中|running|ok|err)$/i, "").trim();
      sink.peekLine = label || rest;
      if (sink.taskPeek) setReasoningPeek(sink.taskPeek, sink.peekLine);
      if (finished) {
        subEndReasoning(sink);
        const at = label.indexOf(" · ");
        const nm = at >= 0 ? label.slice(0, at) : label;
        const subj = at >= 0 ? label.slice(at + 3) : "";
        const card = createPersistedToolCard({ name: nm, arguments: subj, output: "", ok: !errored });
        subAttach(sink, card);
      }
    }
  }

  function createReasoningBlock(text, title = t("已思考"), live = false, summaryOnly = false, withWindow = false) {
    const details = document.createElement("details");
    details.className = "reasoning-block";
    details.classList.toggle("is-summary", summaryOnly);
    details.classList.toggle("is-live", live);
    details.open = state.reasoningExpanded === true;
    const summary = document.createElement("summary");
    const atom = makeIconSlot("atom", "reasoning-icon");
    if (live) for (let index = 0; index < 3; index += 1) atom.appendChild(document.createElement("i"));
    const titleNode = document.createElement("span");
    titleNode.className = "reasoning-title";
    titleNode.textContent = title || (live ? t("正在思考") : t("已思考"));
    const chevron = makeIconSlot("chevron-right", "reasoning-chevron");
    summary.append(atom, titleNode);
    let liveStatus = null;
    let progress = null;
    if (live) {
      liveStatus = document.createElement("span");
      liveStatus.className = "reasoning-live-status";
      liveStatus.textContent = "0s";
      summary.appendChild(liveStatus);
      progress = document.createElement("div");
      progress.className = "reasoning-progress";
      progress.setAttribute("role", "progressbar");
      progress.setAttribute("aria-label", t("思考进度"));
      progress.setAttribute("aria-valuetext", t("正在思考"));
      const progressFill = document.createElement("i");
      progressFill.setAttribute("aria-hidden", "true");
      progress.appendChild(progressFill);
    }
    // 思考内容收着的时候,标题行右边那片空白放思考的尾巴:正在想就跟着滚,想完了
    // 也留着(回看那份同样有),展开时才让位。尾部对齐,新字从右边推进来,旧字从左边淡出。
    const slot = document.createElement("span");
    slot.className = "reasoning-peek";
    const peek = document.createElement("span");
    slot.appendChild(peek);
    summary.appendChild(slot);
    // 此时还没挂进文档量不到宽度;先写字,挂上后由 fit/下一次 delta 再量
    peek.textContent = reasoningPeekText(text);
    window.requestAnimationFrame(() => setReasoningPeek(peek, text));
    summary.appendChild(chevron);
    // 正在想 + 思考收着:标题底下那扇滚动窗(见 thinkingWindowLines)。想完或点开
    // 就让位——想完那一行收成「已思考 · Xs」,和原来一样。
    // 它得挂在 <summary> 里(summary 换行铺满一行):<details> 合着时 summary 以外
    // 的子元素浏览器根本不画,量得到尺寸却不上屏(Chromium 151 实测)。
    // 只给主线的思考块开窗:子代理子过程、后台任务流里那些块仍是标题行里那截
    // 单行窥视(用户 09-17:子代理那一路回滚,全部沿用以前的单行刷新窥视)。
    let windowNode = null;
    const windowLines = live && withWindow ? thinkingWindowLines() : 0;
    if (windowLines > 0) {
      windowNode = document.createElement("div");
      windowNode.className = "reasoning-window";
      windowNode.style.setProperty("--think-window-lines", String(windowLines));
      windowNode.setAttribute("aria-hidden", "true");
      details.classList.add("has-window");
      summary.appendChild(windowNode);
    }
    const body = document.createElement("div");
    body.className = "reasoning-text";
    body.textContent = String(text || "");
    details.append(summary);
    if (progress) details.appendChild(progress);
    details.appendChild(body);
    const block = {
      element: details,
      title: titleNode,
      liveStatus,
      progress,
      body,
      peek,
      window: windowNode,
      raw: String(text || ""),
      pendingTitle: "",
      summaryOnly,
      partOpen: false,
      startedAt: live ? performance.now() : null,
      finished: !live,
      userToggled: false,
      ignoreNextToggle: false
    };
    details.addEventListener("toggle", () => {
      if (block.ignoreNextToggle) {
        block.ignoreNextToggle = false;
        return;
      }
      block.userToggled = true;
      railSnapFit(details);
    });
    return block;
  }

  function createAssistantMessage({
    content = "",
    reasoning = "",
    reasoningTitle = t("已思考"),
    // 工具轮次（持久化回合用）。实时那份由事件流按到达顺序往 blocks 里插，
    // 推理、正文、工具卡是交错的；这里从 turn.tool_flow 重建同样的顺序。
    toolRounds = [],
    // 已回答的问题(#5b):按时序落在它对应的 ask_question 工具位上,不再被整
    // 堆到助手消息之前(刷新后问答卡跑到正文前面就是这么来的)。ask_question 在
    // tool_flow 里就是一个调用,这里遇到它就用第 N 个 exchange 顶替那张裸工具卡。
    questionExchanges = [],
    assets = [],
    timestamp = null,
    tokenTotal = 0,
    tokenPrompt = 0,
    tokenCached = 0,
    tokenEstimated = false,
    // 刷新后也要有的「累计」与「每秒」(09-11):累计由 renderConversation 按顺序算好
    cumulative = null,
    generationTokens = 0,
    generationMs = 0,
    providerId = "",
    model = "",
    activeContext = true,
    turnId = null,
    muted = false,
    segmentKind = "final",
    redoTarget = null
  } = {}) {
    const article = document.createElement("article");
    article.className = `message assistant-message${muted ? " is-muted" : ""}`;
    article.dataset.role = "assistant";
    if (turnId) article.dataset.turnId = turnId;
    article.dataset.segmentKind = segmentKind;
    const header = document.createElement("header");
    header.className = "assistant-label";
    const avatar = document.createElement("img");
    avatar.alt = "";
    avatar.setAttribute("aria-hidden", "true");
    setPersonaAvatar(avatar);
    const identity = document.createElement("div");
    const name = document.createElement("strong");
    name.textContent = state.persona.name;
    identity.append(name);
    header.append(avatar, identity);
    const assistantContent = document.createElement("div");
    assistantContent.className = "assistant-content";
    const blocks = document.createElement("div");
    blocks.className = "assistant-blocks";
    // 逐轮重建:每一轮是「思考 → 正文 → 这轮调的工具」,轮次之间按顺序排,
    // 最后才是本回合的最终思考与回答。把所有工具堆到最前面是错的——那样
    // 一个十轮的回合会先甩出二十个工具卡,中间说了什么全看不见了。
    //
    // 卡片必须挂在 blocks 里:样式表是 `.assistant-blocks > .tool-card`,
    // 挂在外面选择器不命中,会退化成一行裸文本。
    const exchangeQueue = Array.isArray(questionExchanges) ? [...questionExchanges] : [];
    for (const round of Array.isArray(toolRounds) ? toolRounds : []) {
      const roundReasoning = String(round?.assistant_reasoning || "");
      if (roundReasoning.trim() && !reasoningHidden()) {
        const parsed = splitReasoningText(roundReasoning);
        procLineAttach(blocks, createReasoningBlock(parsed.body, t("已思考"), false).element, true);
      }
      const roundContent = String(round?.assistant_content || "");
      if (roundContent.trim()) {
        const markdown = document.createElement("div");
        markdown.className = "markdown-body";
        renderMarkdown(markdown, roundContent);
        procLineBreak(blocks);
        blocks.appendChild(markdown);
      }
      for (const call of Array.isArray(round?.calls) ? round.calls : []) {
        // ask_question 这一步:用它对应的已回答卡顶替裸工具卡,落在原时序位。
        if (String(call?.name || "") === "ask_question" && exchangeQueue.length) {
          procLineBreak(blocks);
          blocks.appendChild(createAnsweredQuestionCard(exchangeQueue.shift()));
          continue;
        }
        procLineAttach(blocks, createPersistedToolCard(call), true);
        // share_file 的富预览(播放器/图片/下载条)重建:实时靠 tool.finished
        // 的输出渲染,刷新/切换后从落库的 tool_flow 输出里复原同一份。
        if (window.MiyuShared?.isShareTool(String(call?.name || ""))) {
          const shared = window.MiyuShared.renderCard(String(call?.output || ""));
          if (shared) {
            procLineBreak(blocks);
            blocks.appendChild(shared);
          }
        }
      }
    }
    if (String(reasoning || "").trim() && !reasoningHidden()) {
      const parsed = splitReasoningText(reasoning);
      procLineAttach(blocks, createReasoningBlock(parsed.body, t("已思考"), false).element, true);
    }
    if (String(content || "").trim()) {
      const markdown = document.createElement("div");
      markdown.className = "markdown-body";
      renderMarkdown(markdown, content);
      procLineBreak(blocks);
      blocks.appendChild(markdown);
    }
    // tool_flow 里没找到对应 ask_question 调用的已回答卡(边角情形)兜底补在末尾,
    // 总比丢掉强;正常情形上面已按位插完,这里为空。
    for (const exchange of exchangeQueue) {
      procLineBreak(blocks);
      blocks.appendChild(createAnsweredQuestionCard(exchange));
    }
    for (const asset of Array.isArray(assets) ? assets : []) {
      procLineBreak(blocks);
      blocks.appendChild(createConversationMedia(asset));
    }
    // 回合以工具收尾(没有最终正文)时,最后那条时间线也要切断,否则总结行永远不出
    procLineBreak(blocks);
    assistantContent.appendChild(blocks);
    assistantContent.classList.toggle("is-slim", !blocks.querySelector(WIDE_BLOCK_SELECTOR));
    article.append(header, assistantContent);

    const meta = document.createElement("div");
    meta.className = "assistant-meta";
    if (state.display?.show_mixed_model_endpoint && (String(providerId || "").trim() || String(model || "").trim())) {
      const endpoint = document.createElement("span");
      endpoint.className = "assistant-endpoint";
      endpoint.textContent = [providerId, model].map((value) => String(value || "").trim()).filter(Boolean).join(" / ");
      meta.appendChild(endpoint);
    }
    // 刷新后的回合也带「累计」与「每秒」:累计按会话里到这一轮为止的顺序求和(与
    // run.completed 事件里 daemon 算的口径一致),速度用落库的样本。
    const usageText = formatUsageMeta({
      turnTotal: tokenTotal,
      turnPrompt: tokenPrompt,
      turnCached: tokenCached,
      estimated: tokenEstimated,
      cumulative: cumulative?.total,
      cumulativePrompt: cumulative?.prompt,
      cumulativeCached: cumulative?.cached,
      generationTokens: generationTokens,
      generationMs: generationMs
    });
    if (usageText) {
      const token = document.createElement("span");
      token.textContent = usageText;
      meta.appendChild(token);
    }
    if (!activeContext) {
      const contextBadge = document.createElement("span");
      contextBadge.className = "context-state-badge";
      contextBadge.textContent = t("已移出当前上下文");
      meta.appendChild(contextBadge);
    }
    const copyValue = String(content || "").trim() || String(reasoning || "");
    if (copyValue || redoTarget) {
      const spacer = document.createElement("span");
      spacer.className = "meta-spacer";
      meta.appendChild(spacer);
      if (redoTarget) {
        const redo = makeMessageAction("refresh-cw", t("重新生成回复"), () => submitRedo(redoTarget));
        redo.className = "redo-action";
        meta.appendChild(redo);
      }
      if (copyValue) meta.appendChild(makeCopyButton(copyValue, t("复制回复")));
    }
    if (meta.childNodes.length) article.appendChild(meta);
    return article;
  }

  function setAssistantRedoAction(article, candidate) {
    const meta = article?.querySelector(".assistant-meta");
    if (!meta) return;
    meta.querySelector(".redo-action")?.remove();
    if (!candidate) return;
    const redo = makeMessageAction("refresh-cw", t("重新生成回复"), () => submitRedo(candidate));
    redo.className = "redo-action";
    const copy = meta.querySelector("button:last-child");
    if (copy) meta.insertBefore(redo, copy);
    else meta.appendChild(redo);
  }

  function createAnsweredQuestionCard(exchange, compact = true) {
    const card = document.createElement("section");
    card.className = "answered-question-card";
    if (compact) card.classList.add("is-compact");
    const header = document.createElement("header");
    // 去掉左边那个大对钩(#143 用户嫌大):「已回答」二字已经表达状态了。
    const copy = document.createElement("div");
    const status = document.createElement("small");
    status.textContent = t("已回答");
    const title = document.createElement("strong");
    const questions = Array.isArray(exchange?.questions) ? exchange.questions : [];
    title.textContent = questions.length === 1 ? String(questions[0]?.header || t("补充确认")) : t("{count} 项补充确认", {count: questions.length});
    copy.append(status, title);
    header.append(copy);
    const list = document.createElement("dl");
    list.className = "answered-question-list";
    const answers = Array.isArray(exchange?.answers) ? exchange.answers : [];
    questions.forEach((question, index) => {
      const row = document.createElement("div");
      const term = document.createElement("dt");
      term.textContent = String(question?.question || question?.header || t("问题 {index}", {index: index + 1}));
      const description = document.createElement("dd");
      const selected = Array.isArray(answers[index]) ? answers[index] : [];
      description.textContent = selected.map(String).join("、") || t("未记录");
      row.append(term, description);
      list.appendChild(row);
    });
    card.append(header, list);
    return card;
  }

  function createPersistedQuestion(exchange, turnId) {
    const wrapper = document.createElement("article");
    wrapper.className = "persisted-question-wrap";
    if (turnId) wrapper.dataset.turnId = turnId;
    wrapper.appendChild(createAnsweredQuestionCard(exchange));
    return wrapper;
  }

  function createTurnStatus(turn) {
    const status = document.createElement("div");
    status.className = "turn-status-line";
    status.dataset.turnStatus = String(turn?.id || "");
    // 也标上 turn-id：命令回执按「锚点回合的最后一个 [data-turn-id] 节点」
    // 插入，不标的话回执会插在这条状态行**之前**，时间顺序看着是乱的。
    if (turn?.id) status.dataset.turnId = String(turn.id);
    const isInterrupted = turn?.status === "interrupted";
    status.classList.toggle("is-interrupted", isInterrupted);
    status.appendChild(makeIconSlot(isInterrupted ? "circle-alert" : "loader-circle"));
    const text = document.createElement("span");
    text.textContent = isInterrupted ? t("本轮已中断") : t("本轮正在运行");
    status.appendChild(text);
    if (asFiniteNumber(turn?.token_total) > 0) {
      const usage = document.createElement("span");
      usage.textContent = t("{approx}{total} tokens", {approx: turn.token_usage_estimated ? t("约 ") : "", total: formatTokens(turn.token_total)});
      status.appendChild(usage);
    }
    if (turn?.active_context === false) {
      const context = document.createElement("span");
      context.className = "context-state-badge";
      context.textContent = t("已移出当前上下文");
      status.appendChild(context);
    }
    return status;
  }

  function renderPersistedTurn(turn) {
    const turnId = String(turn?.id || "");
    const candidate = state.redoCandidate && String(state.redoCandidate.turn_id) === turnId
      ? state.redoCandidate
      : null;
    appendUserMessage(elements.timeline, turn?.user_content || "", turn?.user_timestamp, {
      turnId,
      inputId: turnId,
      revisionTarget: candidate && String(candidate.input_id) === turnId ? candidate : null,
      attachments: turn?.attachments,
      fromParent: turn?.from_parent === true
    });

    /*
     * 本页会话内完成的 turn:优先复用 live 流式渲染出的 article(含按时序排列的
     * 思考签 / 工具签 / 正文块),避免用扁平的「单 reasoning + 正文」重建而丢失时序。
     * 历史重载(后端快照没有 parts 顺序)才退回扁平重建。
     */
    const stash = turnId && turn?.status !== "running" ? state.finishedTurnArticles.get(turnId) : null;
    const claimed = turn?.status === "running" && liveClaimsTurn(turnId);
    let stashIndex = 0;
    const takeStash = (kind) => {
      if (!stash || stashIndex >= stash.length || stash[stashIndex].kind !== kind) return null;
      return stash[stashIndex++].article;
    };

    // 已回答的问题卡:live 存档里原位保留;快照重建时**不再**整堆甩在助手消息
    // 之前(#5b:刷新后问答卡跑到正文前面),而是交给下面的 createAssistantMessage
    // 按 ask_question 的时序位插进 blocks。只有在没有最终助手块可挂时才在这里兜底。
    const persistedExchanges = (!stash && !claimed && Array.isArray(turn?.question_exchanges))
      ? turn.question_exchanges
      : [];

    const followups = Array.isArray(turn?.followups) ? turn.followups : [];
    for (const followup of followups) {
      const precedingContent = String(followup?.preceding_assistant_content || "");
      const precedingReasoning = String(followup?.preceding_assistant_reasoning || "");
      const stashedSegment = takeStash("segment");
      if (stashedSegment) {
        elements.timeline.appendChild(stashedSegment);
      } else if (!claimed && (precedingContent.trim() || precedingReasoning.trim())) {
        elements.timeline.appendChild(createAssistantMessage({
          content: precedingContent,
          reasoning: precedingReasoning,
          providerId: followup?.provider_id,
          model: followup?.model,
          timestamp: followup?.submitted_at,
          turnId,
          segmentKind: "segment",
          activeContext: turn?.active_context !== false
        }));
      }
      appendUserMessage(elements.timeline, followup?.content || "", followup?.submitted_at, {
        turnId,
        followupId: String(followup?.id || ""),
        inputId: String(followup?.id || ""),
        revisionTarget: candidate && String(candidate.input_id) === String(followup?.id || "") ? candidate : null,
        attachments: followup?.attachments
      });
    }
    let leftoverSegment;
    while ((leftoverSegment = takeStash("segment"))) elements.timeline.appendChild(leftoverSegment);

    // 这一轮调过的工具。`stash` 存在说明刚在本端实时渲染过，实时卡片还在
    // 原位，不要再画一遍。卡片要交给助手消息放进它的 `assistant-blocks`
    // 里——挂在外面样式选择器不命中，会退化成一行裸文本。
    const persistedToolRounds = stash
      ? []
      : (Array.isArray(turn?.tool_flow) ? turn.tool_flow : []);

    const assistantContent = String(turn?.assistant_content || "");
    const assistantReasoning = String(turn?.assistant_reasoning || "");
    const assets = turn?.status === "running" ? [] : (Array.isArray(turn?.assets) ? turn.assets : []);
    const stashedFinal = takeStash("final");
    if (stashedFinal) {
      stashedFinal.classList.toggle("is-muted", turn?.active_context === false);
      stashedFinal.dataset.segmentKind = "final";
      setAssistantRedoAction(stashedFinal, candidate);
      elements.timeline.appendChild(stashedFinal);
    } else if (
      !claimed
      && (assistantContent.trim()
        || assistantReasoning.trim()
        || assets.length
        || persistedToolRounds.length
        || persistedExchanges.length)
    ) {
      elements.timeline.appendChild(createAssistantMessage({
        content: assistantContent,
        reasoning: assistantReasoning,
        toolRounds: persistedToolRounds,
        questionExchanges: persistedExchanges,
        providerId: turn?.provider_id,
        model: turn?.model,
        assets,
        timestamp: turn?.assistant_timestamp,
        tokenTotal: turn?.token_total,
        tokenPrompt: turn?.token_prompt,
        tokenCached: turn?.token_cache_read,
        tokenEstimated: Boolean(turn?.token_usage_estimated),
        cumulative: state.cumulativeByTurn?.get(turnId) || null,
        generationTokens: turn?.generation_tokens,
        generationMs: turn?.generation_ms,
        activeContext: turn?.active_context !== false,
        turnId,
        segmentKind: "final",
        redoTarget: candidate,
        muted: turn?.active_context === false
      }));
    }
    if ((turn?.status === "running" && !claimed) || turn?.status === "interrupted") elements.timeline.appendChild(createTurnStatus(turn));
    else if (!stashedFinal && !assistantContent.trim() && !assistantReasoning.trim() && (asFiniteNumber(turn?.token_total) > 0 || turn?.active_context === false)) {
      const metadata = createTurnStatus({ ...turn, status: "completed" });
      metadata.querySelector("span:nth-child(2)").textContent = t("本轮已完成");
      metadata.querySelector(".icon-slot").replaceChildren(createIcon("check"));
      elements.timeline.appendChild(metadata);
    }
  }

  function renderConversation({ forceScroll = false } = {}) {
    elements.loadingState.hidden = true;
    elements.blockedState.hidden = true;
    clearQuestionDock();
    // 每条回合的「累计」=会话里到它为止的顺序求和(与 run.completed 里 daemon 报的口径一致)
    state.cumulativeByTurn = new Map();
    {
      // 按页取时手上只有最近几页：从前面那些回合的合计接着算。
      let total = state.turnsBase?.total || 0;
      let prompt = state.turnsBase?.prompt || 0;
      let cached = state.turnsBase?.cached || 0;
      for (const turn of state.turns) {
        total += asFiniteNumber(turn?.token_total);
        prompt += asFiniteNumber(turn?.token_prompt);
        cached += asFiniteNumber(turn?.token_cache_read);
        state.cumulativeByTurn.set(String(turn?.id || ""), { total, prompt, cached });
      }
    }
    // 刷新/切会话后,输入框下方信息行按最后一轮回填(速度 + 累计),不然刷新就空了(#99)。
    {
      const lastTurn = state.turns[state.turns.length - 1];
      const lastCum = lastTurn ? state.cumulativeByTurn.get(String(lastTurn.id || "")) : null;
      // 回填给「累计」定基线,重连后若正跑子代理,refreshComposerCumulative 有基线可加。
      // 但**只在没有基线、或候选更高时才用它**:按落库回合求和会漏算子代理子会话,每秒
      // 轮询若照它下调,会把实时事件维护的、含子代理的权威累计压低——后台子代理跑完后
      // 累计瞬间掉一大块正是这么来的(#131)。可信度更高的 bootstrap 会话累计(含子代理)
      // 也纳入比较,取最高的当基线。/reset 等清零场景由 conversation.* 事件另行清基线。
      const cand = lastCum && lastCum.total > 0
        ? { total: lastCum.total, prompt: lastCum.prompt, cached: lastCum.cached }
        : null;
      const ctxTotal = asFiniteNumber(state.context?.cumulative_tokens);
      const ctx = ctxTotal > 0
        ? { total: ctxTotal, prompt: asFiniteNumber(state.context?.cumulative_prompt_tokens), cached: asFiniteNumber(state.context?.cumulative_cache_read_tokens) }
        : null;
      const best = [state.cumulativeBase, ctx, cand]
        .filter((c) => c && c.total > 0)
        .reduce((a, b) => (!a || b.total > a.total ? b : a), null);
      state.cumulativeBase = best;
      setComposerUsage({
        speed: lastTurn ? generationSpeedValue(lastTurn.generation_tokens, lastTurn.generation_ms) : null,
      });
      refreshComposerCumulative();
    }
    // 回合运行期间每秒轮询都可能整段重建（refreshViewSnapshot）。用户正往回
    // 翻历史时不能每秒被拽回底部：只有明确导航（换会话/启动）或用户本来就
    // 跟着输出走时才滚到底，否则原地恢复滚动位置。
    const keepScroll = !forceScroll && !state.followOutput;
    const previousScrollTop = elements.chatScroll.scrollTop;
    // replaceChildren 让 scrollHeight 瞬间塌掉,浏览器把 scrollTop 钳到 0 并派发
    // 一条 scroll 事件;这条事件先于下面的 rAF 到达监听器。不守卫的话监听器
    // 把「跳到顶」当成用户上滚,关掉跟随——后台任务完成的通知落库触发整段
    // 重建时就是这么把自动滚动弄丢的(之后 AI 继续输出也不再往下走)。
    armProgrammaticScroll();
    elements.timeline.replaceChildren();
    const turns = [...state.turns].sort((left, right) => asFiniteNumber(left?.seq) - asFiniteNumber(right?.seq));
    state.turns = turns;
    syncArtifactsFromTurns(turns);
    loadStageTodos(state.viewSessionId);
    loadGoal(state.viewSessionId);
    if (state.finishedTurnArticles.size) {
      const knownTurnIds = new Set(turns.map((turn) => String(turn?.id)));
      for (const [key, list] of [...state.finishedTurnArticles.entries()]) {
        // 别的会话离屏完成的存档不在本会话的 turns 里,不能因此被剪掉。
        const foreign = list.some((entry) => entry.sessionId && String(entry.sessionId) !== String(state.viewSessionId || ""));
        if (!foreign && !knownTurnIds.has(key)) state.finishedTurnArticles.delete(key);
      }
    }
    if (turns.length === 0) {
      elements.timeline.hidden = true;
      elements.emptyState.hidden = false;
    } else {
      elements.emptyState.hidden = true;
      elements.timeline.hidden = false;
      // 不再插日期分隔条：它在回执/流式气泡之间来回跳位置，信息量又低
      // （悬停消息时间戳就有完整日期）。
      for (const turn of turns) renderPersistedTurn(turn);
    }
    // 命令回执不是回合，不在 state.turns 里；timeline 每次重建都要补回来。
    window.MiyuCommands?.renderNotices(elements.timeline, state.viewSessionId);
    reattachLiveArticles();
    // 跨会话收到的消息并进紧跟着的那段 AI 回复(09-24 定的版式)。
    window.MiyuCrossSession?.foldInto(elements.timeline);
    // 落盘回合数为 0 不等于屏幕上没内容：回执和正在流式输出的气泡都不在
    // state.turns 里。只按 turns 判空的话，运行中一次重绘就把画面整个换成
    // 欢迎页，气泡瞬间蒸发。
    if (elements.timeline.childElementCount > 0) {
      elements.emptyState.hidden = true;
      elements.timeline.hidden = false;
    }
    if (keepScroll) {
      // 同步恢复（不等下一帧），重建就不会闪一下再跳回来。上方内容高度
      // 变化仍可能让视口偏移，先接受这个近似。
      armProgrammaticScroll();
      elements.chatScroll.scrollTop = previousScrollTop;
      state.nearBottom = isNearBottom();
      elements.jumpBottomButton.hidden = false;
    } else {
      state.nearBottom = true;
      state.followOutput = true;
      elements.jumpBottomButton.hidden = true;
      // 先同步钉到底:replaceChildren 之后 scrollTop 被钳成 0,只等下一帧再滚
      // 的话会画出一帧顶部——手机上每轮结束整段重建都闪一下(09-10 沙盒实测
      // 采样到 scrollTop 291→0→291)。rAF 那次是布局稳定后的最终校正。
      armProgrammaticScroll();
      elements.chatScroll.scrollTop = elements.chatScroll.scrollHeight;
      window.requestAnimationFrame(() => {
        armProgrammaticScroll();
        elements.chatScroll.scrollTop = elements.chatScroll.scrollHeight;
        // 重建前后都可能有 scroll 事件进监听器,跟随位在这里再钉一次。
        state.followOutput = true;
        state.nearBottom = true;
        elements.jumpBottomButton.hidden = true;
      });
    }
    updateConversationChrome();
  }

  /// 标记「接下来这次滚动是程序发起的」:监听器看到守卫就不把它当用户上滚。
  /// 非 smooth 滚动由紧随其后的那条 scroll 事件解除;没动(scrollTop 没变)
  /// 就不派发事件,靠超时兜底。
  function armProgrammaticScroll() {
    state.programmaticScroll = true;
    programmaticScrollSmooth = false;
    window.clearTimeout(programmaticScrollTimer);
    programmaticScrollTimer = window.setTimeout(() => {
      state.programmaticScroll = false;
    }, PROGRAMMATIC_SCROLL_AUTO_MS);
  }

  /// 把还在跑的 live 气泡挂回重建后的时间线。
  ///
  /// `renderConversation` 会 `replaceChildren()` 整段重建，而 live 气泡不在
  /// `state.turns` 里——重建之后它就脱离了文档，后续的 assistant.delta 全写
  /// 进一个看不见的节点，直到回合结束、那一轮作为持久化回合被画出来，内容才
  /// 整段冒出来。自己发消息时不会中途重画，所以这个洞只在 daemon 自己发起的
  /// 回合上露出来（目标续轮、后台任务唤醒）：它们的回合一落盘就触发重画。
  function reattachLiveArticles() {
    for (const live of state.liveRuns.values()) {
      if (!live.article || live.ended) continue;
      // 离屏保活的别会话气泡不能挂进当前时间线。
      if (!liveViewed(live)) continue;
      // 落库的 running 占位与直播气泡是同一轮:重挂前撤掉占位。
      removeRunningStatus(live.turnId);
      if (!live.article.isConnected) {
        elements.timeline.appendChild(live.article);
        pinQueuedMessages();
      }
      if (live.stopButton && !live.stopButton.isConnected) {
        elements.liveStopRail.appendChild(live.stopButton);
        elements.liveStopRail.hidden = false;
      }
      // 切走时被 clearQuestionDock 摘下的待答问题,切回原样归位。
      for (const question of live.questions?.values?.() || []) {
        if (question.pending && question.card && !question.card.isConnected) {
          elements.questionDock.appendChild(question.card);
        }
      }
    }
    updateQuestionDock();
    syncRunIndicator();
  }

  function createLiveState(runId, options = {}) {
    return {
      runId,
      // 归属会话:切走时离屏保活、切回按它过滤重挂(retireLiveRunsForSwitch)。
      sessionId: String(options.sessionId || state.viewSessionId || ""),
      turnId: options.turnId || null,
      userText: options.userText || "",
      userAttachments: Array.isArray(options.userAttachments) ? options.userAttachments : [],
      startedAt: options.startedAt || new Date(),
      userRendered: Boolean(options.userRendered),
      article: null,
      blocks: null,
      headerStatus: null,
      stopButton: null,
      cancellationRequested: false,
      meta: null,
      endpoint: null,
      copyButton: null,
      currentText: null,
      assistantText: "",
      assistantReasoning: "",
      assets: [],
      artifacts: [],
      reasoning: null,
      reasoningParts: [],
      reasoningStarted: false,
      reasoningTitle: "",
      reasoningTimer: null,
      providerId: "",
      model: "",
      tools: new Map(),
      preparingTool: null,
      questions: new Map(),
      contextOperation: null,
      typing: null,
      typingAnimation: null,
      streamRail: null,
      ended: false,
      operation: options.operation || "create",
      inputId: options.inputId || null,
      editedContent: options.editedContent ?? null,
      redoCommitted: false
    };
  }

  function isJobFollowupContent(content) {
    const raw = String(content || "");
    return isSyntheticTurnContent(raw);
  }

  // 排队的消息不再放输入框上方的托盘,直接画在对话末尾(用户气泡 + 「排队中」小签),
  // 就是它轮到时会出现的位置。这里按 state.queuedPrompts 同步时间线里的占位:
  // 少了的撤掉,多了的补上,顺序和位置(永远在最后)由 pinQueuedMessages 兜底。
  function renderQueueTray() {
    // 后台任务完成的自动跟进不是用户消息，不画。
    const prompts = (Array.isArray(state.queuedPrompts) ? state.queuedPrompts : [])
      .filter((prompt) => !isJobFollowupContent(prompt?.content) && !isJobFollowupContent(prompt?.display_content));
    if (elements.queueTray) {
      elements.queueTray.replaceChildren();
      elements.queueTray.hidden = true;
    }
    const ids = new Set(prompts.map((prompt) => String(prompt?.id)));
    for (const node of elements.timeline.querySelectorAll(".user-message.is-queued")) {
      if (!ids.has(String(node.dataset.queueId))) node.remove();
    }
    let added = null;
    for (const prompt of prompts) {
      const id = String(prompt?.id);
      if (queuedMessageNode(id)) continue;
      const node = createUserMessage(prompt?.content || "", prompt?.submitted_at || new Date(), {
        queued: true,
        queueId: id,
        attachments: prompt?.attachments
      });
      if (!node) continue;
      elements.timeline.appendChild(node);
      added = node;
    }
    pinQueuedMessages();
    if (added) contentAdded(added);
    updateControlState();
  }

  function queuedMessageNode(id) {
    for (const node of elements.timeline.querySelectorAll(".user-message.is-queued")) {
      if (String(node.dataset.queueId) === String(id)) return node;
    }
    return null;
  }

  // 排队占位永远贴在时间线末尾,按排队顺序:直播气泡后挂进来、回合重建之后都要再钉一次
  function pinQueuedMessages() {
    for (const prompt of Array.isArray(state.queuedPrompts) ? state.queuedPrompts : []) {
      const node = queuedMessageNode(prompt?.id);
      if (node && node !== elements.timeline.lastElementChild) elements.timeline.appendChild(node);
    }
  }

  async function removeQueuedPrompt(promptId) {
    if (!promptId) return;
    const target = activeTurnUpdateTarget(state.viewSessionId);
    if (!target) {
      showToast(t("无法确定排队消息所属的回复"), "error");
      return;
    }
    try {
      await apiRequest(`/api/runs/${encodeURIComponent(target.runId)}/turns/${encodeURIComponent(target.turnId)}/queue/${encodeURIComponent(promptId)}`, { method: "DELETE" });
      state.queuedPrompts = state.queuedPrompts.filter((prompt) => String(prompt?.id) !== String(promptId));
      renderQueueTray();
    } catch (error) {
      showToast(error.message || t("排队消息移除失败"), "error");
      if (error.status === 404 && state.viewSessionId) await loadSessionView(state.viewSessionId, { quiet: true });
    }
  }

  function disposeLiveState(live) {
    if (!live) return;
    for (const question of live.questions?.values?.() || []) {
      if (question.autoAdvanceTimer) window.clearTimeout(question.autoAdvanceTimer);
      question.autoAdvanceTimer = null;
    }
    clearPreparingTool(live);
    removeLiveStopButton(live);
    live.typingAnimation?.cancel();
    live.typingAnimation = null;
    if (live.reasoningTimer) {
      window.clearInterval(live.reasoningTimer);
      live.reasoningTimer = null;
    }
    if (live.currentText?.renderFrame) {
      window.cancelAnimationFrame(live.currentText.renderFrame);
      live.currentText.renderFrame = null;
    }
    for (const tool of live.tools?.values?.() || []) {
      if (tool.collapseTimer) window.clearTimeout(tool.collapseTimer);
      tool.collapseTimer = null;
      if (tool.outputRenderFrame) window.cancelAnimationFrame(tool.outputRenderFrame);
      tool.outputRenderFrame = null;
    }
  }

  function ensureTimelineVisible() {
    elements.loadingState.hidden = true;
    elements.blockedState.hidden = true;
    elements.emptyState.hidden = true;
    elements.timeline.hidden = false;
  }

  function ensureLiveUser(live, content) {
    if (!live || live.userRendered) return;
    // 离屏 live 不往当前时间线插用户消息;切回时落库回合会带上它。
    if (!liveViewed(live)) return;
    const text = String(content || live.userText || "");
    if (!text.trim() && !live.userAttachments.length) return;
    live.userText = text;
    ensureTimelineVisible();
    const message = createUserMessage(text, new Date(), {
      runId: live.runId,
      attachments: live.userAttachments
    });
    // 目标续轮等合成内容不画用户气泡(createUserMessage 返回 null),别的
    // 调用点都走 appendUserMessage 的空值兜底,这里以前直接 appendChild(null)
    // 抛 TypeError,把整段 live 装配掐断。
    if (message) {
      if (live.article?.isConnected) elements.timeline.insertBefore(message, live.article);
      else elements.timeline.appendChild(message);
      window.MiyuCrossSession?.foldInto(elements.timeline);
    }
    live.userRendered = true;
    updateConversationChrome();
    contentAdded();
  }

  function removeRunningStatus(turnId) {
    if (!turnId) return;
    const status = Array.from(elements.timeline.querySelectorAll("[data-turn-status]"))
      .find((node) => node.dataset.turnStatus === String(turnId));
    status?.remove();
  }

  /// 中断落定后在原位补一条「本轮已中断」状态行,取代整会话静默重拉。
  /// 后端实测 cancel→run.cancelled 仅 ~12ms,之前那次 loadSessionView 把整条
  /// 对话全量重渲染才是中断「不是秒停 / 感觉加载很久」的真因;这里只动这一条。
  function showInterruptedMarker(turnId, article) {
    const id = String(turnId || "");
    removeRunningStatus(turnId);
    const turn = (id && state.turns.find((item) => String(item?.id) === id)) || { id, status: "interrupted" };
    const line = createTurnStatus({ ...turn, status: "interrupted" });
    let anchor = article && article.isConnected ? article : null;
    if (!anchor && id) {
      const nodes = Array.from(elements.timeline.querySelectorAll(`[data-turn-id="${CSS.escape(id)}"]`));
      anchor = nodes.length ? nodes[nodes.length - 1] : null;
    }
    if (anchor?.parentNode) anchor.parentNode.insertBefore(line, anchor.nextSibling);
    else elements.timeline.appendChild(line);
  }

  function commitRedoLive(live) {
    if (!live || live.operation !== "redo" || live.redoCommitted) return;
    live.redoCommitted = true;
    closeRevisionEditor();
    const stashKey = String(live.turnId || "");
    const previousStash = state.finishedTurnArticles.get(stashKey) || [];
    for (const entry of previousStash) {
      if (entry.kind === "final") entry.article?.remove();
    }
    const prefixSegments = previousStash.filter((entry) => entry.kind === "segment");
    if (prefixSegments.length) state.finishedTurnArticles.set(stashKey, prefixSegments);
    else state.finishedTurnArticles.delete(stashKey);
    for (const article of elements.timeline.querySelectorAll(".assistant-message")) {
      if (article.dataset.turnId === String(live.turnId || "") && article.dataset.segmentKind === "final") {
        article.remove();
      }
    }
    removeRunningStatus(live.turnId);
    if (live.inputId && live.editedContent != null) {
      const user = Array.from(elements.timeline.querySelectorAll(".user-message"))
        .find((article) => article.dataset.inputId === String(live.inputId));
      const paragraph = user?.querySelector(".user-bubble p");
      if (paragraph) paragraph.textContent = String(live.editedContent);
    }
    const turn = state.turns.find((item) => String(item?.id) === String(live.turnId));
    if (turn) {
      turn.status = "running";
      turn.assistant_content = "";
      turn.assistant_reasoning = null;
    }
    showTypingIndicator(live);
  }

  function createTypingIndicator() {
    // AI 输出的「加载中」用编排点动效(用户拍板):三点走三角·顺时针→聚合→三角→
    // 逆时针→聚合→三角,6s 循环;没有「水平一行」这个姿态(用户 09-18)。
    // 输入框那份仍是旧的匀速三点。
    const indicator = document.createElement("div");
    indicator.className = "miyu-run typing-run";
    indicator.setAttribute("aria-hidden", "true");
    const spin = document.createElement("span");
    spin.className = "mr-spin";
    for (const cls of ["mr1", "mr2", "mr3"]) {
      const dot = document.createElement("i");
      dot.className = cls;
      spin.appendChild(dot);
    }
    indicator.appendChild(spin);
    return indicator;
  }

  /* 运行指示器挪到了输入框那一排（`composerRunIndicator`）：气泡内那份只在
     「第一个块到达前」出现（`childElementCount > 0` 就直接 return），推理块或
     工具卡一出来就没了——而那两个阶段恰恰是最需要「它还在动」的时候。
     现在由回合状态统一驱动，见 `syncRunIndicator`。 */
  function showTypingIndicator(live) {
    if (!live || live.ended) return;
    ensureLiveArticle(live);
    syncRunIndicator();
    // 气泡里这份只管「还没开口」这一段:等待期给个落点,不然气泡是空的。
    // 整个回合期间的指示由输入框那排负责(推理、工具阶段它也在转)。
    if (live.typing || live.blocks.childElementCount > 0) return;
    const indicator = createTypingIndicator();
    live.blocks.appendChild(indicator);
    live.typing = indicator;
    contentAdded(live);
  }

  // 只要这个视图里有回合在跑就转，与是正文、推理还是工具无关。
  function syncRunIndicator() {
    syncTurnClock();
    const indicator = elements.composerRunIndicator;
    if (!indicator) return;
    indicator.hidden = !conversationRunning();
  }

  /// 输入框下面那排最右边的「用时」(用户 09-24):这个视图里正在跑的那一轮从它开始
  /// (中途打开页面时取库里的 user_timestamp)算起;跑完就不显示,和终端一个规矩。
  function syncTurnClock() {
    let running = null;
    for (const live of state.liveRuns.values()) {
      if (!live.ended && liveViewed(live)) running = live;
    }
    if (!running) {
      state.turnClock = null;
    } else if (!state.turnClock || state.turnClock.runId !== running.runId) {
      const startedAt = new Date(running.startedAt).getTime();
      state.turnClock = {
        runId: running.runId,
        startedAt: Number.isFinite(startedAt) ? startedAt : Date.now()
      };
    }
    renderTurnClock();
  }

  function renderTurnClock() {
    const element = elements.composerTimer;
    if (!element) return;
    const clock = state.turnClock;
    element.hidden = !clock;
    if (!clock) return;
    elements.composerTimerValue.textContent = formatHms((Date.now() - clock.startedAt) / 1000);
  }

  // 三点已挪到输入框那排，这里只保留 `is-streaming` 状态位（正文流式时的
  // 样式还靠它），不再往气泡里塞节点、也不再做那段位移补间。
  function promoteTypingIndicator(live) {
    if (!live || live.ended) return;
    ensureLiveArticle(live);
    // 开口了就撤掉气泡里那份等待动画,它的语义只有「还没开口」。
    if (live.typing) {
      live.typing.remove();
      live.typing = null;
    }
    live.article.classList.add("is-streaming");
    syncRunIndicator();
  }

  function clearTypingIndicator(live, { waitingOnly = false } = {}) {
    if (!live) return;
    // 气泡里那份是「还没开口」的占位，有任何内容落进来就撤。
    if (live.typing) {
      live.typing.remove();
      live.typing = null;
    }
    if (waitingOnly) {
      syncRunIndicator();
      return;
    }
    if (live.streamRail) live.streamRail.hidden = true;
    live.article?.classList.remove("is-streaming");
    syncRunIndicator();
  }

  /* 完成态保时序:live 渲染出的 article 按 turn 存档,重渲染时原样复用 */
  function stashLiveArticle(live, kind) {
    if (!live?.article) return;
    clearTypingIndicator(live);
    if (!live.turnId) return;
    if (!live.blocks || live.blocks.childElementCount === 0) return;
    live.article.classList.remove("live-assistant");
    live.article.dataset.segmentKind = kind;
    const key = String(live.turnId);
    const list = state.finishedTurnArticles.get(key) || [];
    // sessionId 随存:重建时的清理只能剪本会话的存档(离屏完成的轮要留到
    // 用户切回它的会话时复用)。
    list.push({ kind, article: live.article, sessionId: live.sessionId || "" });
    state.finishedTurnArticles.set(key, list);
  }

  function updateLiveStopButton(live) {
    if (!live.stopButton) return;
    live.stopButton.disabled = live.ended || live.cancellationRequested;
    live.stopButton.title = live.cancellationRequested ? t("正在停止") : t("停止本条回复");
    live.stopButton.setAttribute("aria-label", live.stopButton.title);
  }

  function removeLiveStopButton(live) {
    if (!live.stopButton) return;
    live.stopButton.remove();
    live.stopButton = null;
    elements.liveStopRail.hidden = elements.liveStopRail.childElementCount === 0;
  }

  async function cancelLiveRun(live) {
    if (!live || live.ended || live.cancellationRequested) return;
    live.cancellationRequested = true;
    updateLiveStopButton(live);
    if (live.headerStatus) live.headerStatus.textContent = t("正在停止");
    try {
      await apiRequest(`/api/runs/${encodeURIComponent(live.runId)}/cancel`, { method: "POST" });
    } catch (error) {
      live.cancellationRequested = false;
      updateLiveStopButton(live);
      if (live.headerStatus && !live.ended) live.headerStatus.textContent = t("正在回复");
      showToast(error.message || t("停止失败"), "error");
      if ((error.status === 404 || error.status === 409) && state.viewSessionId) {
        await loadSessionView(state.viewSessionId, { quiet: true });
      }
    }
  }

  // 普通 Markdown 随内容收缩；只有需要稳定横向空间的结构撑满消息列。
  // .image-gen-bubble 必须算宽块:纯生图回合没有其他宽内容,漏掉它气泡
  // 会收缩成 fit-content,占位方块的 70% 宽随之塌成一丁点(08-25 实录)。
  const WIDE_BLOCK_SELECTOR = ".markdown-body pre, .markdown-table-scroll, .conversation-media, .context-operation, img, .image-gen-bubble, .tool-card:not(.collapsed), .tool-live-progress:not([hidden])";
  function syncBubbleWidth(article) {
    if (!article) return;
    const content = article.querySelector(".assistant-content");
    if (!content) return;
    content.classList.toggle("is-slim", !content.querySelector(WIDE_BLOCK_SELECTOR));
  }

  function ensureLiveArticle(live) {
    if (live.article) return live.article;
    // 离屏 live 的气泡建成游离节点继续吃事件,切回时 reattach 挂载。
    const viewed = liveViewed(live);
    if (viewed) {
      ensureTimelineVisible();
      ensureLiveUser(live, live.userText);
      removeRunningStatus(live.turnId);
    }
    const article = document.createElement("article");
    article.className = "message assistant-message live-assistant";
    article.dataset.role = "assistant";
    article.dataset.runId = live.runId;
    if (live.turnId) article.dataset.turnId = String(live.turnId);
    const header = document.createElement("header");
    header.className = "assistant-label";
    const avatar = document.createElement("img");
    avatar.alt = "";
    avatar.setAttribute("aria-hidden", "true");
    setPersonaAvatar(avatar);
    const identity = document.createElement("div");
    const name = document.createElement("strong");
    name.textContent = state.persona.name;
    const status = document.createElement("span");
    status.className = "live-indicator";
    // 直播状态由三点弹跳/思考签表达,header 不再写「正在回复」;完成后写「刚刚」等
    status.textContent = "";
    identity.append(name, status);
    // Each running reply owns a compact stop control in its bubble corner.
    const stop = document.createElement("button");
    stop.type = "button";
    stop.className = "live-stop-button";
    stop.dataset.runId = live.runId;
    stop.appendChild(makeIconSlot("stop-square"));
    stop.addEventListener("click", () => cancelLiveRun(live));
    header.append(avatar, identity);
    if (viewed) {
      for (const existing of elements.liveStopRail.querySelectorAll(".live-stop-button")) {
        if (existing.dataset.runId === live.runId) existing.remove();
      }
      elements.liveStopRail.appendChild(stop);
      elements.liveStopRail.hidden = false;
    }
    const assistantContent = document.createElement("div");
    assistantContent.className = "assistant-content is-slim";
    const blocks = document.createElement("div");
    blocks.className = "assistant-blocks";
    assistantContent.appendChild(blocks);
    const bubble = document.createElement("div");
    bubble.className = "assistant-bubble";
    bubble.appendChild(assistantContent);
    const meta = document.createElement("div");
    meta.className = "assistant-meta";
    const endpoint = document.createElement("span");
    endpoint.className = "assistant-endpoint";
    endpoint.hidden = true;
    const metaText = document.createElement("span");
    metaText.textContent = "";
    const spacer = document.createElement("span");
    spacer.className = "meta-spacer";
    const copy = makeCopyButton(() => live.assistantText, t("复制回复"));
    copy.hidden = true;
    meta.append(endpoint, metaText, spacer, copy);
    const streamRail = document.createElement("div");
    streamRail.className = "assistant-stream-rail";
    streamRail.hidden = true;
    article.append(header, bubble, meta, streamRail);
    if (viewed) {
      elements.timeline.appendChild(article);
      pinQueuedMessages();
    }
    live.article = article;
    live.blocks = blocks;
    // 刚收到的跨会话消息挪进这段回复的最前面(09-24 定的版式)。
    if (viewed) window.MiyuCrossSession?.foldInto(elements.timeline);
    live.headerStatus = status;
    live.stopButton = stop;
    live.meta = metaText;
    live.endpoint = endpoint;
    live.copyButton = copy;
    live.streamRail = streamRail;
    updateLiveStopButton(live);
    contentAdded(live);
    return article;
  }

  function breakLiveText(live) {
    live.currentText = null;
  }

  /// 流式渲染时把没闭合的行内标记先补上:模型正在输出 `` `sudo pacman -Syu` ``,
  /// 闭合反引号没到之前整段按普通文字排版,一到就换成代码样式——每次这么
  /// 一换,那一行前后的字全部重排,看起来就是「字在跳」(09-10 沙盒逐帧取证)。
  /// 只补三样:未闭合的围栏代码块、行内反引号、`**` 粗体;单个 `*`/`_` 与列表
  /// 和数学冲突,不碰。回合结束后按落库原文重画,这里的补丁不进任何存档。
  function stabilizeStreamingMarkdown(raw) {
    const text = String(raw || "");
    if (!text) return text;
    const lines = text.split("\n");
    let fenceOpen = false;
    let tailStart = 0;
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index];
      if (/^\s*(```|~~~)/.test(line)) {
        fenceOpen = !fenceOpen;
        // 围栏一关,尾巴从它后面算:围栏里的反引号不参与行内配对
        if (!fenceOpen) tailStart = index + 1;
      } else if (!fenceOpen && !line.trim()) tailStart = index + 1;
    }
    if (fenceOpen) return `${text}\n\`\`\``;
    const tail = lines.slice(tailStart).join("\n");
    let patched = text;
    const backticks = (tail.match(/`/g) || []).length;
    if (backticks % 2 === 1) patched += "`";
    const bolds = (tail.match(/\*\*/g) || []).length;
    if (bolds % 2 === 1) patched += "**";
    return patched;
  }

  function scheduleMarkdownRender(block) {
    if (block.renderFrame) return;
    block.renderFrame = window.requestAnimationFrame(() => {
      block.renderFrame = null;
      renderMarkdown(block.element, stabilizeStreamingMarkdown(block.raw));
      contentAdded(block.element);
    });
  }

  function appendAssistantDelta(live, delta) {
    const text = String(delta || "");
    if (!text) return;
    ensureLiveArticle(live);
    const startsText = !live.currentText;
    if (!live.currentText) {
      finalizeLiveReasoning(live);
      const element = document.createElement("div");
      element.className = "markdown-body live-text-block";
      const block = { element, raw: "", renderFrame: null };
      procLineBreak(live.blocks);
      live.blocks.appendChild(element);
      syncBubbleWidth(live.article);
      live.currentText = block;
      live.contextOperation = null;
      if (live.assistantText && !/\s$/.test(live.assistantText)) live.assistantText += "\n\n";
    }
    live.currentText.raw += text;
    live.assistantText += text;
    live.copyButton.hidden = !live.assistantText.trim();
    if (startsText) {
      renderMarkdown(live.currentText.element, stabilizeStreamingMarkdown(live.currentText.raw));
      promoteTypingIndicator(live);
    } else {
      scheduleMarkdownRender(live.currentText);
    }
    contentAdded(live);
  }

  function resetSupersededGeneration(live) {
    if (live.currentText?.renderFrame) window.cancelAnimationFrame(live.currentText.renderFrame);
    live.currentText?.element?.remove();
    live.currentText = null;
    for (const reasoning of live.reasoningParts || []) reasoning.element?.remove();
    if (live.reasoningTimer) window.clearInterval(live.reasoningTimer);
    live.reasoningTimer = null;
    live.reasoning = null;
    live.reasoningParts = [];
    live.reasoningStarted = false;
    live.reasoningTitle = "";
    live.reasoningClockStart = null;
    live.assistantText = "";
    live.assistantReasoning = "";
    if (live.copyButton) live.copyButton.hidden = true;
    clearTypingIndicator(live);
    showTypingIndicator(live);
  }

  function ensureLiveReasoning(live) {
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    if (live.reasoning) return live.reasoning;
    breakLiveText(live);
    live.contextOperation = null;
    const reasoning = createReasoningBlock("", t("正在思考"), true, false, true);
    // 计时从 reasoning.start 事件算起,而不是签出现的时刻(签是惰性创建的)
    if (live.reasoningClockStart != null) reasoning.startedAt = live.reasoningClockStart;
    reasoning.pendingTitle = normalizeReasoningTitle(live.reasoningTitle);
    if (!reasoningHidden()) procLineAttach(live.blocks, reasoning.element);
    live.reasoning = reasoning;
    live.reasoningParts.push(reasoning);
    if (live.reasoningTimer) window.clearInterval(live.reasoningTimer);
    const updateProgress = () => {
      if (!reasoning.liveStatus || reasoning.startedAt == null) return;
      const elapsed = Math.max(0, Math.floor((performance.now() - reasoning.startedAt) / 1000));
      reasoning.liveStatus.textContent = `${elapsed}s`;
    };
    updateProgress();
    live.reasoningTimer = window.setInterval(updateProgress, 1000);
    return reasoning;
  }

  function collectLiveReasoning(live) {
    return (live.reasoningParts || [])
      .map((part) => String(part.raw || "").trim())
      .filter(Boolean)
      .join("\n\n");
  }

  function finalizeLiveReasoning(live) {
    const reasoning = live.reasoning;
    if (!reasoning) return;
    if (live.reasoningTimer) {
      window.clearInterval(live.reasoningTimer);
      live.reasoningTimer = null;
    }
    const parsed = splitReasoningText(reasoning.raw);
    const title = t("已思考");
    reasoning.raw = parsed.body;
    reasoning.finished = true;
    if (!reasoning.raw.trim() && title === t("已思考")) {
      reasoning.element.remove();
    } else {
      reasoning.element.classList.remove("is-live");
      reasoning.element.classList.remove("has-window");
      reasoning.title.textContent = title;
      reasoning.body.textContent = reasoning.raw;
      if (reasoning.progress) reasoning.progress.remove();
      if (reasoning.window) {
        reasoning.window.remove();
        reasoning.window = null;
      }
      if (reasoning.liveStatus) {
        if (reasoning.startedAt != null) {
          reasoning.liveStatus.textContent = `${((performance.now() - reasoning.startedAt) / 1000).toFixed(1)}s`;
        } else {
          reasoning.liveStatus.remove();
        }
      }
    }
    live.reasoning = null;
    live.reasoningTitle = "";
    live.reasoningStarted = false;
    live.reasoningClockStart = null;
    live.assistantReasoning = collectLiveReasoning(live);
  }

  function handleReasoningEvent(name, live, data) {
    if (name === "reasoning.start" || name === "reasoning.part_start") {
      // 惰性创建:只记状态,签等第一段真实思考文本(reasoning.delta)到达才出现,
      // 避免不输出思考的模型挂着空的「正在思考」签和空面板
      finalizeLiveReasoning(live);
      resetPreparingWindow(live);
      live.reasoningStarted = true;
      live.reasoningClockStart = performance.now();
      breakLiveText(live);
      return;
    }
    if (name === "reasoning.reset") {
      if (live.reasoning) {
        live.reasoning.raw = "";
        live.reasoning.body.textContent = "";
        live.reasoning.pendingTitle = "";
        setReasoningWindow(live.reasoning, "");
      }
      return;
    }
    if (name === "reasoning.title") {
      live.reasoningTitle = String(data?.title || "").trim();
      // 只更新已存在的签;没有思考文本就不为标题单独建签
      if (live.reasoning) live.reasoning.pendingTitle = normalizeReasoningTitle(live.reasoningTitle);
      return;
    }
    if (name === "reasoning.delta") {
      const delta = String(data?.delta || "");
      if (!delta) return;
      if (!live.reasoning && !delta.trim()) return;
      const reasoning = ensureLiveReasoning(live);
      reasoning.raw += delta;
      reasoning.body.textContent = reasoning.raw;
      // 窥视槽只放尾巴:换行折成空格,取最后 160 字,够撑满一行还不至于每个 delta 都重排一大段
      setReasoningPeek(reasoning.peek, reasoning.raw);
      setReasoningWindow(reasoning, reasoning.raw);
      live.assistantReasoning = collectLiveReasoning(live);
      contentAdded(live);
      return;
    }
    if (name === "reasoning.part_end") {
      finalizeLiveReasoning(live);
    }
  }

  function prettyArguments(value) {
    if (value == null) return "";
    let obj = value;
    if (typeof value === "string") {
      const trimmed = value.trim();
      if (!trimmed) return "";
      try {
        obj = JSON.parse(trimmed);
      } catch (_) {
        return value;
      }
    }
    if (obj == null) return "";
    // 顶层对象 → 「键：值」逐行,不再是裹着大括号的裸 JSON(09-12 #6:工具展开
    // 信息不该是裸 json)。嵌套值压成一行紧凑 JSON;非对象/数组回退 pretty JSON。
    if (typeof obj !== "object" || Array.isArray(obj)) {
      try {
        return JSON.stringify(obj, null, 2);
      } catch (_) {
        return String(obj);
      }
    }
    const lines = [];
    for (const [key, raw] of Object.entries(obj)) {
      let rendered;
      if (raw == null) rendered = "";
      else if (typeof raw === "object") {
        try { rendered = JSON.stringify(raw); } catch (_) { rendered = String(raw); }
      } else rendered = String(raw);
      lines.push(`${key}: ${rendered}`);
    }
    return lines.join("\n");
  }

  // 子代理事件名后端格式化成 `subagent:<描述>`(让并行子代理各有独立事件名,
  // 见 agent::reports::tool_event_name),所以判定/取图标要认这个前缀,不能只比
  // 精确名——否则子代理工具行认不出来,窥视/子过程时间线整套都不触发(09-11
  // 用户报「气泡还在、渲染不对」的真因,Playwright 实测揪出)。
  function subagentToolBaseName(name) {
    const n = String(name || "");
    const at = n.search(/[:：]/);
    return at >= 0 ? n.slice(0, at) : n;
  }
  function isSubagentTool(name) {
    const base = subagentToolBaseName(name);
    return base === "subagent" || base === "task";
  }

  function parsedToolArguments(value) {
    if (value && typeof value === "object" && !Array.isArray(value)) return value;
    if (typeof value !== "string" || !value.trim()) return {};
    try {
      const parsed = JSON.parse(value);
      return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
    } catch (_) {
      return {};
    }
  }

  function compactLine(value, limit = 92) {
    const line = String(value || "").replace(/\s+/g, " ").trim();
    if (line.length <= limit) return line;
    return `${line.slice(0, Math.max(1, limit - 1))}…`;
  }

  function compactPath(value) {
    const path = String(value || "").trim();
    if (!path) return "";
    return path.split(/[\\/]/).filter(Boolean).pop() || path;
  }

  function toolSubject(name, value) {
    const args = parsedToolArguments(value);
    const toolName = String(name || "");
    if (toolName === "run_command" || toolName === "Bash") {
      const line = compactLine(args.command || args.cmd);
      const background = args.background === true || args.run_in_background === true;
      return background ? t("[后台] {command}", {command: line}) : line;
    }
    if (toolName === "read" || toolName === "read_file") {
      const path = compactPath(args.path);
      const offset = Number.isFinite(Number(args.offset)) && args.offset != null ? Number(args.offset) : null;
      const limit = Number.isFinite(Number(args.limit)) && args.limit != null ? Number(args.limit) : null;
      if (offset === null && limit === null) return path;
      const start = Math.max(offset ?? 1, 1);
      const page = limit !== null ? `L${start}-${start + limit - 1}` : `L${start}+`;
      return path ? `${path} (${page})` : page;
    }
    if (["edit", "artifact", "kb", "apply_patch", "apply_artifact_patch"].includes(toolName)) {
      // 唯一编辑器:patchText 里抠出文件名当副标题,不然标签恒空。
      const text = String(args.patchText || args.patch_text || "");
      const files = [...text.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)].map((m) => m[1].trim());
      if (files.length === 1) return compactPath(files[0]);
      if (files.length > 1) return t("{first} 等 {count} 个文件", {first: compactPath(files[0]), count: files.length});
      return "";
    }
    if (["read", "write", "edit", "print_image", "vision_analyze"].includes(toolName)) {
      return compactPath(args.filePath || args.file_path || args.path || args.image);
    }
    if (toolName === "grep") {
      const target = compactPath(args.path);
      return compactLine(`${args.pattern || ""}${target ? ` · ${target}` : ""}`);
    }
    if (toolName === "glob") return compactLine(`${args.pattern || ""}${args.path ? ` · ${compactPath(args.path)}` : ""}`);
    if (toolName === "todowrite" || toolName === "todoupdate") {
      // 清单本体画在任务卡片里，这里只说这一下动了几项。
      if (Array.isArray(args.todos)) return t("{count} 项", {count: args.todos.length});
      if (Array.isArray(args.updates)) return t("更新 {count} 项", {count: args.updates.length});
      return "";
    }
    if (["webfetch", "web_fetch"].includes(toolName)) return compactLine(args.url);
    if (["web_search", "search_web", "search_web_images"].includes(toolName)) return compactLine(args.query || args.q);
    if (toolName === "generate_image") return compactLine(args.prompt);
    if (isSubagentTool(toolName)) return compactLine(args.description || args.prompt);
    if (toolName === "load_skill") return compactLine(args.name);
    if (window.MiyuCrossSession?.isSendTool(toolName)) {
      const target = findSession(String(args.session_id || ""));
      return window.MiyuCrossSession.sendSubject(args, target ? sessionDisplayName(target) : "");
    }
    const preferred = ["query", "command", "path", "filePath", "url", "name", "id", "target"];
    for (const key of preferred) {
      if (typeof args[key] === "string" && args[key].trim()) return compactLine(args[key]);
    }
    return "";
  }

  function formatToolDuration(milliseconds) {
    if (!Number.isFinite(milliseconds) || milliseconds < 0) return "";
    if (milliseconds < 1_000) return `${Math.max(1, Math.round(milliseconds))} ms`;
    if (milliseconds < 10_000) return `${(milliseconds / 1_000).toFixed(1)} s`;
    return `${Math.round(milliseconds / 1_000)} s`;
  }

  // 主题与工具显示名共享 ≥6 字符前缀时去重(如「Linux 游戏兼容性调查」+「Linux 游戏兼容性: xxx」)
  function dedupeToolSubject(title, subject) {
    const t = String(title || "").trim();
    const s = String(subject || "").trim();
    if (!t || !s) return s;
    let i = 0;
    while (i < t.length && i < s.length && t[i] === s[i]) i += 1;
    if (i < 6) return s;
    const rest = s.slice(i).replace(/^[\s:：·,，、-]+/, "");
    return rest || s;
  }

  function commandPreviewBudget() {
    return Math.max(0, Number(state.display?.command_output_lines ?? 8) || 0);
  }

  /** 命令抬头底下露几行命令本身，跟 `display.command_output_lines`（默认 8）。
   *
   * 这个数是**屏幕上占几行**，不是「命令有几行」：一条很长的单行命令会软换行
   * 把这几行空间用满（用户 09-20），而不是截成一行加省略号。取源码时按逻辑行
   * 切到同样的数就够了——每条逻辑行至少占一行，多切也填不进去。
   *
   * 留头不留尾：超出的在**底部**换成一个 `⋮`。前几行才说明这条命令在干什么，
   * 尾巴通常是重定向和管道。0 = 一行都不露。
   */
  function commandPreviewRows(text) {
    const max = commandPreviewBudget();
    const lines = String(text || "").replace(/\s+$/, "").split("\n");
    if (max === 0 || (lines.length === 1 && !lines[0])) return { lines: [], omitted: false };
    if (lines.length <= max) return { lines, omitted: false };
    return { lines: lines.slice(0, max), omitted: true };
  }

  /** 裁到 `max` 个**可见**行；裁着了就让出最后一行给 `⋮`。
   *
   * 只能量出来，算不出来——软换行占几行取决于卡片当时多宽。宽度变了要重量，
   * 所以下面挂了 ResizeObserver；只在**宽度**真的变了才重跑，不然改高度这件事
   * 自己会把观察器再触发一遍，绕不出来。
   */
  function clipCommandPreview(tool) {
    const host = tool.commandPreview;
    if (!host || host.hidden) return;
    const max = commandPreviewBudget();
    if (max <= 0) return;
    host.style.setProperty("--command-rows", String(max));
    const clipped = host.scrollHeight > host.clientHeight + 1;
    const more = clipped || tool.commandOmitted;
    if (more && max > 1) host.style.setProperty("--command-rows", String(max - 1));
    if (tool.commandMore) tool.commandMore.hidden = !more;
    // 行数一定，连线的终点跟着变（末尾那一步的线要盖到命令行底下）。
    railSnapFit(host);
  }

  function watchCommandPreviewWidth(tool) {
    if (tool.commandWidthWatcher || !tool.commandPreview || typeof ResizeObserver === "undefined") {
      return;
    }
    let lastWidth = -1;
    tool.commandWidthWatcher = new ResizeObserver((entries) => {
      const width = Math.round(entries[0]?.contentRect?.width ?? 0);
      if (width === lastWidth) return;
      lastWidth = width;
      clipCommandPreview(tool);
    });
    tool.commandWidthWatcher.observe(tool.commandPreview);
  }

  /** 造出「命令那几行 + ⋮」这一对，并把点击/裁剪都接好。
   *
   * 实时那条路（`createTool`）和刷新重建那条路（`createPersistedToolCard`）**都要
   * 用它**。以前只改了实时那条，于是一刷新命令签就退回老样子：命令挤回抬头当窥视、
   * 底下没有命令行（用户 09-20）。
   */
  function buildCommandRows(commandText, head) {
    const preview = document.createElement("div");
    preview.className = "tool-command-preview";
    preview.setAttribute("aria-label", t("命令"));
    const more = document.createElement("span");
    more.className = "tool-command-more";
    more.textContent = "⋮";
    more.title = t("还有，点开看完整命令");
    more.hidden = true;
    // 命令那几行在 `.tool-head`（button）**外面**，不接就只有抬头那一条窄缝可点，
    // 而视线正落在命令上。拖着选文字不算点：有选区就放过，否则复制命令会顺手把
    // 卡片折起来。
    const expandOnClick = (event) => {
      if (String(window.getSelection?.() || "").length) return;
      event.preventDefault();
      head.click();
    };
    for (const node of [preview, more]) {
      node.addEventListener("click", expandOnClick);
    }
    const handle = { commandPreview: preview, commandMore: more, commandText };
    paintCommandPreview(handle);
    return { preview, more, handle };
  }

  /** 第一块可见详情不带分界线，后面每块都带。`root` 是放详情的那层容器。 */
  function markFirstDetail(root) {
    let first = true;
    for (const detail of root.querySelectorAll(".tool-detail")) {
      const visible = !detail.hidden;
      detail.classList.toggle("is-first-detail", visible && first);
      if (visible) first = false;
    }
  }

  function paintCommandPreview(tool) {
    const host = tool.commandPreview;
    if (!host) return;
    const { lines, omitted } = commandPreviewRows(tool.commandText);
    tool.commandOmitted = omitted;
    host.textContent = "";
    for (const line of lines) {
      const row = document.createElement("span");
      row.className = "tool-command-line";
      // 空行也得占一行高，否则命令里的空行会让行数对不上。
      row.textContent = line || " ";
      host.appendChild(row);
    }
    host.hidden = lines.length === 0;
    if (tool.commandMore) tool.commandMore.hidden = true;
    if (host.hidden) return;
    // 量高度得等这一帧排完版。
    requestAnimationFrame(() => {
      clipCommandPreview(tool);
      watchCommandPreviewWidth(tool);
    });
  }

  /** 展开态里，除第一块可见详情外都带一道分界线。
   *
   * CSS 选不出「前一个**可见**兄弟」——中间那几块（进度／命令输出／错误输出）
   * 是 `hidden` 的，相邻选择器照样匹配它们，所以第一块由这儿打标记。
   */
  function refreshDetailDividers(tool) {
    markFirstDetail(tool.card);
  }

  function updateToolSummary(tool) {
    const details = [];
    const subject = dedupeToolSubject(tool.titleText, tool.subject);
    refreshDetailDividers(tool);
    if (tool.commandPreview) {
      paintCommandPreview(tool);
      // 抬头右边给 short_title；没填就空着，不退回塞命令文本——命令就在下面。
      tool.summary.textContent = tool.commandTitle || "";
      return;
    }
    if (subject) details.push(subject);
    if (tool.imageCount) details.push(t("{count} 张图片", {count: tool.imageCount}));
    // 没有主语就空着:「无输出 / 等待输出」是旧芯片时代占摘要位的话,时间线上耗时和转圈
    // 都在状态位,这里再写字只会让人以为工具真的没输出。
    tool.summary.textContent = details.filter(Boolean).join(" · ");
  }

  function scrollToolOutputToEnd(tool) {
    for (const detail of [tool.stdoutDetail, tool.stderrDetail, tool.resultDetail]) {
      if (!detail.wrapper.hidden) detail.content.scrollTop = detail.content.scrollHeight;
    }
  }

  function boundedAppend(current, addition) {
    const combined = `${current || ""}${addition || ""}`;
    if (combined.length <= MAX_TOOL_OUTPUT_CHARS) return combined;
    return t("[较早输出已省略]\n{tail}", {tail: combined.slice(combined.length - MAX_TOOL_OUTPUT_CHARS)});
  }

  // 持久化回合里的工具卡片（只读）。
  //
  // 不复用 `createTool`：那个和实时流状态强耦合（往 live.tools 注册、跟踪
  // 分块输出、进度更新），拿持久化数据去喂它要伪造一个 live 对象，很脆。
  // 这里只画「调了什么、给了什么参数、返回了什么」，CSS 类沿用同一套，
  // 所以看起来和实时那份一致。
  //
  // 数据来自 `turn.tool_flow`，库里一直有——以前 API 不发，于是 WebUI 的
  // 工具信息只在事件流里活过一次，切走再回来就没了。
  function createPersistedToolCard(call) {
    const card = document.createElement("section");
    card.className = state.toolExpanded ? "tool-card" : "tool-card collapsed";
    const name = String(call?.name || "");
    card.dataset.toolName = name;
    if (COMMAND_TOOLS.includes(name)) card.classList.add("is-command");
    if (isSubagentTool(name)) card.classList.add("is-task");
    // 图标配色来自 is-success（金）/ is-failure（红）。两个都不加会退回默认色，
    // 看起来就是「颜色不对」。
    //
    // 成败没有单独落库，但也不需要：运行时那个 ok 本来就是从输出文本算的
    // （`tool_output_succeeded`：输出是 JSON 且 success/ok 为 false 才算失败，
    // 其余一律成功），这里照抄同一条规则，两边判定必然一致。
    // 成败由后端算好（`web::dto::tool_call_succeeded`）：规则有两条——硬失败
    // 看 `tool error:` 前缀，业务失败看输出 JSON 的 success/ok。抄到这里就成
    // 了第二份真相，改一条忘另一条，同一次调用实时是红的、刷新变绿的。
    const ok = call?.ok !== false;
    card.classList.add(ok ? "is-success" : "is-failure");
    if (ok && (name === "generate_image" || name === "print_image")) {
      card.classList.add("image-tool-chip");
    }

    const head = document.createElement("button");
    head.className = "tool-head";
    head.type = "button";
    head.setAttribute("aria-expanded", String(Boolean(state.toolExpanded)));
    const icon = document.createElement("span");
    icon.className = "tool-icon";
    icon.appendChild(makeIconSlot(toolIconName(name)));
    // 与实时同构的三段：友好名（粗体）/ 技术名（小字）/ 主语摘要。
    // 只画技术名的话，用户看到的就是 archlinux_official_package_query 这种。
    const title = document.createElement("span");
    title.className = "tool-title";
    const displayName = document.createElement("strong");
    displayName.textContent = String(call?.display_name || name || t("未命名工具"));
    // 子代理:显示「子代理 / 开发中」,不显裸的 `subagent:xxx`(刷新回看时历史里存的
    // display_name 是技术名,和实时的「子代理」不一致,#97 刷新后变回原始名)。任务
    // 标题走下面的 summary(toolSubject → description)。
    if (isSubagentTool(name)) {
      displayName.textContent =
        parsedToolArguments(call?.arguments)?.dev === true ? t("开发中") : t("子代理");
    }
    // 名字被芯片截断时,悬浮还能看全(load_tools 一次点名几个工具就会超长)。
    displayName.title = displayName.textContent;
    const realName = document.createElement("small");
    realName.className = "tool-technical-name";
    realName.textContent = name;
    const summary = document.createElement("small");
    summary.className = "tool-summary";
    const persistedArgs = parsedToolArguments(call?.arguments);
    const isCommandCall = name === "run_command" || name === "Bash";
    // 命令签的抬头右边给 short_title（模型自报的 `title`），命令本身排到下面；
    // 别的工具照旧给主语。两条路必须一致，否则一刷新命令签就变回老样子。
    summary.textContent = isCommandCall
      ? String(persistedArgs?.title || "").trim()
      : toolSubject(name, call?.arguments) || "";
    title.append(displayName, realName, summary);
    // 与实时那份同构：head 是 icon / title / status / chevron 四段。少了
    // status 这段，回看时卡片会比实时的窄一块，右边空一片。
    const status = document.createElement("span");
    status.className = "tool-status";
    const statusText = document.createElement("span");
    const startedMs = Number(call?.started_ms);
    const finishedMs = Number(call?.finished_ms);
    const hasSpan = Number.isFinite(startedMs) && Number.isFinite(finishedMs) && finishedMs >= startedMs;
    if (hasSpan) card.miyuTiming = { startedAt: startedMs, finishedAt: finishedMs };
    statusText.textContent = ok ? (hasSpan ? formatToolDuration(finishedMs - startedMs) || t("完成") : t("完成")) : t("失败");
    status.append(makeIconSlot(ok ? "check" : "circle-alert"), statusText);
    head.append(icon, title, status, makeIconSlot("chevron-down", "tool-chevron"));
    // 读秒：落库的起止时间算得出真实耗时，和实时那条定格的是同一个数。
    if (isCommandCall && ok && hasSpan) {
      const seconds = document.createElement("small");
      seconds.className = "tool-command-seconds";
      seconds.textContent = statusText.textContent;
      title.insertBefore(seconds, summary);
    }
    head.addEventListener("click", () => {
      if (window.MiyuSubagents?.openFromCard(card)) return;
      const collapsed = card.classList.toggle("collapsed");
      head.setAttribute("aria-expanded", String(!collapsed));
      railSnapFit(card);
    });

    const commandRows = isCommandCall
      ? buildCommandRows(
          String(persistedArgs?.command || persistedArgs?.cmd || "").trim(),
          head
        )
      : null;

    const body = document.createElement("div");
    body.className = "tool-body";
    // 文件编辑:把 patchText 参数画成 diff(增删配色),而不是摊一坨补丁 JSON。
    // patchText 随 tool_flow 落库,回看/刷新走同一份。渲不出(解析失败)再退回原始参数。
    const diffView = window.MiyuDiff?.renderFromCall?.(call) || null;
    let argumentsDetail = null;
    if (diffView) {
      body.appendChild(diffView);
    } else {
      const argumentText = prettyArguments(call?.arguments);
      if (argumentText) {
        const detail = createToolDetail(t("参数"), true);
        detail.content.textContent = argumentText;
        detail.wrapper.hidden = false;
        body.appendChild(detail.wrapper);
        argumentsDetail = detail;
      }
    }
    const output = String(call?.output || "");
    // 编辑成功时,结果就是 `{ok:true, files:[…]}` 这类样板,和上面的 diff 重复——藏掉;
    // 失败时结果是报错原文,留着(diffView 存在=是编辑工具且解析出了补丁)。
    const hideEditOutput = diffView && ok;
    if (output && !hideEditOutput) {
      const detail = createToolDetail(t("结果"), true);
      detail.content.textContent = output;
      detail.wrapper.hidden = false;
      body.appendChild(detail.wrapper);
    }
    // 子代理:回看/刷新时把落库的子过程标记流回放成时间线(#9)。放在参数/结果之前,
    // 和实时展开态一个样。用一个一次性 sink 走同款 renderSubagentProgress。
    if (isSubagentTool(name) && Array.isArray(call?.sub_trace) && call.sub_trace.length) {
      const subBlocks = document.createElement("div");
      subBlocks.className = "sub-blocks assistant-blocks";
      const sink = {
        blocks: subBlocks, brief: false, think: null, thinkAccum: "", contentBlock: null,
        contentAccum: "", pendingCall: null, taskPeek: null, taskToken: null, peekLine: "",
      };
      for (const marker of call.sub_trace) renderSubagentProgress(sink, String(marker));
      subEndReasoning(sink);
      subEndContent(sink);
      body.insertBefore(subBlocks, body.firstChild);
      card.classList.add("is-task");
    }
    markFirstDetail(body);
    const fold = document.createElement("div");
    fold.className = "tool-fold";
    fold.appendChild(body);
    card.append(head, fold);
    if (isSubagentTool(name)) {
      window.MiyuSubagents?.link(
        card,
        call?.child_session_id || window.MiyuSubagents.sessionOfOutput(output)
      );
    }
    // 命令那几行排在抬头和展开区之间——和实时那条同一个次序。
    if (commandRows) {
      card.insertBefore(commandRows.preview, fold);
      card.insertBefore(commandRows.more, fold);
    }
    // 跨会话发话(09-23):和实时那张同一个样子;抬头右边的会话名用结果里报的。
    if (window.MiyuCrossSession?.isSendTool(name)) {
      window.MiyuCrossSession.decorateSendCard({
        card, head, body, argumentsDetail, argumentsValue: call?.arguments
      });
      const target = window.MiyuCrossSession.targetName(output);
      if (target) summary.textContent = window.MiyuCrossSession.sendSubject(call?.arguments, target);
    }
    // 待办列表挂在签外面,收起态也看得见——那是给人看的产出,不是调试信息。
    const todos = window.MiyuTodos?.isTodoTool(name) ? window.MiyuTodos.render(output) : null;
    if (todos) card.appendChild(todos);
    // 分享附件同理:文件卡片是交付物,直接出现在气泡里,点击即下载。
    const shared = window.MiyuShared?.isShareTool(name) ? window.MiyuShared.renderCard(output) : null;
    if (shared) card.appendChild(shared);
    return card;
  }

  function createToolDetail(labelText, preformatted = false) {
    const wrapper = document.createElement("div");
    wrapper.className = "tool-detail";
    wrapper.hidden = true;
    const label = document.createElement("span");
    label.className = "tool-detail-label";
    label.textContent = labelText;
    const content = document.createElement(preformatted ? "pre" : "p");
    wrapper.append(label, content);
    return { wrapper, content, raw: "" };
  }

  function updateToolStatus(tool, status, iconName, statusClass = "") {
    tool.statusText.textContent = status;
    // 命令签的读秒定格成**真实耗时**（`status` 里就是它，比如「23 ms」），并停掉
    // ticker。ticker 算的是「卡片建出来到现在」，对一条 23ms 就跑完的命令毫无
    // 意义——它会一直涨到下一次 tick 才停在一个错数上。
    if (tool.commandSeconds && iconName !== "loader-circle") {
      delete tool.commandSeconds.dataset.taskStart;
      tool.commandSeconds.textContent = status === t("失败") ? "" : status;
    }
    tool.statusIcon.replaceChildren(createIcon(iconName));
    tool.statusIcon.classList.toggle("is-spinning", iconName === "loader-circle");
    tool.card.classList.remove("is-success", "is-failure");
    if (statusClass) tool.card.classList.add(statusClass);
    procLineRefresh(tool.card.closest(".proc-line"));
  }

  function renderCommandOutputPreview(tool) {
    const preview = tool.pendingOutputPreview;
    const panel = tool.commandOutputPreview;
    if (!panel || !preview || !Array.isArray(preview.lines)) return;
    const wasFollowing = panel.hidden || panel.scrollHeight - panel.scrollTop - panel.clientHeight <= 2;
    const previousScrollTop = panel.scrollTop;
    const children = [];
    if (preview.omitted) {
      const omitted = document.createElement("span");
      omitted.className = "tool-command-output-omitted";
      omitted.textContent = t("⋮ 已省略较早输出");
      children.push(omitted);
    }
    for (const line of preview.lines) {
      const row = document.createElement("span");
      row.className = `tool-command-output-line${line?.stream === "stderr" ? " is-stderr" : ""}`;
      row.textContent = String(line?.text || "");
      children.push(row);
    }
    panel.replaceChildren(...children);
    panel.hidden = children.length === 0;
    if (!panel.hidden) panel.scrollTop = wasFollowing ? panel.scrollHeight : previousScrollTop;
  }

  function scheduleCommandOutputPreview(tool, preview) {
    if (!tool?.commandOutputPreview || !preview || typeof preview !== "object") return;
    tool.pendingOutputPreview = preview;
    if (tool.outputRenderFrame) return;
    tool.outputRenderFrame = window.requestAnimationFrame(() => {
      tool.outputRenderFrame = null;
      renderCommandOutputPreview(tool);
      contentAdded(tool.card);
    });
  }

  // 工具家族图标(验收清单):终端=$、网络=地球仪、编辑=笔、记忆=大脑……
  // 未列家族回落扳手。全部 lucide 线稿,无 emoji。
  function toolIconName(name) {
    const n = String(name || "");
    if (["run_command", "Bash", "job_status", "job_stop"].includes(n)) return "terminal";
    if (["web_search", "web_fetch", "search_web", "webfetch", "read_url_content"].includes(n)) return "globe";
    // agy 原生工具(antigravity 中转)。
    if (["view_file", "list_dir"].includes(n)) return "file-text";
    if (["write_to_file", "replace_file_content"].includes(n)) return "square-pen";
    if (["find_by_name", "grep_search"].includes(n)) return "search";
    if (["manage_task", "invoke_subagent", "define_subagent", "manage_subagents"].includes(n)) return "bot";
    if (n.startsWith("browser_") || n === "call_mcp_tool") return "wrench";
    if (n === "search_web_images") return "image-search";
    if (["edit", "artifact", "kb", "apply_patch", "apply_artifact_patch"].includes(n)) return "square-pen";
    if (["recall_memories", "recall_past_events", "remember_fact", "search_evicted_context"].includes(n)) return "brain";
    if (["create_goal", "get_goal", "update_goal"].includes(n)) return "target";
    if (n === "todowrite" || n === "todoupdate") return "list-todo";
    if (isSubagentTool(n)) return "bot";
    if (n.includes("knowledge_base")) return "book-open";
    if (n === "ask_question") return "circle-help";
    if (n === "send_to_other_running_session") return "send";
    if (n === "generate_image") return "paintbrush";
    if (["analyze_image", "vision_analyze", "print_image"].includes(n)) return "image";
    if (n.includes("meme")) return "smile";
    if (n.includes("alarm")) return "alarm-clock";
    if (n === "read_clipboard") return "clipboard";
    if (n === "get_weather") return "cloud-sun";
    if (["calculator", "scientific_calculator", "calculate_hash", "get_exchange_rate", "decode_encoded_text"].includes(n)) return "calculator";
    if (n === "read" || n === "read_file") return "file-text";
    if (n === "glob" || n === "grep") return "search";
    if (n === "trash_path") return "trash-2";
    if (n === "load_tools") return "package";
    if (n.includes("skill")) return "puzzle";
    if (n.startsWith("aur_") || n.startsWith("archlinux") || n.startsWith("archwiki") || n === "install_aur_package") return "arch";
    if (n.startsWith("online_man")) return "package";
    if (n === "usage_query") return "chart-column";
    if (["draw_tarot_card", "draw_zhouyi_hexagram", "draw_fortune_lot"].includes(n)) return "sparkles";
    if (["create_artifact", "read_artifact", "present_artifact"].includes(n)) return "file-text";
    return "wrench";
  }

  /// 生图占位气泡的点阵动画(A 方案,08-22 定稿):随机位置/大小/时长的
  /// 小块点阵若隐若现,同屏最多 3 块;出图/失败/离屏即停,定时器不外泄。
  function startImageGenDots(bubble) {
    const spawn = () => {
      if (!bubble.isConnected) {
        stopImageGenDots(bubble);
        return;
      }
      if (bubble.querySelectorAll(".dot-patch").length >= 3) return;
      const patch = document.createElement("span");
      patch.className = "dot-patch";
      const size = 60 + Math.random() * 90;
      patch.style.width = `${size}px`;
      patch.style.height = `${size}px`;
      patch.style.left = `${Math.random() * 78}%`;
      patch.style.top = `${Math.random() * 78}%`;
      patch.style.animationDuration = `${(2.2 + Math.random() * 1.6).toFixed(2)}s`;
      patch.addEventListener("animationend", () => patch.remove());
      bubble.appendChild(patch);
    };
    spawn();
    window.setTimeout(spawn, 500);
    bubble.miyuDotsTimer = window.setInterval(spawn, 700);
  }

  function stopImageGenDots(bubble) {
    if (bubble?.miyuDotsTimer) {
      window.clearInterval(bubble.miyuDotsTimer);
      bubble.miyuDotsTimer = null;
    }
  }

  function createTool(live, data, opts = {}) {
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    breakLiveText(live);
    finalizeLiveReasoning(live);
    live.contextOperation = null;
    const toolId = String(data?.tool_id || `${live.runId}_tool_unknown_${live.tools.size + 1}`);
    if (live.tools.has(toolId)) return live.tools.get(toolId);
    const card = document.createElement("section");
    card.className = state.toolExpanded ? "tool-card" : "tool-card collapsed";
    card.dataset.toolId = toolId;
    // 收缩行按工具名分类数(09-24):命令 / 改文件 / 其余。
    card.dataset.toolName = String(data?.name || "");
    const isCommand = COMMAND_TOOLS.includes(String(data?.name || ""));
    if (isCommand) card.classList.add("is-command");
    const isTask =
      isSubagentTool(data?.name) ||
      /^(subagent|task)[:：]/i.test(String(data?.display_name || ""));
    if (isTask) {
      card.classList.add("is-task");
      // 前台子代理:运行时自动展开那块四行活区域,子过程实时流入(用户拍板);
      // 跑完(tool.finished)再收起成一行。head 的 aria-expanded 也置真。
      card.classList.remove("collapsed");
    }
    const subjectText = toolSubject(data?.name, data?.arguments);
    const commandArguments = isCommand ? parsedToolArguments(data?.arguments) : null;
    const commandText = isCommand ? String(commandArguments?.command || commandArguments?.cmd || "").trim() : "";
    const head = document.createElement("button");
    head.className = "tool-head";
    head.type = "button";
    head.setAttribute("aria-expanded", String(Boolean(state.toolExpanded)));
    const icon = document.createElement("span");
    icon.className = "tool-icon";
    const toolName = String(data?.name || "");
    // 生图/打图走 GPT 式点阵占位气泡,芯片隐藏(失败时再露出来给细节)。
    const isImageTool = toolName === "generate_image" || toolName === "print_image";
    if (isImageTool) card.classList.add("image-tool-chip");
    icon.appendChild(makeIconSlot(toolIconName(toolName)));
    const title = document.createElement("span");
    title.className = "tool-title";
    const displayName = document.createElement("strong");
    displayName.textContent = String(data?.display_name || data?.name || t("未命名工具"));
    // 开发模式子代理显示「开发中」而非「子代理」,和普通子代理区分开(09-11)。
    if (isTask && parsedToolArguments(data?.arguments)?.dev === true) {
      displayName.textContent = t("开发中");
    }
    displayName.title = displayName.textContent;
    const realName = document.createElement("small");
    realName.className = "tool-technical-name";
    realName.textContent = String(data?.name || "");
    const summary = document.createElement("small");
    summary.className = "tool-summary";
    title.append(displayName, realName, summary);
    const status = document.createElement("span");
    status.className = "tool-status";
    const statusIcon = makeIconSlot("loader-circle", "is-spinning");
    const statusText = document.createElement("span");
    statusText.textContent = t("运行中");
    status.append(statusIcon, statusText);
    const chevron = makeIconSlot("chevron-down", "tool-chevron");
    // 子代理:标题行里放一条单行窥视(和「已思考」标题右侧尾巴同款),收起态
    // 显示子代理当前在做什么;不再用带底色的方块(那读起来像独立 tag,09-11)。
    let taskPeek = null;
    let taskToken = null;
    if (isTask) {
      const peekSlot = document.createElement("span");
      peekSlot.className = "reasoning-peek tool-peek";
      taskPeek = document.createElement("span");
      peekSlot.appendChild(taskPeek);
      // 前台子代理行也带 token 消耗 + 读秒(09-12 #6,与后台任务条同口径)。
      // token 由 renderSubagentProgress 解析 stats 后写进 taskToken;读秒由全局
      // ticker 按 data-task-start 更新,卡片进入 is-success/is-failure 即定格。
      taskToken = document.createElement("span");
      taskToken.className = "job-chip-token tool-task-token";
      const seconds = document.createElement("span");
      seconds.className = "tool-task-seconds";
      seconds.dataset.taskStart = String(performance.now());
      seconds.textContent = "0s";
      // 布局(#2):子代理·title · token 秒数 · <淡出过渡> 窥视(撑开)。token/秒数紧跟标题,
      // 窥视占满余下、左侧淡出,不再夹在标题和 token 之间把标题顶开。
      head.append(icon, title, taskToken, seconds, peekSlot, status, chevron);
    } else {
      head.append(icon, title, status, chevron);
    }
    let commandPreview = null;
    let commandMore = null;
    let commandOutputPreview = null;
    let commandSeconds = null;
    if (isCommand) {
      // 读秒塞进 `.tool-title` 里（工具名和 short_title 之间），不挪 `.tool-summary`：
      // 现有 CSS 有一堆 `.tool-title .tool-summary` 的选择器（失败变红、省略、悬浮），
      // 把它移出去会牵动别的工具签。`.tool-status` 那个位置指望不上——它的文字被
      // `.tool-status > span:not(.icon-slot){display:none}` 全局藏着，成功的行上
      // 图标也藏了，所以那儿从来什么都没有。
      commandSeconds = document.createElement("small");
      commandSeconds.className = "tool-command-seconds";
      commandSeconds.dataset.taskStart = String(performance.now());
      commandSeconds.textContent = "0s";
      title.insertBefore(commandSeconds, summary);
      // 命令本身按行排在抬头底下，靠时间线那根竖线（`.proc-rail`）串起来。
      commandPreview = document.createElement("div");
      commandPreview.className = "tool-command-preview";
      commandPreview.setAttribute("aria-label", t("命令"));
      // `⋮` 得在裁剪容器**外面**，否则它自己也被 max-height 裁掉。
      commandMore = document.createElement("span");
      commandMore.className = "tool-command-more";
      commandMore.textContent = "⋮";
      commandMore.title = t("还有，点开看完整命令");
      commandMore.hidden = true;
      // 命令那几行也要能点开——它们在 `.tool-head`（button）**外面**，不点就
      // 只有抬头那一条窄缝可点，而视线正落在命令上（用户 09-20）。
      // 拖着选文字不算点：有选区就放过，否则复制命令会顺手把卡片折起来。
      const expandOnClick = (event) => {
        if (String(window.getSelection?.() || "").length) return;
        event.preventDefault();
        head.click();
      };
      for (const node of [commandPreview, commandMore]) {
        node.addEventListener("click", expandOnClick);
        node.style.cursor = "pointer";
      }
      commandOutputPreview = document.createElement("div");
      commandOutputPreview.className = "tool-command-output-preview";
      commandOutputPreview.setAttribute("aria-label", t("最近命令输出"));
      commandOutputPreview.style.setProperty("--command-output-lines", String(COMMAND_OUTPUT_PREVIEW_ROWS));
      commandOutputPreview.hidden = true;
    }
    const body = document.createElement("div");
    body.className = "tool-body";
    const argumentsDetail = createToolDetail(t("参数"), true);
    const progressDetail = createToolDetail(t("进度"));
    const stdoutDetail = createToolDetail(t("命令输出"), true);
    const stderrDetail = createToolDetail(t("错误输出"), true);
    stderrDetail.wrapper.classList.add("is-stderr");
    const resultDetail = createToolDetail(t("结果"), true);
    // 文件编辑:patchText 参数画成 diff,而不是摊一坨补丁 JSON(实时与刷新回看同一份)。
    const diffView = window.MiyuDiff?.renderFromCall?.({ name: data?.name, arguments: data?.arguments }) || null;
    const argumentText = diffView ? "" : prettyArguments(data?.arguments);
    if (argumentText) {
      argumentsDetail.raw = argumentText;
      argumentsDetail.content.textContent = argumentText;
      argumentsDetail.wrapper.hidden = false;
    }
    body.append(argumentsDetail.wrapper, progressDetail.wrapper, stdoutDetail.wrapper, stderrDetail.wrapper, resultDetail.wrapper);
    if (diffView) body.insertBefore(diffView, argumentsDetail.wrapper);
    // 子代理:收起看标题行的窥视,展开看下面的「子过程时间线」——子代理自己的
    // 思考与工具流,和主智能体的过程区同款渲染(09-11 用户要求)。不再用方块。
    let liveProgress = null;
    let subBlocks = null;
    let briefBuilt = false;
    if (isTask) {
      // 子过程时间线的承载容器:proc-line 挂进这里(和主对话过程区同构)。
      subBlocks = document.createElement("div");
      subBlocks.className = "sub-blocks assistant-blocks";
      body.insertBefore(subBlocks, body.firstChild);
      // 子代理的任务简介放在展开区最上方,美化呈现,不再让人去读裸 JSON 参数
      //(09-12 #6):标题=description,正文=prompt(整段保留换行)。裸参数那栏
      // 对子代理收起来(信息都在简介里了)。
      const taskArgs = parsedToolArguments(data?.arguments);
      const brief = buildSubagentBrief(taskArgs.description, taskArgs.prompt);
      if (brief) {
        attachSubBrief(subBlocks, brief);
        argumentsDetail.wrapper.hidden = true;
        briefBuilt = true;
      }
      const fold = document.createElement("div");
      fold.className = "tool-fold";
      fold.appendChild(body);
      card.append(head, fold);
      if (taskPeek) taskPeek.textContent = reasoningPeekText(subjectText || t("正在启动子代理…"));
    } else {
      card.append(head);
      if (commandPreview) card.appendChild(commandPreview);
      if (commandMore) card.appendChild(commandMore);
      if (commandOutputPreview) card.appendChild(commandOutputPreview);
      const fold = document.createElement("div");
      fold.className = "tool-fold";
      fold.appendChild(body);
      card.appendChild(fold);
      // 跨会话发话(09-23):抬头底下露正文前几行,展开区顶上放全文。
      if (window.MiyuCrossSession?.isSendTool(toolName)) {
        window.MiyuCrossSession.decorateSendCard({
          card, head, body, argumentsDetail, argumentsValue: data?.arguments
        });
      }
    }
    const tool = {
      id: toolId,
      name: String(data?.name || ""),
      // 抬头右边那句：命令给的是模型自报的 `title`，不是命令文本——命令印在抬头
      // 底下（和 Rust 侧 `tool_display::command_peek()` 同口径）。没填就空着。
      commandTitle: isCommand ? String(commandArguments?.title || "").trim() : "",
      commandSeconds,
      commandMore,
      card,
      head,
      body,
      status,
      statusIcon,
      statusText,
      summary,
      commandPreview,
      commandOutputPreview,
      commandText,
      artifactPreview: null,
      pendingOutputPreview: null,
      outputRenderFrame: null,
      argumentsDetail,
      progressDetail,
      stdoutDetail,
      stderrDetail,
      resultDetail,
      isTask,
      liveProgress,
      taskPeek,
      taskToken,
      brief: briefBuilt,
      blocks: subBlocks,
      think: null,
      thinkAccum: "",
      pendingCall: null,
      titleText: String(data?.display_name || data?.name || t("未命名工具")),
      subject: subjectText,
      argumentsValue: data?.arguments,
      startedAt: performance.now(),
      finishedAt: null,
      imageCount: 0,
      isImageTool,
      imagePlaceholder: null,
      finished: false,
      collapseTimer: null
    };
    head.addEventListener("click", () => {
      // 子代理是一条会话:认得它的会话就打开它,不再原地展开(会话项目第 4 段)。
      if (window.MiyuSubagents?.openFromCard(card)) return;
      const collapsed = card.classList.toggle("collapsed");
      head.setAttribute("aria-expanded", String(!collapsed));
      // 收起子代理状态行时,把里面已展开的思考/工具也一并收起,下次展开是干净的
      // 收起态(#5),不然收起只是把外层折了、里面还留着上次的展开。
      if (collapsed) {
        card.querySelectorAll(".sub-blocks details[open]").forEach((d) => {
          d.open = false;
        });
        card.querySelectorAll(".sub-blocks .tool-card:not(.collapsed)").forEach((inner) => {
          inner.classList.add("collapsed");
          const innerHead = inner.querySelector(".tool-head");
          if (innerHead) innerHead.setAttribute("aria-expanded", "false");
        });
      }
      railSnapFit(card);
      syncBubbleWidth(live.article);
      if (!collapsed) {
        window.requestAnimationFrame(() => {
          scrollToolOutputToEnd(tool);
          // 展开从「当前进行中」看起,而不是从顶部(#8)。滚到底 = 最新那一步。
          const sc = subScrollContainer(tool);
          if (sc) { sc.__pinnedUp = false; sc.scrollTop = sc.scrollHeight; }
          contentAdded();
        });
      }
    });
    updateToolSummary(tool);
    card.miyuTiming = tool;
    live.tools.set(toolId, tool);
    // 顶替「准备 xx」占位签时不重放淡入:占位签已经平滑滑入,这里只是原地
    // 把文字换成正式工具名,再滑一次会显得整行错位(#17,只在会发 preparing
    // 的中转线后端出现)。
    if (opts.staticEnter) card.style.animation = "none";
    procLineAttach(live.blocks, card);
    if (isImageTool) {
      const bubble = document.createElement("div");
      bubble.className = "image-gen-bubble";
      const label = document.createElement("span");
      label.className = "image-gen-label";
      label.textContent = toolName === "print_image" ? t("正在加载图片") : t("正在生成图片");
      if (subjectText) bubble.title = subjectText;
      bubble.appendChild(label);
      procLineBreak(live.blocks);
      live.blocks.appendChild(bubble);
      startImageGenDots(bubble);
      tool.imagePlaceholder = bubble;
    }
    syncBubbleWidth(live.article);
    contentAdded(live);
    return tool;
  }

  function ensureTool(live, data) {
    const toolId = String(data?.tool_id || "");
    return (toolId && live.tools.get(toolId)) || createTool(live, data);
  }

  // The backend sends the phase text; the local map is only a fallback for a
  // daemon older than this asset.
  function preparingToolLabel(name, phase) {
    if (phase) return String(phase);
    if (["edit", "artifact", "kb", "apply_patch", "apply_artifact_patch"].includes(name)) return t("准备编辑");
    if (name === "run_command") return t("准备执行");
    if (name === "ask_question") return t("准备问题");
    return t("准备工具");
  }

  function clearPreparingTool(live) {
    if (!live?.preparingTool) return;
    live.preparingTool.remove();
    live.preparingTool = null;
    stopPreparingTimer(live);
    contentAdded(live);
  }

  function stopPreparingTimer(live) {
    if (!live?.preparingTimer) return;
    window.clearInterval(live.preparingTimer);
    live.preparingTimer = null;
  }

  /// 准备窗口结束：秒表归零，下一批重新计。
  ///
  /// 只在**工具真的跑完**或新一轮思考开始时调用,不在 `tool.started` 时调用
  /// ——批量调用里第二个工具的准备提示紧接着第一个的开工到来,那还是同一个
  /// 等待窗口,归零的话屏幕上的秒数来回横跳(与 REPL 的
  /// `tool_preparing_since` 同一套语义)。
  function resetPreparingWindow(live) {
    if (!live) return;
    live.preparingSince = null;
    clearPreparingTool(live);
  }

  function renderPreparingLabel(live) {
    const tag = live?.preparingTool;
    if (!tag) return;
    const label = tag.querySelector(".tool-preparing-label");
    if (!label) return;
    const base = tag.dataset.phaseLabel || "";
    const elapsed = live.preparingSince == null
      ? ""
      : formatToolDuration(performance.now() - live.preparingSince);
    label.textContent = elapsed ? `${base} · ${elapsed}` : base;
  }

  function handleToolPreparing(live, data) {
    const name = String(data?.tool_name || "");
    if (!name) return;
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    finalizeLiveReasoning(live);
    // 窗口起点只认第一次——批量里换了工具不重新计时。
    if (live.preparingSince == null) live.preparingSince = performance.now();
    if (live.preparingTool?.dataset.toolName === name) return;
    clearPreparingTool(live);
    const tag = document.createElement("div");
    tag.className = "tool-preparing-tag";
    tag.dataset.toolName = name;
    tag.dataset.phaseLabel = preparingToolLabel(name, data?.phase);
    const label = document.createElement("span");
    label.className = "tool-preparing-label";
    tag.append(makeIconSlot("loader-circle", "is-spinning"), label);
    procLineAttach(live.blocks, tag);
    live.preparingTool = tag;
    renderPreparingLabel(live);
    live.preparingTimer = window.setInterval(() => renderPreparingLabel(live), 200);
    syncBubbleWidth(live.article);
    contentAdded(live);
  }

  function handleToolEvent(name, live, data) {
    if (name === "tool.preparing") {
      handleToolPreparing(live, data);
      return;
    }
    if (name === "tool.started") {
      // 只撤标签,不清 `preparingSince`：同一批里下一个工具的准备提示紧接着
      // 到来,那还是同一个等待窗口。
      const morphing = !!live.preparingTool;
      clearPreparingTool(live);
      createTool(live, data, { staticEnter: morphing });
      return;
    }
    const tool = ensureTool(live, data);
    if (name === "tool.image") {
      const asset = data?.asset && typeof data.asset === "object" ? data.asset : null;
      if (asset && safeAssetUrl(asset.url)) {
        const assetId = String(asset.id || asset.url);
        if (!live.assets.some((item) => String(item?.id || item?.url) === assetId)) {
          ensureLiveArticle(live);
          clearTypingIndicator(live, { waitingOnly: true });
          breakLiveText(live);
          finalizeLiveReasoning(live);
          live.contextOperation = null;
          live.assets.push(asset);
          const media = createConversationMedia(asset, { eager: true });
          if (tool.imagePlaceholder) {
            stopImageGenDots(tool.imagePlaceholder);
            tool.imagePlaceholder.replaceWith(media);
            tool.imagePlaceholder = null;
          } else {
            procLineBreak(live.blocks);
            live.blocks.appendChild(media);
          }
          // 不自动进 artifact:图片已经在气泡里画出来了,再塞进面板等于同一张
          // 图占两个位置,还会把面板自动切过去盖住用户正在看的东西——表情包
          // 也会。要在工作区看，气泡上有「在预览工作区打开」按钮。
          syncBubbleWidth(live.article);
          tool.imageCount += 1;
        }
      } else if (data?.error) {
        const message = String(data.error);
        tool.progressDetail.raw = message;
        tool.progressDetail.content.textContent = message;
        tool.progressDetail.wrapper.hidden = Boolean(tool.liveProgress);
        if (tool.liveProgress) {
          tool.liveProgress.textContent = message;
          tool.liveProgress.hidden = false;
        }
      }
      updateToolSummary(tool);
    } else if (name === "tool.artifact") {
      const artifact = normalizeArtifact(data?.artifact, "file");
      if (artifact) {
        registerArtifact(artifact, { autoOpen: true });
        if (!live.artifacts) live.artifacts = [];
        const index = live.artifacts.findIndex((item) => String(item?.id) === artifact.id);
        if (index >= 0) live.artifacts[index] = artifact;
        else live.artifacts.push(artifact);
        if (!tool.artifactPreview) {
          tool.artifactPreview = document.createElement("button");
          tool.artifactPreview.type = "button";
          tool.artifactPreview.className = "tool-artifact-preview";
          tool.card.insertBefore(tool.artifactPreview, tool.body);
          tool.artifactPreview.addEventListener("click", () => {
            const current = state.artifacts.find((item) => item.id === tool.artifactPreview.dataset.artifactId);
            if (!current) return;
            state.selectedArtifactId = current.id;
            setArtifactWorkspaceOpen(true);
          });
        }
        tool.artifactPreview.dataset.artifactId = artifact.id;
        const artifactLabel = document.createElement("span");
        artifactLabel.textContent = artifact.name;
        tool.artifactPreview.replaceChildren(
          makeIconSlot(artifactIconName(artifact)),
          artifactLabel,
          makeIconSlot("panel-right")
        );
        tool.subject = artifact.name;
      } else if (data?.error) {
        tool.progressDetail.raw = String(data.error);
        tool.progressDetail.content.textContent = tool.progressDetail.raw;
        tool.progressDetail.wrapper.hidden = false;
      }
      updateToolSummary(tool);
    } else if (name === "subagent.progress" && tool.isTask) {
      // 子代理此刻的样子(会话项目第 4 段之二):窥视、词元、子会话。子会话里的过程在它自己
      // 那儿,卡片点下去打开它,父会话这边只画标题行这一行。
      if (data?.session_id) window.MiyuSubagents?.link(tool.card, String(data.session_id));
      const peek = String(data?.peek || "");
      if (peek) {
        tool.peekLine = peek;
        if (tool.taskPeek) setReasoningPeek(tool.taskPeek, peek);
      }
      const tokens = String(data?.tokens_label || "");
      if (tokens) {
        tool.tokenText = tokens;
        if (tool.taskToken) tool.taskToken.textContent = tokens;
      }
      if (!tool.finished) updateToolStatus(tool, t("运行中"), "loader-circle");
    } else if (name === "tool.progress" && tool.isTask) {
      // 子代理的过程在它自己的会话里,窥视、词元、会话走 `subagent.progress`(会话项目第 4 段
      // 之二)。这条通道上只剩「交到后台了」那一句:当成窥视挂在标题行上。
      const message = String(data?.message || "").replace(/^__subagent_detach__/, "").trim();
      if (message && !message.startsWith("__")) {
        tool.peekLine = message;
        if (tool.taskPeek) setReasoningPeek(tool.taskPeek, message);
      }
      if (!tool.finished) updateToolStatus(tool, t("运行中"), "loader-circle");
    } else if (name === "tool.progress") {
      let message = String(data?.message || "");
      // 文件编辑(edit/kb/artifact):diff 卡已由 patchText 参数在建卡时画好,「准备修改」
      // 这类阶段签、`__patch_preview__` 预览等中间进度都是噪点,一律丢弃,只留 diff + 结果。
      if (message.startsWith("__patch_preview__") || window.MiyuDiff?.isEditTool?.(tool.name)) return;
      // todowrite 的清单：那是给终端画表用的标记（`__todo_table__` + 整张清单的 JSON）。
      // 网页的任务卡片另从会话的 todos 接口取，这条对网页没用；原来没拦，没有主语时
      // 被拿来当主语，整段 JSON 印在时间线上（09-24）。
      if (message.startsWith("__todo_table__")) return;
      // 阶段签(「准备修改」这类)只描述过程,不是结果:工具失败后不该留在卡片上
      // 当错误说明(09-11 手机端实测 edit 被沙盒拒后还挂着「准备修改」)。
      tool.lastProgressWasPhase = message.startsWith("__tool_phase__");
      if (message.startsWith("__tool_phase__")) {
        message = message.slice("__tool_phase__".length).replace(/^~\s*/, "").trim();
      } else if (message.startsWith("__subagent_stats__")) {
        message = message.slice("__subagent_stats__".length).trim();
      } else if (message.startsWith("__subagent_detach__")) {
        message = message.slice("__subagent_detach__".length).trim();
      }
      // 任何持续汇报进度的工具(插件子代理如兼容性调查)都惰性获得实时进度面板,
      // 不再仅限内置 task 工具
      if (!tool.liveProgress && !tool.finished && message) {
        tool.liveProgress = document.createElement("div");
        tool.liveProgress.className = "tool-live-progress";
        // body 在普通/命令卡里包在 .tool-fold 里,不是 card 的直接子节点,直接
        // card.insertBefore(_, body) 会抛 NotFoundError(编辑工具的「准备修改」阶段
        // 一直在悄悄抛,live 进度面板从来没真出现过)。挂到 body 顶部即可。
        if (tool.body.parentNode === tool.card) {
          tool.card.insertBefore(tool.liveProgress, tool.body);
        } else {
          tool.body.insertBefore(tool.liveProgress, tool.body.firstChild);
        }
      }
      tool.progressDetail.raw = message;
      tool.progressDetail.content.textContent = message;
      tool.progressDetail.wrapper.hidden = !message || Boolean(tool.liveProgress);
      if (tool.liveProgress && message) {
        tool.liveProgress.textContent = message;
        tool.liveProgress.hidden = false;
        syncBubbleWidth(live.article);
      }
      if (!tool.subject && message) tool.subject = compactLine(message);
      updateToolStatus(tool, t("运行中"), "loader-circle");
      updateToolSummary(tool);
    } else if (name === "tool.output") {
      const detail = data?.stream === "stderr" ? tool.stderrDetail : tool.stdoutDetail;
      detail.raw = boundedAppend(detail.raw, String(data?.output || ""));
      detail.content.textContent = detail.raw;
      detail.wrapper.hidden = !detail.raw;
      if (!tool.card.classList.contains("collapsed")) detail.content.scrollTop = detail.content.scrollHeight;
      scheduleCommandOutputPreview(tool, data?.preview);
      updateToolSummary(tool);
    } else if (name === "tool.finished") {
      tool.finished = true;
      tool.finishedAt = performance.now();
      // 子代理跑完了,把最后停在「正在思考」的那块思考收尾成「已思考」(#4/#7)——
      // subEndReasoning 平时只在下一个工具调用到来时触发,子代理以思考结尾就没人收。
      // 跑完把那块四行活区域平滑收起成一行(用户拍板:运行时展开、完成后收起,可再点开)。
      if (tool.isTask) {
        subEndReasoning(tool);
        tool.card.classList.add("collapsed");
        tool.head.setAttribute("aria-expanded", "false");
        railSnapFit(tool.card);
        // 子代理跑完:它的实时估算先「冻住」保留(别立刻抽走,否则基线还没把它算进来
        // 之前累计会掉一下),等下个主回合的权威基线接管时再删(见 handleRoundUsage)。
        const doneId = String(data?.tool_id || tool.id || "");
        const entry = doneId && state.liveSubagentTokens.get(doneId);
        if (entry) { entry.done = true; entry.baseAtDone = asFiniteNumber(state.cumulativeBase?.total); refreshComposerCumulative(); }
      }
      const output = String(data?.output || "");
      if (tool.isTask) window.MiyuSubagents?.link(tool.card, window.MiyuSubagents.sessionOfOutput(output));
      // 跨会话发话:抬头右边补上对方报的会话名(侧栏里不一定认得它)。
      if (window.MiyuCrossSession?.isSendTool(tool.name)) {
        const target = window.MiyuCrossSession.targetName(output);
        if (target) {
          tool.subject = window.MiyuCrossSession.sendSubject(tool.argumentsValue, target);
          updateToolSummary(tool);
        }
      }
      tool.resultDetail.raw = output.length > MAX_TOOL_OUTPUT_CHARS ? t("[较早输出已省略]\n{tail}", {tail: output.slice(-MAX_TOOL_OUTPUT_CHARS)}) : output;
      tool.resultDetail.content.textContent = tool.resultDetail.raw;
      // 子代理的最终输出要显示出来(#6:用户要看 AI 的最终输出,上批误删了)。
      // 编辑工具成功时结果是 `{ok:true,files:[…]}` 样板,和 diff 卡重复——藏掉;失败留报错。
      const hideEditOutput = Boolean(data?.ok) && window.MiyuDiff?.isEditTool?.(tool.name)
        && tool.body.querySelector(".diff-view");
      tool.resultDetail.wrapper.hidden = !tool.resultDetail.raw || Boolean(hideEditOutput);
      if (tool.commandPreview && tool.resultDetail.raw) {
        tool.stdoutDetail.wrapper.hidden = true;
        tool.stderrDetail.wrapper.hidden = true;
      }
      const ok = Boolean(data?.ok);
      resetPreparingWindow(live);
      // 只刷正在看的那个会话——后台会话的 todowrite 不该改屏幕上这块面板。
      // 08-21 token-diet:新版 todowrite 输出是一行文本(不再回显整表 JSON),
      // parse 不出来时改从会话 todos API 取当前清单;旧 JSON 输出走原路。
      if (ok && window.MiyuTodos?.isTodoTool(tool.name)) {
        const parsed = window.MiyuTodos.parse(output);
        const sameSession = runSessionId(live.runId) === String(state.viewSessionId || "");
        if (parsed) {
          if (sameSession) renderStageTodos(parsed);
          // 与回看那份同构（`createPersistedToolCard`）：待办列表挂在签外面。
          // 只在这里画会让实时和刷新后长得不一样,那正是工具签之前踩过的坑。
          const todos = window.MiyuTodos.renderList(parsed);
          tool.card.querySelector(".todo-panel")?.remove();
          if (todos) tool.card.appendChild(todos);
        } else {
          attachLiveTodoPanel(tool, live, sameSession);
        }
      }
      // 分享附件同坑同修:实时完成时也要挂,否则只有刷新后才能看到卡片。
      if (ok && window.MiyuShared?.isShareTool(tool.name)) {
        const shared = window.MiyuShared.renderCard(output);
        tool.card.querySelector(".shared-attachment")?.remove();
        if (shared) tool.card.appendChild(shared);
      }
      scheduleCommandOutputPreview(tool, data?.preview);
      if (tool.imagePlaceholder) {
        stopImageGenDots(tool.imagePlaceholder);
        // 失败不留空气泡(08-22 用户反馈):撤占位、露芯片,错误细节在芯片里。
        tool.imagePlaceholder.remove();
        tool.imagePlaceholder = null;
      }
      if (tool.isImageTool && !ok) {
        tool.card.classList.remove("image-tool-chip");
      }
      // 时间线上成功不打勾不写「完成」,右侧就是耗时;失败才写字
      updateToolStatus(tool, ok ? formatToolDuration(tool.finishedAt - tool.startedAt) || t("完成") : t("失败"), ok ? "check" : "circle-alert", ok ? "is-success" : "is-failure");
      updateToolSummary(tool);
      if (tool.liveProgress) {
        if (ok || tool.lastProgressWasPhase) tool.liveProgress.hidden = true;
        else tool.liveProgress.classList.add("is-error");
        tool.progressDetail.wrapper.hidden = !tool.progressDetail.raw;
        syncBubbleWidth(live.article);
      }
      if (!state.toolExpanded) {
        tool.card.classList.add("collapsed");
        tool.head.setAttribute("aria-expanded", "false");
      }
    }
    contentAdded(live);
  }

  function questionHasAnswer(questionState, index = questionState.pageIndex) {
    const control = questionState.controls[index];
    if (!control) return false;
    return control.options.some((option) => option.input.checked)
      || Boolean(control.custom?.toggle.checked && control.custom.textarea.value.trim());
  }

  function updateQuestionNavigation(questionState) {
    if (!questionState?.questions?.length) return;
    const lastIndex = questionState.questions.length - 1;
    const atLastPage = questionState.pageIndex === lastIndex;
    const answered = questionHasAnswer(questionState);
    const canInteract = questionState.pending && !questionState.submitting && !questionState.closing;

    questionState.previous.disabled = !canInteract || questionState.pageIndex === 0;
    questionState.next.hidden = atLastPage;
    questionState.next.disabled = !canInteract || !answered;
    questionState.next.classList.toggle("is-ready", canInteract && answered && !atLastPage);
    questionState.submit.hidden = !atLastPage;
    questionState.submit.disabled = !canInteract || !answered;
    questionState.submit.classList.toggle("is-ready", canInteract && answered && atLastPage);
    questionState.close.disabled = !canInteract;

    questionState.controls.forEach((control, index) => {
      const custom = control.custom;
      if (!custom?.next) return;
      const customAnswered = Boolean(custom.toggle.checked && custom.textarea.value.trim());
      const show = canInteract && customAnswered;
      custom.next.hidden = !show;
      custom.next.disabled = !show;
      custom.next.classList.toggle("is-ready", show);
      custom.next.replaceChildren(makeIconSlot(index === lastIndex ? "check" : "chevron-right"));
      custom.next.title = index === lastIndex ? t("提交回答") : t("下一题");
      custom.next.setAttribute("aria-label", custom.next.title);
    });
  }

  function updateQuestionOptionClasses(questionState) {
    for (const control of questionState.controls) {
      for (const option of control.options) option.label.classList.toggle("selected", option.input.checked);
      if (control.custom) control.custom.wrapper.classList.toggle("selected", control.custom.toggle.checked);
    }
    updateQuestionNavigation(questionState);
  }

  function updateQuestionDock() {
    elements.questionDock.hidden = elements.questionDock.childElementCount === 0;
    elements.composerDock.classList.toggle("has-pending-question", !elements.questionDock.hidden);
    window.requestAnimationFrame(updateJumpButtonOffset);
  }

  function clearQuestionDock() {
    elements.questionDock.replaceChildren();
    updateQuestionDock();
  }

  function moveQuestionToTimeline(questionState) {
    if (questionState.card.parentElement !== elements.questionDock) return;
    if (questionState.timelineParent?.isConnected) questionState.timelineParent.appendChild(questionState.card);
    else questionState.card.remove();
    updateQuestionDock();
  }

  function removeQuestionFromDock(questionState) {
    if (questionState.card.parentElement === elements.questionDock) questionState.card.remove();
    updateQuestionDock();
  }

  function setQuestionPage(questionState, index, { focus = false } = {}) {
    if (!questionState?.pages?.length) return;
    if (questionState.autoAdvanceTimer) {
      window.clearTimeout(questionState.autoAdvanceTimer);
      questionState.autoAdvanceTimer = null;
    }
    const lastIndex = questionState.pages.length - 1;
    const nextIndex = Math.max(0, Math.min(lastIndex, Number(index) || 0));
    questionState.pageIndex = nextIndex;
    questionState.pages.forEach((page, pageIndex) => {
      page.hidden = pageIndex !== nextIndex;
    });
    const question = questionState.questions[nextIndex] || {};
    questionState.prompt.textContent = String(question.question || question.header || t("问题 {index}", {index: nextIndex + 1}));
    questionState.position.textContent = `${nextIndex + 1} / ${questionState.pages.length}`;
    updateQuestionNavigation(questionState);
    elements.questionDock.scrollTop = 0;
    window.requestAnimationFrame(() => {
      updateJumpButtonOffset();
      if (focus) questionState.pages[nextIndex].querySelector("input:not(:disabled), textarea:not(:disabled)")?.focus();
    });
  }

  function advanceQuestion(questionState) {
    if (!questionState?.pending || questionState.submitting || !questionHasAnswer(questionState)) return;
    if (questionState.pageIndex >= questionState.pages.length - 1) {
      submitQuestion(questionState);
      return;
    }
    setQuestionPage(questionState, questionState.pageIndex + 1, { focus: true });
  }

  function selectedQuestionAnswers(questionState) {
    const answers = [];
    for (let index = 0; index < questionState.controls.length; index += 1) {
      const control = questionState.controls[index];
      const selected = control.options.filter((option) => option.input.checked).map((option) => option.value);
      if (control.custom?.toggle.checked) {
        const custom = control.custom.textarea.value.trim();
        if (!custom) throw new Error(t("请填写第 {index} 项的自定义回答", {index: index + 1}));
        if (countCharacters(custom) > MAX_CUSTOM_ANSWER_CHARS) throw new Error(t("第 {index} 项的自定义回答不能超过 4,000 个字符", {index: index + 1}));
        if (/[\u0000-\u001f\u007f-\u009f]/.test(custom)) throw new Error(t("第 {index} 项的自定义回答不能包含控制字符或换行", {index: index + 1}));
        if (selected.includes(custom)) throw new Error(t("第 {index} 项包含重复回答", {index: index + 1}));
        selected.push(custom);
      }
      if (selected.length === 0) throw new Error(t("请回答第 {index} 项", {index: index + 1}));
      if (!control.multiple && selected.length !== 1) throw new Error(t("第 {index} 项只能选择一个回答", {index: index + 1}));
      answers.push(selected);
    }
    return answers;
  }

  function setQuestionControlsDisabled(questionState, disabled) {
    questionState.form.querySelectorAll("input, textarea, button").forEach((control) => {
      control.disabled = disabled;
    });
  }

  function renderQuestionAnswerSummary(questionState, answers) {
    questionState.summary.replaceChildren();
    const normalized = Array.isArray(answers) ? answers : [];
    questionState.questions.forEach((question, index) => {
      const row = document.createElement("div");
      const term = document.createElement("dt");
      term.textContent = String(question?.question || question?.header || t("问题 {index}", {index: index + 1}));
      const value = document.createElement("dd");
      value.textContent = (Array.isArray(normalized[index]) ? normalized[index] : []).map(String).join("、") || t("未记录");
      row.append(term, value);
      questionState.summary.appendChild(row);
    });
    questionState.summary.hidden = false;
  }

  function markQuestionAnswered(questionState, answers) {
    if (!questionState || !questionState.pending) return;
    if (questionState.autoAdvanceTimer) window.clearTimeout(questionState.autoAdvanceTimer);
    questionState.autoAdvanceTimer = null;
    questionState.pending = false;
    questionState.submitting = false;
    questionState.closing = false;
    questionState.restoreFocusOnClose = false;
    questionState.answers = answers;
    questionState.card.classList.remove("is-error");
    questionState.card.classList.add("is-answered");
    questionState.card.removeAttribute("aria-busy");
    questionState.header.hidden = false;
    questionState.card.removeAttribute("aria-label");
    questionState.card.setAttribute("aria-labelledby", questionState.titleId);
    questionState.status.textContent = t("已回答");
    // 去掉那个大对钩(#143/#161):和落库回看的已回答卡一致,「已回答」二字已够表达状态。
    questionState.icon.replaceChildren();
    questionState.icon.hidden = true;
    questionState.error.hidden = true;
    setQuestionControlsDisabled(questionState, true);
    renderQuestionAnswerSummary(questionState, answers);
    moveQuestionToTimeline(questionState);
    updateControlState();
    contentAdded(questionState.card);
  }

  function markQuestionClosed(questionState) {
    if (!questionState?.pending) return;
    const restoreFocus = questionState.restoreFocusOnClose || questionState.card.contains(document.activeElement);
    questionState.restoreFocusOnClose = false;
    if (questionState.autoAdvanceTimer) window.clearTimeout(questionState.autoAdvanceTimer);
    questionState.autoAdvanceTimer = null;
    questionState.pending = false;
    questionState.submitting = false;
    questionState.closing = false;
    questionState.card.removeAttribute("aria-busy");
    setQuestionControlsDisabled(questionState, true);
    removeQuestionFromDock(questionState);
    updateControlState();
    showToast(t("回答界面已关闭"));
    if (restoreFocus) window.requestAnimationFrame(focusComposerIfDesktop);
    contentAdded(questionState.card);
  }

  async function closeQuestion(questionState) {
    if (!questionState?.pending || questionState.submitting || questionState.closing) return;
    questionState.restoreFocusOnClose = questionState.card.contains(document.activeElement);
    questionState.closing = true;
    questionState.error.hidden = true;
    questionState.card.classList.remove("is-error");
    questionState.card.setAttribute("aria-busy", "true");
    questionState.close.replaceChildren(makeIconSlot("loader-circle", "is-spinning"));
    questionState.close.title = t("正在关闭");
    questionState.close.setAttribute("aria-label", t("正在关闭"));
    setQuestionControlsDisabled(questionState, true);
    try {
      await apiRequest(`/api/questions/${encodeURIComponent(questionState.id)}`, { method: "DELETE" });
      if (questionState.pending) markQuestionClosed(questionState);
    } catch (error) {
      if (!questionState.pending) return;
      const restoreFocus = questionState.restoreFocusOnClose;
      questionState.restoreFocusOnClose = false;
      questionState.closing = false;
      questionState.card.removeAttribute("aria-busy");
      questionState.error.textContent = error.message || t("回答界面关闭失败");
      questionState.error.hidden = false;
      questionState.card.classList.add("is-error");
      questionState.close.replaceChildren(makeIconSlot("x"));
      questionState.close.title = t("关闭回答");
      questionState.close.setAttribute("aria-label", t("关闭回答"));
      setQuestionControlsDisabled(questionState, false);
      updateQuestionNavigation(questionState);
      showToast(error.message || t("回答界面关闭失败"), "error");
      if (restoreFocus) window.requestAnimationFrame(() => questionState.close.focus());
      if ((error.status === 404 || error.status === 409) && state.viewSessionId) {
        window.setTimeout(() => loadSessionView(state.viewSessionId, { quiet: true }), 300);
      }
    }
  }

  async function submitQuestion(questionState) {
    if (!questionState.pending || questionState.submitting) return;
    let answers;
    try {
      answers = selectedQuestionAnswers(questionState);
    } catch (error) {
      const page = String(error.message || "").match(/第 (\d+) 项/);
      if (page) setQuestionPage(questionState, Number(page[1]) - 1);
      questionState.error.textContent = error.message;
      questionState.error.hidden = false;
      questionState.card.classList.add("is-error");
      return;
    }
    questionState.submitting = true;
    questionState.error.hidden = true;
    questionState.card.classList.remove("is-error");
    questionState.card.setAttribute("aria-busy", "true");
    questionState.submit.replaceChildren(makeIconSlot("loader-circle", "is-spinning"));
    questionState.submit.title = t("提交中");
    questionState.submit.setAttribute("aria-label", t("提交中"));
    setQuestionControlsDisabled(questionState, true);
    try {
      await apiRequest(`/api/questions/${encodeURIComponent(questionState.id)}/answer`, {
        method: "POST",
        body: JSON.stringify({ answers })
      });
      if (questionState.pending) markQuestionAnswered(questionState, answers);
    } catch (error) {
      if (!questionState.pending) return;
      questionState.submitting = false;
      questionState.card.removeAttribute("aria-busy");
      questionState.error.textContent = error.message || t("回答提交失败");
      questionState.error.hidden = false;
      questionState.card.classList.add("is-error");
      questionState.submit.replaceChildren(makeIconSlot("check"));
      questionState.submit.title = t("提交回答");
      questionState.submit.setAttribute("aria-label", t("提交回答"));
      setQuestionControlsDisabled(questionState, false);
      updateQuestionNavigation(questionState);
      showToast(error.message || t("回答提交失败"), "error");
      if ((error.status === 404 || error.status === 409) && state.viewSessionId) {
        window.setTimeout(() => loadSessionView(state.viewSessionId, { quiet: true }), 300);
      }
    }
  }

  function createQuestion(live, data) {
    clearTypingIndicator(live, { waitingOnly: true });
    const questionId = String(data?.question_id || "");
    if (!questionId) return null;
    if (live.questions.has(questionId)) return live.questions.get(questionId);
    ensureLiveArticle(live);
    breakLiveText(live);
    finalizeLiveReasoning(live);
    live.contextOperation = null;
    const questions = Array.isArray(data?.questions) ? data.questions : [];
    const card = document.createElement("section");
    card.className = "question-card";
    card.dataset.questionId = questionId;
    const titleId = `live-question-title-${live.questions.size + 1}`;
    card.setAttribute("aria-label", t("待回答问题"));
    const header = document.createElement("header");
    header.hidden = true;
    const icon = document.createElement("span");
    icon.className = "question-icon";
    icon.appendChild(makeIconSlot("circle-help"));
    const headerCopy = document.createElement("div");
    const status = document.createElement("small");
    status.textContent = t("等待回答");
    const title = document.createElement("strong");
    title.id = titleId;
    title.textContent = questions.length === 1 ? String(questions[0]?.header || t("补充确认")) : t("{count} 项补充确认", {count: questions.length});
    headerCopy.append(status, title);
    header.append(icon, headerCopy);
    const form = document.createElement("form");
    form.className = "question-form";
    const heading = document.createElement("div");
    heading.className = "question-heading";
    const prompt = document.createElement("p");
    prompt.className = "question-prompt";
    prompt.id = `question-${questionId}-prompt`;
    prompt.setAttribute("aria-live", "polite");
    prompt.setAttribute("aria-atomic", "true");
    prompt.textContent = String(questions[0]?.question || questions[0]?.header || t("问题 1"));
    const navigation = document.createElement("div");
    navigation.className = "question-navigation";
    navigation.setAttribute("role", "group");
    navigation.setAttribute("aria-label", t("问题导航"));
    const previous = document.createElement("button");
    previous.type = "button";
    previous.className = "question-page-button is-previous";
    previous.title = t("上一题");
    previous.setAttribute("aria-label", t("上一题"));
    previous.appendChild(makeIconSlot("chevron-right"));
    const position = document.createElement("span");
    position.className = "question-position";
    // 页码只用数字和斜杠，中英文界面都通用。原来写死「1 of N」，中文界面也显示英文
    //（i18n 门禁只查中文字符串，英文模板查不到，09-24）。
    position.textContent = `1 / ${questions.length}`;
    position.setAttribute("aria-live", "polite");
    const next = document.createElement("button");
    next.type = "button";
    next.className = "question-page-button";
    next.title = t("下一题");
    next.setAttribute("aria-label", t("下一题"));
    next.appendChild(makeIconSlot("chevron-right"));
    const submit = document.createElement("button");
    submit.className = "question-page-button question-submit";
    submit.type = "submit";
    submit.title = t("提交回答");
    submit.setAttribute("aria-label", t("提交回答"));
    submit.hidden = true;
    submit.appendChild(makeIconSlot("check"));
    const close = document.createElement("button");
    close.type = "button";
    close.className = "question-page-button question-close-button";
    close.title = t("关闭回答");
    close.setAttribute("aria-label", t("关闭回答"));
    close.appendChild(makeIconSlot("x"));
    navigation.append(previous, position, next, submit, close);
    heading.append(prompt, navigation);
    form.appendChild(heading);
    const controls = [];
    const pages = [];
    questions.forEach((question, questionIndex) => {
      const fieldset = document.createElement("fieldset");
      fieldset.className = "question-fieldset";
      fieldset.id = `question-${questionId}-page-${questionIndex + 1}`;
      fieldset.setAttribute("aria-labelledby", prompt.id);
      fieldset.hidden = questionIndex !== 0;
      const legend = document.createElement("legend");
      legend.className = "question-legend";
      legend.setAttribute("aria-hidden", "true");
      legend.textContent = String(question?.question || question?.header || t("问题 {index}", {index: questionIndex + 1}));
      fieldset.appendChild(legend);
      const optionList = document.createElement("div");
      optionList.className = "question-options";
      const multiple = Boolean(question?.multiple);
      const inputType = multiple ? "checkbox" : "radio";
      const inputName = `question-${questionId}-${questionIndex}`;
      const options = [];
      for (const option of Array.isArray(question?.options) ? question.options : []) {
        const label = document.createElement("label");
        label.className = "question-option";
        const input = document.createElement("input");
        input.type = inputType;
        input.name = inputName;
        input.value = String(option?.label || "");
        input.dataset.questionIndex = String(questionIndex);
        const optionCopy = document.createElement("span");
        optionCopy.className = "question-option-copy";
        const optionLabel = document.createElement("strong");
        optionLabel.textContent = String(option?.label || "");
        optionCopy.appendChild(optionLabel);
        if (String(option?.description || "")) {
          const description = document.createElement("small");
          description.textContent = String(option.description);
          optionCopy.appendChild(description);
        }
        label.append(input, optionCopy);
        optionList.appendChild(label);
        options.push({ input, label, value: String(option?.label || "") });
      }
      fieldset.appendChild(optionList);
      let custom = null;
      if (question?.custom !== false) {
        const wrapper = document.createElement("div");
        wrapper.className = "custom-answer";
        const toggle = document.createElement("input");
        toggle.type = inputType;
        toggle.name = inputName;
        toggle.value = "__custom__";
        toggle.dataset.questionIndex = String(questionIndex);
        toggle.setAttribute("aria-label", t("{header}使用自定义回答", {header: question?.header || t("问题 {index}", {index: questionIndex + 1})}));
        const textarea = document.createElement("textarea");
        textarea.rows = 1;
        textarea.placeholder = t("自定义回答");
        textarea.setAttribute("aria-label", t("{header}的自定义回答", {header: question?.header || t("问题 {index}", {index: questionIndex + 1})}));
        textarea.addEventListener("focus", () => {
          toggle.checked = true;
          updateQuestionOptionClasses(questionState);
        });
        textarea.addEventListener("input", () => {
          toggle.checked = Boolean(textarea.value.trim());
          updateQuestionOptionClasses(questionState);
        });
        let customNext = null;
        if (!multiple) {
          customNext = document.createElement("button");
          customNext.type = "button";
          customNext.className = "custom-answer-next";
          customNext.title = t("下一题");
          customNext.setAttribute("aria-label", t("下一题"));
          customNext.hidden = true;
          customNext.appendChild(makeIconSlot("chevron-right"));
          customNext.addEventListener("click", () => advanceQuestion(questionState));
        }
        wrapper.append(toggle, textarea);
        if (customNext) wrapper.appendChild(customNext);
        fieldset.appendChild(wrapper);
        custom = { wrapper, toggle, textarea, next: customNext };
      }
      form.appendChild(fieldset);
      pages.push(fieldset);
      controls.push({ multiple, options, custom });
    });
    const error = document.createElement("p");
    error.className = "question-error";
    error.setAttribute("role", "alert");
    error.hidden = true;
    form.appendChild(error);
    const summary = document.createElement("dl");
    summary.className = "question-answer-summary";
    summary.hidden = true;
    card.append(header, form, summary);
    const questionState = {
      id: questionId,
      runId: live.runId,
      questions,
      card,
      header,
      titleId,
      form,
      controls,
      pages,
      pageIndex: 0,
      prompt,
      position,
      previous,
      next,
      icon,
      status,
      submit,
      close,
      error,
      summary,
      timelineParent: live.blocks,
      pending: true,
      submitting: false,
      closing: false,
      restoreFocusOnClose: false,
      autoAdvanceTimer: null,
      answers: null
    };
    form.querySelectorAll("input").forEach((input) => input.addEventListener("change", () => {
      updateQuestionOptionClasses(questionState);
      const questionIndex = Number(input.dataset.questionIndex);
      const control = questionState.controls[questionIndex];
      if (!input.checked || input.value === "__custom__" || control?.multiple || questionIndex >= questionState.pages.length - 1) return;
      window.clearTimeout(questionState.autoAdvanceTimer);
      questionState.autoAdvanceTimer = window.setTimeout(() => {
        questionState.autoAdvanceTimer = null;
        if (questionState.pageIndex !== questionIndex || !input.checked) return;
        advanceQuestion(questionState);
      }, 120);
    }));
    previous.addEventListener("click", () => setQuestionPage(questionState, questionState.pageIndex - 1, { focus: true }));
    next.addEventListener("click", () => advanceQuestion(questionState));
    close.addEventListener("click", () => closeQuestion(questionState));
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      submitQuestion(questionState);
    });
    live.questions.set(questionId, questionState);
    // 离屏 live 的问题卡先游离,切回时 reattachLiveArticles 归位。
    if (liveViewed(live)) elements.questionDock.appendChild(card);
    updateQuestionDock();
    setQuestionPage(questionState, 0);
    updateQuestionOptionClasses(questionState);
    updateControlState();
    contentAdded(live);
    return questionState;
  }

  function endPendingQuestions(live, message) {
    for (const question of live.questions.values()) {
      if (!question.pending) continue;
      if (question.autoAdvanceTimer) window.clearTimeout(question.autoAdvanceTimer);
      question.autoAdvanceTimer = null;
      question.pending = false;
      question.submitting = false;
      question.closing = false;
      question.restoreFocusOnClose = false;
      question.card.removeAttribute("aria-busy");
      question.card.classList.add("is-error");
      question.status.textContent = t("本轮已结束");
      question.error.textContent = message;
      question.error.hidden = false;
      setQuestionControlsDisabled(question, true);
      removeQuestionFromDock(question);
    }
  }

  function createContextOperation(live, kind) {
    ensureLiveArticle(live);
    clearTypingIndicator(live, { waitingOnly: true });
    breakLiveText(live);
    finalizeLiveReasoning(live);
    const block = document.createElement("section");
    block.className = "context-operation";
    const title = document.createElement("strong");
    title.append(makeIconSlot("refresh-cw"), document.createElement("span"));
    title.lastChild.textContent = kind === "compact" ? t("正在整理上下文") : t("正在释放旧上下文");
    const output = document.createElement("pre");
    output.hidden = true;
    block.append(title, output);
    const operation = { kind, block, title: title.lastChild, output, raw: "" };
    procLineBreak(live.blocks);
    live.blocks.appendChild(block);
    syncBubbleWidth(live.article);
    live.contextOperation = operation;
    contentAdded(live);
    return operation;
  }

  function handleContextEvent(name, live, data) {
    if (name === "context.compact_start") createContextOperation(live, "compact");
    else if (name === "context.compact_delta") {
      const operation = live.contextOperation?.kind === "compact" ? live.contextOperation : createContextOperation(live, "compact");
      operation.raw = boundedAppend(operation.raw, String(data?.delta || ""));
      operation.output.textContent = operation.raw;
      operation.output.hidden = !operation.raw;
    } else if (name === "context.compact_end") {
      if (live.contextOperation?.kind === "compact") live.contextOperation.title.textContent = t("上下文已整理");
      live.contextOperation = null;
    } else if (name === "context.pop_start") createContextOperation(live, "pop");
    else if (name === "context.pop_end") {
      if (live.contextOperation?.kind === "pop") live.contextOperation.title.textContent = t("旧上下文已释放");
      live.contextOperation = null;
    } else if (name === "context.error") {
      const operation = live.contextOperation || createContextOperation(live, "compact");
      operation.block.classList.add("is-error");
      operation.title.textContent = t("上下文整理未完成");
      operation.raw = String(data?.message || t("上下文维护失败"));
      operation.output.textContent = operation.raw;
      operation.output.hidden = false;
      live.contextOperation = null;
    }
    contentAdded(live);
  }

  function jobStatusDisplay(status) {
    const value = String(status || "");
    if (value === "stopped") return t("已中断");
    if (value === "timed_out") return t("已超时");
    if (value === "exited(signal)") return t("异常退出");
    if (value === "exited(0)") return t("完成");
    const match = value.match(/^exited\((-?\d+)\)$/);
    return match ? t("退出码 {code}", {code: match[1]}) : value;
  }

  function visibleBackgroundJobs() {
    // 会话隔离: 状态条只显示当前查看会话的任务(无会话标记的旧任务保持可见)。
    // 后代(子代理/孙代理)的后台任务也列:它们归自己的会话,树根是当前会话(09-18 会话化)。
    return Array.from(state.backgroundJobs.values()).filter(
      (job) => !job.session_id || !state.viewSessionId
        || job.session_id === state.viewSessionId || job.root_session_id === state.viewSessionId
    );
  }

  // 盲文点阵转圈 spinner(09-12 用户指定):一个全局 ticker 刷所有 .job-braille
  // 的字符,避免每行各自 CSS 动画在任务条重建时被打回起点。
  // 空心盲文点阵转圈(和会话列表 BRAILLE_FRAMES 同款),不是之前那组实心的
  // ⣾⣽⣻…(09-12 #2 用户指出实心不对)。
  const JOB_BRAILLE = BRAILLE_FRAMES;
  let jobBrailleFrame = 0;

  function makeJobSpinner() {
    // 左侧标记槽:默认点阵 spinner,鼠标悬浮时原地换成展开/收起箭头
    //(09-12 #8b 用户要求,和子代理一样)。展开态箭头旋转 180°。
    const slot = document.createElement("span");
    slot.className = "job-chip-marker-slot";
    const s = document.createElement("span");
    s.className = "job-chip-marker job-braille";
    s.textContent = JOB_BRAILLE[jobBrailleFrame];
    slot.appendChild(s);
    slot.appendChild(makeIconSlot("chevron-down", "job-chip-chevron"));
    return slot;
  }

  setInterval(() => {
    if (document.hidden) return;
    const nodes = elements.jobsStrip?.querySelectorAll(".job-braille");
    if (!nodes || !nodes.length) return;
    jobBrailleFrame = (jobBrailleFrame + 1) % JOB_BRAILLE.length;
    const frame = JOB_BRAILLE[jobBrailleFrame];
    nodes.forEach((node) => {
      node.textContent = frame;
    });
  }, 110);

  // 后台命令没有实时进度流,展开那行时拉日志尾巴看输出(09-12 用户报「命令无法
  // 点击展开看输出」);运行中每 1.5s 轮询一次,退出即停。
  function commandLogPanel(jobId) {
    let entry = state.commandLogs.get(jobId);
    if (!entry) {
      const panel = document.createElement("div");
      panel.className = "job-stream-panel job-log-panel";
      const pre = document.createElement("pre");
      pre.className = "job-log-pre";
      pre.textContent = "…";
      panel.appendChild(pre);
      entry = { panel, pre, timer: null };
      state.commandLogs.set(jobId, entry);
    }
    return entry;
  }

  async function refreshCommandLog(jobId) {
    const entry = state.commandLogs.get(jobId);
    if (!entry) return;
    try {
      // apiRequest 返回的是 Response,得再 .json()(09-12 #8a 命令永远「暂无输出」
      // 的真凶:直接把 Response 当 JSON 用,data.log 恒为 undefined)。
      const resp = await apiRequest(`/api/jobs/${encodeURIComponent(jobId)}/log`);
      const data = await resp.json();
      const atBottom = entry.pre.scrollTop + entry.pre.clientHeight >= entry.pre.scrollHeight - 8;
      entry.pre.textContent = data?.log || t("(暂无输出)");
      if (atBottom) entry.pre.scrollTop = entry.pre.scrollHeight;
      if (!data?.running && entry.timer) {
        clearInterval(entry.timer);
        entry.timer = null;
      }
    } catch {
      entry.pre.textContent = t("(读取日志失败)");
    }
  }

  // 后台命令的窥视(#120):轮询日志尾行,取最后一条非空行喂给状态行窥视。命令没有
  // 进度流,但输出全在日志里,尾行就是「它现在在干嘛」。行会随任务条重建而换元素,
  // 所以 timer 里每次都从当前 DOM 找回该 job 的窥视 span。
  function trackCommandPeek(jobId) {
    if (state.commandPeekTimers.has(jobId)) return;
    const tick = async () => {
      const job = state.backgroundJobs.get(jobId);
      const running = job && job.running;
      try {
        const resp = await apiRequest(`/api/jobs/${encodeURIComponent(jobId)}/log`);
        const data = await resp.json();
        const lines = String(data?.log || "").split("\n").map((l) => l.trimEnd()).filter(Boolean);
        const last = lines.length ? lines[lines.length - 1] : "";
        if (last) {
          state.commandPeekLine.set(jobId, last);
          const el = elements.jobsStrip?.querySelector(`.job-chip[data-job-id="${CSS.escape(jobId)}"] .job-chip-peek > span`);
          if (el) setReasoningPeek(el, last);
        }
        if (data?.running === false) stop();
      } catch { /* 忽略,下次再试 */ }
      if (!running) stop();
    };
    const stop = () => {
      const t = state.commandPeekTimers.get(jobId);
      if (t) clearInterval(t);
      state.commandPeekTimers.delete(jobId);
    };
    tick();
    state.commandPeekTimers.set(jobId, setInterval(tick, 1500));
  }

  function renderJobsStrip() {
    const strip = elements.jobsStrip;
    if (!strip) return;
    const jobs = visibleBackgroundJobs();
    // 并行任务数首次达到收缩阈值(≥3)时自动收起成「后台任务 ×N」一行(#11):
    // 从 <3 跨到 ≥3 的那一刻强制收起(刷新时 prev=0 也算跨越),之后用户手动展开保留。
    const prevJobCount = state.prevJobCount || 0;
    state.prevJobCount = jobs.length;
    if (jobs.length >= 3 && prevJobCount < 3) state.jobsStripOpen = false;
    if (!jobs.length) {
      strip.hidden = true;
      strip.replaceChildren();
      updateJumpButtonOffset();
      return;
    }
    const fragment = document.createDocumentFragment();
    const collapsible = jobs.length >= 3;
    if (collapsible) {
      // 合并行做成和单行一样的 job-chip 外观(09-12 用户报):braille spinner +
      // 「后台任务 ×N」+ 展开箭头,不再是另一种带 ▸ 前缀的按钮。
      const toggle = document.createElement("div");
      toggle.className = state.jobsStripOpen ? "job-chip is-toggle is-open" : "job-chip is-toggle";
      toggle.setAttribute("role", "button");
      toggle.setAttribute("aria-expanded", String(state.jobsStripOpen));
      const label = document.createElement("span");
      label.className = "job-chip-label";
      label.textContent = t("后台任务 ×{count}", {count: jobs.length});
      toggle.append(makeJobSpinner(), label);
      toggle.addEventListener("click", () => {
        state.jobsStripOpen = !state.jobsStripOpen;
        // 收起「后台任务 ×N」合并行时,把里面所有已展开的状态行 + 思考/工具卡
        // 一并收起(09-12 #15),不留展开残留。
        if (!state.jobsStripOpen) state.expandedJobs.clear();
        localStorage.setItem("miyu.web.jobsStripOpen", state.jobsStripOpen ? "1" : "0");
        renderJobsStrip();
      });
      fragment.appendChild(toggle);
    }
    const showRows = !collapsible || state.jobsStripOpen;
    for (const job of showRows ? jobs : []) {
      const jid = String(job.job_id);
      // kind 是 "subagent" 或 "dev"(开发模式子代理单列一类,见 jobs::kind_label);
      // 原来只认前者,开发模式的后台子代理被当成命令画:标签「命令」、点开是日志
      // 面板而不是子过程时间线(用户 09-18 截图)。
      const isSubagent = job.kind === "subagent" || job.kind === "dev";
      const row = document.createElement("div");
      row.className = "job-chip is-expandable";
      row.dataset.jobId = jid;

      const label = document.createElement("span");
      label.className = "job-chip-label";
      const kindWord = isSubagent ? (job.dev ? t("开发中") : t("子代理")) : t("命令");
      // 后代的任务前面挂个 ↳,看得出不是这一层开的。
      const nested = job.root_session_id && job.session_id && job.root_session_id !== job.session_id;
      label.textContent = `${kindWord} ${nested ? "↳ " : ""}${job.job_id} · ${job.title}`;
      label.title = label.textContent;

      // 行窥视:跑到工具显示工具、跑到思考窥思考,单行滚动刷新(仅子代理有进度流,
      // 命令没有进度流所以窥视留空)。标题保持完整、不被窥视替换。
      const peekSlot = document.createElement("span");
      peekSlot.className = "job-chip-peek reasoning-peek";
      const peek = document.createElement("span");
      peekSlot.appendChild(peek);

      const token = document.createElement("span");
      token.className = "job-chip-token";

      const time = document.createElement("span");
      time.className = "job-chip-time";
      const seconds = job.running
        ? Math.max(0, Math.round(job.runtime_seconds + (Date.now() - job.receivedAt) / 1000))
        : job.runtime_seconds;
      time.textContent = formatJobDuration(seconds);

      const stop = document.createElement("button");
      stop.type = "button";
      stop.className = "job-chip-stop";
      stop.textContent = "✕";
      stop.title = t("停止该后台任务");
      stop.addEventListener("click", async (event) => {
        event.stopPropagation();
        try {
          await apiRequest(`/api/jobs/${encodeURIComponent(jid)}`, { method: "DELETE" });
        } catch (error) {
          showToast(error.message || t("停止失败"), "error");
        }
      });

      // 布局(09-12 #2):节点 · 标题 · token 秒数 · <淡出过渡> 窥视(撑开右对齐) · ✕。
      // 标题贴左 hug、token/时间紧跟其后,窥视占满余下空间、左侧淡出滚动,不再让标题
      // flex 撑开把窥视顶到最右留下大空档(#12)。展开箭头合进左侧标记槽。
      row.append(makeJobSpinner(), label, token, time, peekSlot, stop);

      if (isSubagent) {
        // 窥视、词元取 `job.progress` 里收好的那份(会话项目第 4 段之二)。
        const status = state.subagentJobs.get(jid) || {};
        status.peekEl = peek;
        status.tokenEl = token;
        if (job.child_session_id) status.session = String(job.child_session_id);
        state.subagentJobs.set(jid, status);
        if (status.peek) setReasoningPeek(peek, status.peek);
        if (status.tokens) token.textContent = status.tokens;
        else if (job.metric) token.textContent = String(job.metric);
      } else {
        // 后台命令没有进度流,但有输出日志(#120):把日志尾行当窥视,轮询刷新;
        // 先用已缓存的尾行填上(重建行时不闪)。
        if (state.commandPeekLine?.has(jid)) setReasoningPeek(peek, state.commandPeekLine.get(jid));
        if (job.running) trackCommandPeek(jid, peek);
      }

      const expanded = state.expandedJobs.has(jid);
      row.classList.toggle("is-open", expanded);
      row.setAttribute("aria-expanded", String(expanded));
      row.addEventListener("click", (event) => {
        if (event.target.closest(".job-chip-stop")) return;
        // 后台子代理:打开它的会话(和终端任务条口径一致),过程在那儿看。子会话还没建好
        // (刚派出去那一瞬)就什么都不做。
        if (isSubagent) {
          const session = state.subagentJobs.get(jid)?.session;
          if (session) window.MiyuSubagents?.open(session);
          return;
        }
        if (state.expandedJobs.has(jid)) {
          state.expandedJobs.delete(jid);
        } else {
          state.expandedJobs.add(jid);
        }
        renderJobsStrip();
      });

      const wrap = document.createElement("div");
      wrap.className = "job-chip-wrap";
      wrap.appendChild(row);
      if (expanded && !isSubagent) {
        const entry = commandLogPanel(jid);
        wrap.appendChild(entry.panel);
        refreshCommandLog(jid);
        if (job.running && !entry.timer) {
          entry.timer = setInterval(() => refreshCommandLog(jid), 1500);
        }
      } else if (!isSubagent) {
        const entry = state.commandLogs.get(jid);
        if (entry?.timer) {
          clearInterval(entry.timer);
          entry.timer = null;
        }
      }
      fragment.appendChild(wrap);
    }
    strip.replaceChildren(fragment);
    strip.hidden = false;
    updateJumpButtonOffset();
  }

  /// 时分秒:`12s` / `1m 05s` / `1h 02m 05s`,和终端的 `format_hms` 一个写法。
  function formatHms(seconds) {
    const value = Math.max(0, Math.floor(seconds));
    const hours = Math.floor(value / 3600);
    const minutes = Math.floor((value % 3600) / 60);
    const rest = value % 60;
    if (hours > 0) return `${hours}h ${String(minutes).padStart(2, "0")}m ${String(rest).padStart(2, "0")}s`;
    if (minutes > 0) return `${minutes}m ${String(rest).padStart(2, "0")}s`;
    return `${rest}s`;
  }

  function formatJobDuration(seconds) {
    const value = Math.max(0, Math.floor(seconds));
    if (value >= 3600) return `${Math.floor(value / 3600)}h ${String(Math.floor((value % 3600) / 60)).padStart(2, "0")}m`;
    if (value >= 60) return `${Math.floor(value / 60)}m ${String(value % 60).padStart(2, "0")}s`;
    return `${value}s`;
  }

  async function seedJobsStrip() {
    try {
      // apiRequest 返回 Response,得再 .json()(与 #8a 命令日志同一坑:直接把
      // Response 当 JSON,data.jobs 恒为 undefined → 刷新后一个后台任务都存不进,
      // 状态行整条消失。job.started 只在开跑那一刻发,刷新后不重放,全靠这里补拉)。
      const data = await (await apiRequest("/api/jobs")).json();
      state.backgroundJobs.clear();
      for (const job of data?.jobs || []) {
        const jid = String(job.job_id);
        state.backgroundJobs.set(jid, { ...job, receivedAt: Date.now() });
      }
      renderJobsStrip();
    } catch {
      /* daemon may predate the jobs API */
    }
  }

  setInterval(() => {
    if (document.hidden) return;
    const visible = visibleBackgroundJobs();
    if (!visible.length) return;
    // 只更新计时文本：全量重建会重启 CSS 旋转动画，导致 spinner 每秒瞬移回原点。
    let missing = false;
    for (const job of visible) {
      const row = elements.jobsStrip?.querySelector(`.job-chip[data-job-id="${CSS.escape(String(job.job_id))}"]`);
      if (!row) {
        missing = true;
        continue;
      }
      const time = row.querySelector(".job-chip-time");
      if (!time) continue;
      const seconds = Math.max(0, Math.round(job.runtime_seconds + (Date.now() - job.receivedAt) / 1000));
      time.textContent = formatJobDuration(seconds);
    }
    if (missing && (state.jobsStripOpen || visible.length < 3)) renderJobsStrip();
  }, 1000);
  setTimeout(seedJobsStrip, 800);

  // 回到前台补一刀(09-12 #9:手机切到别的程序再切回,后台期间任务完成了却不刷新;
  // #3:刷新/断连回来状态行没了)。手机后台久了系统会掐断 SSE 且不自动重连,所以:
  // 连接死了就按 lastEventId 重连、补拉后台任务;当前没有在跑的直播时静默补同步一次
  // 会话,追回后台期间错过的完成事件(有直播在跑就不动,免得打断流式重挂)。
  let lastVisibleResync = 0;
  document.addEventListener("visibilitychange", () => {
    if (document.hidden || state.blocked) return;
    const src = state.eventSource;
    const dead = !src || src.readyState === EventSource.CLOSED;
    if (dead) connectEventSource(state.lastEventId || 0);
    seedJobsStrip();
    // 只有 SSE 真的断过(切走太久被系统掐了)才补同步会话:SSE 一直连着就没漏事件,
    // 没必要重建整个对话。长对话整段 loadSessionView 很重,每次切回前台都重建正是
    // 「滚动中切回来渲染丢失/卡死」的诱因(09-12 #13:visibilitychange 无条件重建)。
    if (!dead) return;
    const now = Date.now();
    if (now - lastVisibleResync < 1500) return;
    lastVisibleResync = now;
    if (!conversationRunning() && state.viewSessionId && !state.viewLoading) {
      loadSessionView(state.viewSessionId, { quiet: true });
    }
  });

  function appendRunNotice(live, message, error = false) {
    ensureLiveArticle(live);
    clearTypingIndicator(live);
    breakLiveText(live);
    const notice = document.createElement("div");
    notice.className = `run-notice${error ? " is-error" : ""}`;
    notice.append(makeIconSlot(error ? "circle-alert" : "circle-stop"));
    const text = document.createElement("span");
    // 报错现在是多行的（一句结论 + 每个端点一行 + 一句该怎么办，见 BUG-16），
    // 顶上那句和明细得分开显示；`flex` 默认是竖向居中，多行时图标要顶对齐。
    text.textContent = String(message || "");
    if (text.textContent.includes("\n")) notice.classList.add("is-multiline");
    notice.appendChild(text);
    procLineBreak(live.blocks);
    live.blocks.appendChild(notice);
  }

  function markUnfinishedTools(live) {
    for (const tool of live.tools.values()) {
      if (tool.finished) continue;
      tool.finished = true;
      tool.finishedAt = performance.now();
      updateToolStatus(tool, t("已中断"), "circle-alert", "is-failure");
      updateToolSummary(tool);
      if (tool.liveProgress) {
        if (tool.liveProgress.textContent.trim()) tool.liveProgress.classList.add("is-error");
        else tool.liveProgress.hidden = true;
        tool.progressDetail.wrapper.hidden = !tool.progressDetail.raw;
        syncBubbleWidth(live.article);
      }
      if (!state.toolExpanded) {
        tool.card.classList.add("collapsed");
        tool.head.setAttribute("aria-expanded", "false");
      }
    }
  }

  function setLiveEndpoint(live, providerId, model) {
    const values = [providerId, model].map((value) => String(value || "").trim()).filter(Boolean);
    live.providerId = String(providerId || "");
    live.model = String(model || "");
    if (!live.endpoint) return;
    live.endpoint.textContent = values.join(" / ");
    live.endpoint.hidden = !state.display?.show_mixed_model_endpoint || values.length === 0;
  }

  function consumeLiveQueue(live, data) {
    finalizeLiveReasoning(live);
    procLineBreak(live.blocks);
    setLiveEndpoint(live, data?.provider_id, data?.model);
    if (live.headerStatus) live.headerStatus.textContent = "";
    // followup 插在步与步之间时,前一段末尾不再打「已完成」那条带背景的小字
    // (09-12 用户报没必要):中间段没有独立用量可报,留空并隐藏那行。
    if (live.meta) {
      live.meta.textContent = "";
      live.meta.hidden = true;
    }

    const ids = new Set((Array.isArray(data?.prompt_ids) ? data.prompt_ids : []).map(String));
    const consumed = state.queuedPrompts.filter((prompt) => ids.has(String(prompt?.id)));
    state.queuedPrompts = state.queuedPrompts.filter((prompt) => !ids.has(String(prompt?.id)));
    for (const prompt of consumed) {
      appendUserMessage(elements.timeline, prompt?.content || "", prompt?.submitted_at || new Date(), {
        turnId: live.turnId,
        runId: live.runId,
        followupId: prompt?.id,
        attachments: prompt?.attachments
      });
    }
    renderQueueTray();

    stashLiveArticle(live, "segment");
    removeLiveStopButton(live);
    live.article = null;
    live.blocks = null;
    live.headerStatus = null;
    live.meta = null;
    live.endpoint = null;
    live.copyButton = null;
    live.streamRail = null;
    live.typingAnimation = null;
    live.currentText = null;
    live.assistantText = "";
    live.assistantReasoning = "";
    live.reasoning = null;
    live.reasoningParts = [];
    live.reasoningStarted = false;
    live.reasoningTitle = "";
    live.tools = new Map();
    live.questions = new Map();
    live.contextOperation = null;
    showTypingIndicator(live);
    contentAdded(live);
  }

  function updateLocalTurnFromLive(live, terminalStatus, data) {
    const status = terminalStatus === "completed" ? "completed" : "interrupted";
    let turn = live.turnId ? state.turns.find((item) => String(item?.id) === String(live.turnId)) : null;
    if (!turn && (live.userText || live.userAttachments.length)) {
      turn = {
        id: live.turnId || `local-${live.runId}`,
        seq: state.turns.length ? Math.max(...state.turns.map((item) => asFiniteNumber(item?.seq))) + 1 : 1,
        status,
        active_context: true,
        user_content: live.userText,
        assistant_content: live.assistantText,
        assistant_reasoning: live.assistantReasoning || null,
        provider_id: data?.provider_id || live.providerId || null,
        model: data?.model || live.model || null,
        user_timestamp: new Date().toISOString(),
        assistant_timestamp: new Date().toISOString(),
        token_total: effectiveUsageTotal(data?.usage),
        token_usage_estimated: Boolean(data?.usage_estimated),
        question_exchanges: [],
        followups: [],
        assets: [...live.assets],
        artifacts: [...live.artifacts],
        attachments: [...live.userAttachments]
      };
      state.turns.push(turn);
    } else if (turn) {
      turn.status = status;
      if (live.assistantText.trim()) turn.assistant_content = live.assistantText;
      if (live.assistantReasoning.trim()) turn.assistant_reasoning = live.assistantReasoning;
      if (data?.provider_id || live.providerId) turn.provider_id = data?.provider_id || live.providerId;
      if (data?.model || live.model) turn.model = data?.model || live.model;
      if (live.assets.length) turn.assets = [...live.assets];
      if (live.artifacts.length) turn.artifacts = [...live.artifacts];
      turn.assistant_timestamp = new Date().toISOString();
      if (terminalStatus === "completed") {
        turn.token_total = effectiveUsageTotal(data?.usage);
        turn.token_usage_estimated = Boolean(data?.usage_estimated);
      }
    }
  }

  // 回合内一次模型请求结束(chat.round_usage):立即刷新气泡计量与上下文
  // 条,不等 run 完结。usage 是刚结束请求的用量,其 prompt+completion 即
  // 当前上下文占用;turn_* 是回合累计。回合结束后 finishLiveRun 会用权威
  // 数字覆盖这里的中间值。
  function handleRoundUsage(live, data) {
    if (live.meta) {
      const usage = formatUsageMeta({
        turnTotal: asFiniteNumber(data?.turn_total),
        turnPrompt: data?.turn_prompt,
        turnCached: data?.turn_cache_read,
        estimated: data?.estimated,
        generationTokens: data?.turn_generation_tokens,
        generationMs: data?.turn_generation_ms
      });
      if (usage) live.meta.textContent = usage;
    }
    // 输入框下方那个「累计」逐请求刷新(#131:以前只有 run.completed 才刷,子代理跑
    // 完的花销要等整回合结束才体现)。后端现在每个主回合都带会话实时累计。
    state.cumulativeBase = {
      total: asFiniteNumber(data?.cumulative_tokens),
      prompt: asFiniteNumber(data?.cumulative_prompt_tokens),
      cached: asFiniteNumber(data?.cumulative_cache_read_tokens),
    };
    // 已跑完的子代理:基线确实涨上来把它算进去了才摘掉那份估算(见 absorbDoneSubagents,
    // 躲开后端竞态导致的掉数)。
    absorbDoneSubagents(state.cumulativeBase.total);
    refreshComposerCumulative({
      speed: generationSpeedValue(data?.turn_generation_tokens, data?.turn_generation_ms),
    });
    const round = data?.usage;
    const contextTokens = asFiniteNumber(round?.prompt_tokens, 0) + asFiniteNumber(round?.completion_tokens, 0);
    if (contextTokens > 0) {
      state.context.tokens = contextTokens;
      updateContext();
    }
  }

  // 输入框「累计」的合成:基线(后端每回合 / 收尾给的会话实时累计)+ 正在跑的子代理
  // 的实时估算之和(#131)。子代理还没落库的花销靠估算先顶上、跑完由下个主回合的
  // 基线接管;并行子代理各自更新自己那一份,这里只求个和、按 rAF 合并刷,不会鬼畜抖。
  function composerCumulativeTokens() {
    const base = state.cumulativeBase || null;
    if (!base || !(base.total > 0)) return null;
    let extra = 0;
    for (const v of state.liveSubagentTokens?.values() || []) extra += asFiniteNumber(v?.tokens);
    const total = base.total + extra;
    return { total, prompt: base.prompt, cached: base.cached };
  }
  // 收尾:把「已跑完」的子代理估算从合成里摘掉——但只在权威基线确实已经把它算进来
  // 之后才摘(基线比标记完成时涨了至少估算的一半)。否则会撞上后端竞态:子代理刚跑完、
  // 它的用量还没落库进会话累计,唤醒回合的 round_usage 先带了一个不含它的基线过来,
  // 这会儿摘掉估算 = 累计瞬间掉一大块(#131 后台子代理实测到的掉数)。等基线真涨上来
  // 再摘,既不掉也不会和基线重复计。
  function absorbDoneSubagents(newBaseTotal) {
    for (const [id, entry] of state.liveSubagentTokens) {
      if (!entry?.done) continue;
      const grewBy = asFiniteNumber(newBaseTotal) - asFiniteNumber(entry.baseAtDone);
      if (grewBy >= asFiniteNumber(entry.tokens) * 0.5) state.liveSubagentTokens.delete(id);
    }
  }
  function refreshComposerCumulative(opts = {}) {
    const cum = composerCumulativeTokens();
    const payload = {};
    if ("speed" in opts) payload.speed = opts.speed;
    payload.cumulative = cum
      ? `${formatTokens(cum.total)}${cacheSuffix(cum.cached, cum.prompt)}`
      : null;
    setComposerUsage(payload);
  }
  // 「≈1.2k」「498.9K」「1.2万」这类计数文本抠成数值(带 k/m/b/万 单位)。是估算,精度
  // 到单位,足够撑「累计」逐步涨,收尾由后端权威基线纠正。抠不出返回 null。
  function tokensFromCount(text) {
    const m = String(text || "").match(/([\d.]+)\s*([kKmMbB万]?)/);
    if (!m) return null;
    let n = parseFloat(m[1]);
    if (!Number.isFinite(n)) return null;
    const unit = (m[2] || "").toLowerCase();
    if (unit === "k") n *= 1e3;
    else if (unit === "m") n *= 1e6;
    else if (unit === "b") n *= 1e9;
    else if (m[2] === "万") n *= 1e4; // i18n-allow: 数字后缀是数据,程序按原文解析单位
    return Math.round(n);
  }

  function finishLiveRun(kind, data, live) {
    if (!live || live.ended) return;
    const runId = live.runId;
    if (live.operation === "redo" && kind !== "completed") {
      live.ended = true;
      disposeLiveState(live);
      state.liveRuns.delete(runId);
      state.replayRunIds?.delete(runId);
      state.terminalRunIds.add(runId);
      showToast(kind === "failed" ? String(data?.message || t("重新生成失败")) : t("重新生成已取消"), "error");
      if (state.viewSessionId) loadSessionView(state.viewSessionId, { quiet: true });
      updateConversationChrome();
      updateControlState();
      return;
    }
    live.ended = true;
    clearPreparingTool(live);
    clearTypingIndicator(live);
    finalizeLiveReasoning(live);
    procLineBreak(live.blocks);
    setLiveEndpoint(live, data?.provider_id, data?.model);
    removeLiveStopButton(live);
    state.terminalRunIds.add(runId);
    if (state.terminalRunIds.size > 30) state.terminalRunIds.delete(state.terminalRunIds.values().next().value);

    if (kind === "completed") {
      if (live.headerStatus) live.headerStatus.textContent = "";
      if (live.meta) {
        const usage = formatUsageMeta({
          turnTotal: effectiveUsageTotal(data?.usage),
          turnPrompt: data?.usage?.prompt_tokens,
          turnCached: data?.usage?.cache_read_tokens,
          estimated: data?.usage_estimated,
          cumulative: data?.cumulative_tokens,
          cumulativePrompt: data?.cumulative_prompt_tokens,
          cumulativeCached: data?.cumulative_cache_read_tokens,
          generationTokens: data?.usage?.generation_tokens,
          generationMs: data?.usage?.generation_ms
        });
        live.meta.textContent = usage || t("已完成");
      }
      // 输入框下方信息行:最新一轮的输出速度 + 会话累计 token(#99/#131)。收尾时
      // 会话累计是权威值(所有子代理都跑完、子会话都记好了),直接当基线,把中途的
      // 子代理实时估算清空(已被基线接管)。
      state.cumulativeBase = {
        total: asFiniteNumber(data?.cumulative_tokens),
        prompt: asFiniteNumber(data?.cumulative_prompt_tokens),
        cached: asFiniteNumber(data?.cumulative_cache_read_tokens),
      };
      // 只摘「已跑完且基线确实涨上来把它算进去」的子代理估算;仍在跑的后台子代理会活过
      // 父回合,别清;唤醒回合竞态下基线还没含它时也别清(见 absorbDoneSubagents,#131)。
      absorbDoneSubagents(state.cumulativeBase.total);
      refreshComposerCumulative({
        speed: generationSpeedValue(data?.usage?.generation_tokens, data?.usage?.generation_ms),
      });
    } else if (kind === "cancelled") {
      markUnfinishedTools(live);
      endPendingQuestions(live, t("本轮已停止，无法再提交回答"));
      // 停止状态只由时间线的「本轮已中断」一处表达,气泡内通知与 header/meta 不再重复
      if (live.headerStatus) live.headerStatus.textContent = "";
      if (live.meta) live.meta.textContent = "";
    } else {
      markUnfinishedTools(live);
      endPendingQuestions(live, t("本轮已结束，无法再提交回答"));
      appendRunNotice(live, String(data?.message || t("本轮运行失败")), true);
      if (live.headerStatus) live.headerStatus.textContent = t("运行失败");
      if (live.meta) live.meta.textContent = "";
    }

    // 离屏 live 属于别的会话:state.turns 是当前视图的,不能往里塞。
    if (liveViewed(live)) updateLocalTurnFromLive(live, kind, data);
    // 刚起步就被掐掉的轮（目标编辑打断最常见）：气泡里什么都没有，留着就是
    // 一个空壳。丢弃它，让下面的静默重拉用落库的中断轮接管。
    const emptyCancelled = kind === "cancelled"
      && !String(live.assistantText || "").trim()
      && !(live.reasoningParts && live.reasoningParts.length)
      && !(live.tools && live.tools.size);
    const cancelledInView = kind === "cancelled" && data?.session_id
      && String(data.session_id) === String(state.viewSessionId || "");
    const markerTurnId = live.turnId;
    const markerArticle = (!emptyCancelled && live.article?.isConnected) ? live.article : null;
    if (emptyCancelled) {
      disposeLiveState(live);
      state.liveRuns.delete(runId);
    } else {
      stashLiveArticle(live, "final");
    }
    if (cancelledInView) {
      // 中断轮已落库（含部分输出与状态）。以前这里整会话静默重拉，把「本轮已中断」
      // 标记捎带渲染出来——但那次全量重渲染在长对话里就是中断「特别高延迟 / 感觉
      // 加载很久」的由来。改成只在原位补这一条状态行（后端 cancel 事件仅 ~12ms）。
      showInterruptedMarker(markerTurnId, markerArticle);
      // 紧跟着的那次 120ms 后台快照别再整会话重渲染一遍(上面已画对)。
      state.suppressPostCancelRender = true;
    }
    if (kind === "completed" || kind === "cancelled") {
      // 上下文条跟着正在看的会话走（没有视图时退回终端车道）。
      // cancelled 也要刷新：被中断的轮次已经持久化进上下文。
      const updatesGlobalContext = !data?.session_id
        || String(data.session_id) === String(state.viewSessionId || state.currentSessionId || "");
      if (updatesGlobalContext) {
        if (data?.context_tokens != null) state.context.tokens = Math.max(0, asFiniteNumber(data.context_tokens));
        state.context.window = data?.context_window == null ? state.context.window : Math.max(0, asFiniteNumber(data.context_window));
      }
      const usage = data?.usage && typeof data.usage === "object" ? data.usage : null;
      if (usage) {
        state.usage.last_usage = usage;
        state.usage.last_conversation_usage = usage;
        state.usage.requests = asFiniteNumber(state.usage.requests) + 1;
        state.usage.prompt_tokens = asFiniteNumber(state.usage.prompt_tokens) + asFiniteNumber(usage.prompt_tokens);
        state.usage.completion_tokens = asFiniteNumber(state.usage.completion_tokens) + asFiniteNumber(usage.completion_tokens);
        state.usage.total_tokens = asFiniteNumber(state.usage.total_tokens) + effectiveUsageTotal(usage);
        state.usage.cache_read_tokens = asFiniteNumber(state.usage.cache_read_tokens) + asFiniteNumber(usage.cache_read_tokens, 0);
        state.usage.cache_write_tokens = asFiniteNumber(state.usage.cache_write_tokens) + asFiniteNumber(usage.cache_write_tokens, 0);
      }
    }
    state.liveRuns.delete(runId);
    state.replayRunIds?.delete(runId);
    state.pendingSubmission = null;
    updateContext();
    updateRuntimeUsage(data?.usage || null, Boolean(data?.usage_estimated));
    updateConversationChrome();
    updateControlState();
    contentAdded(live);
    if (state.liveRuns.size === 0) {
      window.requestAnimationFrame(() => {
        if (!state.blocked && !consoleIsOpen()) focusComposerIfDesktop();
      });
      window.setTimeout(() => {
        if (state.liveRuns.size === 0) refreshViewSnapshot();
      }, 120);
    }
  }

  function clearViewSyncTimer() {
    if (!state.viewSyncTimer) return;
    window.clearTimeout(state.viewSyncTimer);
    state.viewSyncTimer = null;
  }

  function scheduleViewSync() {
    clearViewSyncTimer();
    if (!state.viewRunningTurnId || state.blocked) return;
    state.viewSyncTimer = window.setTimeout(() => {
      state.viewSyncTimer = null;
      refreshViewSnapshot();
    }, 1_000);
  }

  async function refreshViewSnapshot() {
    const sessionId = state.viewSessionId;
    if (!sessionId || state.blocked || state.viewLoading || state.resyncing) {
      scheduleViewSync();
      return;
    }
    try {
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/turns?limit=${TURN_PAGE_SIZE}`);
      const payload = await response.json();
      if (state.viewSessionId !== sessionId || state.viewLoading) return;
      const runs = (Array.isArray(payload?.runs) ? payload.runs : []).filter((run) => run?.run_id);
      if (runs.length) state.runsBySession.set(sessionId, new Set(runs.map((run) => String(run.run_id))));
      else if (state.liveRuns.size === 0) state.runsBySession.delete(sessionId);
      state.viewRunningTurnId = !runs.length && typeof payload?.running_turn_id === "string" && payload.running_turn_id
        ? payload.running_turn_id
        : null;
      if (state.liveRuns.size === 0) {
        const nextTurns = Array.isArray(payload?.turns)
          ? mergeLatestTurnPage(
            state.turns,
            payload.turns.sort((a, b) => asFiniteNumber(a?.seq) - asFiniteNumber(b?.seq)),
            payload
          )
          : state.turns;
        const turnsChanged = JSON.stringify(nextTurns) !== JSON.stringify(state.turns);
        const nextCandidate = payload?.redo_candidate && typeof payload.redo_candidate === "object"
          ? payload.redo_candidate
          : null;
        const candidateChanged = JSON.stringify(nextCandidate) !== JSON.stringify(state.redoCandidate);
        state.turns = nextTurns;
        state.queuedPrompts = Array.isArray(payload?.queued_prompts) ? payload.queued_prompts : state.queuedPrompts;
        state.redoCandidate = nextCandidate;
        // 刚中断那次不必整会话重渲染(#1):原位补的「本轮已中断」+ 留在原地的直播
        // 气泡已经把最终态画对了,重渲染只是把同样的东西再拼一遍——长对话里这一下
        // 就是中断残留的卡顿。只吞这一次纯 turns 变更;redo 候选变了照常渲染。
        const suppress = state.suppressPostCancelRender && !candidateChanged;
        state.suppressPostCancelRender = false;
        if ((turnsChanged || candidateChanged) && !suppress) renderConversation();
        renderQueueTray();
        restoreLiveRuns(runs);
      }
      renderSessionList();
      updateConversationChrome();
      updateControlState();
    } catch (error) {
      if (error.status === 401) {
        showBlockedState(true);
        return;
      }
      if (error.status === 404) {
        state.viewRunningTurnId = null;
        refreshSessions();
        return;
      }
    } finally {
      scheduleViewSync();
    }
  }

  async function ensureActiveTurnUser(live, turnId) {
    if (!live || live.userRendered || !turnId) return;
    // 离屏 live 不补渲用户消息(下面拉的是当前视图的 turns,张冠李戴)。
    if (!liveViewed(live)) return;
    const existing = state.turns.find((turn) => String(turn?.id) === String(turnId));
    if (existing) {
      live.userText = String(existing.user_content || "");
      live.userRendered = true;
      updateConversationChrome();
      return;
    }
    const sessionId = state.viewSessionId;
    try {
      // 正在跑的那一轮总在最近几轮里，不用整段拉。
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/turns?limit=5`);
      const payload = await response.json();
      if (state.viewSessionId !== sessionId || state.liveRuns.get(live.runId) !== live || live.userRendered) return;
      const turn = Array.isArray(payload?.turns) ? payload.turns.find((item) => String(item?.id) === String(turnId)) : null;
      if (!turn) return;
      live.userText = String(turn.user_content || "");
      live.userAttachments = Array.isArray(turn.attachments) ? turn.attachments : [];
      ensureLiveUser(live, live.userText);
    } catch (_) {
      // The stream can continue; a later view refresh will recover the user turn.
    }
  }

  function handleRunEvent(name, data) {
    const runId = String(data?.run_id || "");
    if (!runId) return;
    const sessionId = typeof data?.session_id === "string" && data.session_id ? data.session_id : runSessionId(runId);
    const terminal = name === "run.completed" || name === "run.cancelled" || name === "run.failed";
    if (name === "run.started" && sessionId) trackRun(sessionId, runId);
    // 正在看的会话不算未读——用户就在现场看着它跑完。
    if (terminal && sessionId && sessionId !== state.viewSessionId) {
      if (!state.unreadSessions.has(sessionId)) {
        state.unreadSessions.add(sessionId);
        renderSessionList();
      }
    }

    let live = state.liveRuns.get(runId);
    if (!live && !terminal && !state.terminalRunIds.has(runId) && sessionId && sessionId === state.viewSessionId) {
      // 视图会话里出现的新 run（本端发起、他端发起或重放）都会挂上 live 块。
      // run.started 意味着全新的 turn，不去认领时间线里已有的 running turn。
      live = createLiveForRun(runId, "", {
        sessionId,
        claimTurn: name !== "run.started",
        operation: String(data?.operation || "create"),
        turnId: String(data?.turn_id || "") || null,
        inputId: String(data?.input_id || "") || null
      });
      if (live.turnId && state.viewRunningTurnId === String(live.turnId)) state.viewRunningTurnId = null;
    }

    if (name === "run.started") {
      if (live) {
        live.operation = String(data?.operation || live.operation || "create");
        live.turnId = String(data?.turn_id || live.turnId || "") || null;
        live.inputId = String(data?.input_id || live.inputId || "") || null;
      }
      if (live && !live.ended && live.operation !== "redo") showTypingIndicator(live);
      renderSessionList();
      updateConversationChrome();
      updateControlState();
      return;
    }
    if (terminal) {
      // 一轮跑完，目标的轮次/阶段可能都变了（模型自己报了完成或受阻）。
      if (sessionId && sessionId === state.viewSessionId) loadGoal(sessionId);
      untrackRun(runId);
      if (live) {
        finishLiveRun(name.slice("run.".length), data, live);
      } else {
        state.terminalRunIds.add(runId);
        if (state.terminalRunIds.size > 30) state.terminalRunIds.delete(state.terminalRunIds.values().next().value);
        if (name === "run.completed" && data?.session_id && String(data.session_id) === String(state.viewSessionId || state.currentSessionId || "")) {
          if (data?.context_tokens != null) state.context.tokens = Math.max(0, asFiniteNumber(data.context_tokens));
          state.context.window = data?.context_window == null ? state.context.window : Math.max(0, asFiniteNumber(data.context_window));
          updateContext();
        }
        renderSessionList();
      }
      return;
    }
    if (!live) return;

    if (name === "turn.started") {
      live.turnId = String(data?.turn_id || "");
      if (live.article) live.article.dataset.turnId = live.turnId;
      if (String(data?.operation || "") === "redo") {
        live.operation = "redo";
        live.inputId = String(data?.input_id || live.inputId || "") || null;
        if (typeof data?.display_content === "string") live.editedContent = data.display_content;
      }
      if (state.viewRunningTurnId === live.turnId) state.viewRunningTurnId = null;
      removeRunningStatus(live.turnId);
      if (live.operation === "redo") commitRedoLive(live);
      else ensureActiveTurnUser(live, live.turnId);
    } else if (name === "assistant.delta") appendAssistantDelta(live, data?.delta);
    else if (name === "chat.round_usage") handleRoundUsage(live, data);
    else if (name === "generation.superseded") resetSupersededGeneration(live);
    else if (name.startsWith("reasoning.")) handleReasoningEvent(name, live, data);
    else if (name === "queue.consumed") consumeLiveQueue(live, data);
    else if (name.startsWith("tool.") || name === "subagent.progress") handleToolEvent(name, live, data);
    else if (name === "question.requested") {
      clearPreparingTool(live);
      createQuestion(live, data);
    }
    else if (name === "question.answered") {
      const question = live.questions.get(String(data?.question_id || ""));
      if (question) markQuestionAnswered(question, data?.answers);
    } else if (name === "question.closed") {
      const question = live.questions.get(String(data?.question_id || ""));
      if (question) markQuestionClosed(question);
    } else if (name.startsWith("context.")) handleContextEvent(name, live, data);
  }

  function eventShouldBeHandled(name, data, eventId) {
    if (name === "resync_required") {
      if (eventId > 0) state.lastEventId = eventId;
      return true;
    }
    if (eventId > 0 && eventId <= state.lastEventId) return false;
    if (eventId > 0) state.lastEventId = eventId;
    if (state.replayRunIds && eventId > 0 && eventId <= state.replayCutoff) {
      // 重放窗口内只重建正在恢复的 run，其余事件已经反映在快照里。
      if (!RUN_EVENTS.has(name)) return false;
      return state.replayRunIds.has(String(data?.run_id || ""));
    }
    if (state.replayRunIds && eventId > state.replayCutoff) state.replayRunIds = null;
    return true;
  }

  function handleSseEvent(name, event) {
    // 登录态没了(blocked)时,一律不再处理 SSE 事件:否则一边弹登录界面、一边还
    // 触发 loadBootstrap/「正在重新同步」的会话重载提示,两个提示重复(09-12 #18)。
    // showBlockedState 已经关了 SSE,这里挡住任何残留在途的事件。
    if (state.blocked) return;
    let data;
    try {
      data = event.data ? JSON.parse(event.data) : {};
    } catch (_) {
      showToast(t("收到无法解析的事件，正在重新同步"), "error");
      loadBootstrap();
      return;
    }
    const eventId = Math.max(0, asFiniteNumber(event.lastEventId));
    if (!eventShouldBeHandled(name, data, eventId)) return;
    if (name === "resync_required") {
      if (state.replayRunIds) {
        state.replayResyncCount += 1;
        state.replayResyncAt = Date.now();
      } else {
        state.replayResyncCount = 0;
      }
      if (!state.resyncing) {
        state.resyncing = true;
        loadBootstrap().finally(() => {
          state.resyncing = false;
        });
      }
      return;
    }
    if (name.startsWith("session.")) {
      handleSessionEvent(name, data);
      return;
    }
    if (name === "queue.added") {
      const prompt = data?.prompt;
      if (queueEventTargetsView(data) && prompt && !state.queuedPrompts.some((item) => String(item?.id) === String(prompt?.id))) {
        state.queuedPrompts.push(prompt);
        renderQueueTray();
      }
      return;
    }
    if (name === "job.started") {
      const job = data?.job;
      if (job?.job_id) {
        state.backgroundJobs.set(String(job.job_id), { ...job, receivedAt: Date.now() });
        renderJobsStrip();
      }
      return;
    }
    if (name === "job.progress") {
      const jobId = String(data?.job_id || "");
      if (jobId && (data?.peek || data?.tokens_label || data?.child_session_id)) {
        const status = state.subagentJobs.get(jobId) || {};
        if (data.peek) status.peek = String(data.peek);
        if (data.tokens_label) status.tokens = String(data.tokens_label);
        if (data.child_session_id) status.session = String(data.child_session_id);
        state.subagentJobs.set(jobId, status);
        if (status.peekEl && status.peek) setReasoningPeek(status.peekEl, status.peek);
        if (status.tokenEl && status.tokens) status.tokenEl.textContent = status.tokens;
      }
      return;
    }
    if (name === "job.finished") {
      const jobId = String(data?.job_id || "");
      // 后台子代理跑完:它的实时估算先「冻住」(别立刻抽走,否则基线还没算进它之前
      // 累计会掉一下),等下个主回合权威基线接管时再删(#131,与前台同款)。
      const entry = state.liveSubagentTokens.get("job:" + jobId);
      if (entry) { entry.done = true; entry.baseAtDone = asFiniteNumber(state.cumulativeBase?.total); refreshComposerCumulative(); }
      state.expandedJobs.delete(jobId);
      state.subagentJobs.delete(jobId);
      if (state.backgroundJobs.delete(jobId)) renderJobsStrip();
      return;
    }
    if (name === "job.acknowledged") {
      const jobId = String(data?.job_id || "");
      state.expandedJobs.delete(jobId);
      state.subagentJobs.delete(jobId);
      if (state.backgroundJobs.delete(jobId)) renderJobsStrip();
      return;
    }
    if (name === "queue.removed") {
      if (queueEventTargetsView(data)) {
        state.queuedPrompts = state.queuedPrompts.filter((prompt) => String(prompt?.id) !== String(data?.prompt_id));
        renderQueueTray();
      }
      return;
    }
    if (name === "conversation.reset" || name === "conversation.pop" || name === "conversation.compacted") {
      const sessionId = typeof data?.session_id === "string" ? data.session_id : "";
      // 清空/压缩/pop 会重排或清零会话累计;把「不下调」用的基线与子代理估算一并清了,
      // 让它按重载后的权威值重新起算(#131:否则 max 会把清零前的旧高值锁住)。
      if (!sessionId || sessionId === state.viewSessionId) {
        state.cumulativeBase = null;
        state.liveSubagentTokens.clear();
      }
      if (sessionId && sessionId !== state.viewSessionId) {
        refreshSessions();
      } else if (!state.viewSessionId || state.viewSessionId === state.currentSessionId) {
        loadBootstrap();
      } else {
        loadSessionView(state.viewSessionId, { quiet: true });
        refreshSessions();
      }
      return;
    }
    handleRunEvent(name, data);
  }

  function queueEventTargetsView(data) {
    const explicit = typeof data?.session_id === "string" && data.session_id ? data.session_id : "";
    if (explicit) return explicit === state.viewSessionId;
    const runId = String(data?.run_id || "");
    if (runId) {
      if (state.liveRuns.has(runId)) return true;
      const sessionId = runSessionId(runId);
      if (sessionId) return sessionId === state.viewSessionId;
    }
    const turnId = String(data?.turn_id || "");
    if (turnId) {
      if (state.viewRunningTurnId && turnId === state.viewRunningTurnId) return true;
      for (const live of state.liveRuns.values()) {
        if (String(live.turnId || "") === turnId) return true;
      }
      return state.turns.some((turn) => String(turn?.id) === turnId && turn?.status === "running");
    }
    return false;
  }

  function closeEventSource() {
    if (state.eventSource) {
      state.eventSource.close();
      state.eventSource = null;
    }
    if (state.healthTimer) {
      window.clearTimeout(state.healthTimer);
      state.healthTimer = null;
    }
  }

  async function refineConnectionHealth(source) {
    if (state.eventSource !== source || source.readyState === EventSource.OPEN) return;
    try {
      const response = await fetch("/api/health", { cache: "no-store", credentials: "same-origin" });
      if (!response.ok) throw new Error("health check failed");
      if (state.eventSource === source && source.readyState !== EventSource.OPEN) setConnectionStatus("connecting");
    } catch (_) {
      if (state.eventSource === source && source.readyState !== EventSource.OPEN) setConnectionStatus("offline");
    }
  }

  function connectEventSource(after) {
    closeEventSource();
    if (state.blocked) return;
    const source = new EventSource(`/api/events?after=${encodeURIComponent(Math.max(0, asFiniteNumber(after)))}`);
    state.eventSource = source;
    source.onopen = () => {
      if (state.eventSource !== source) return;
      setConnectionStatus("online");
      if (state.healthTimer) window.clearTimeout(state.healthTimer);
      state.healthTimer = null;
    };
    source.onerror = () => {
      if (state.eventSource !== source) return;
      setConnectionStatus("connecting");
      if (state.healthTimer) window.clearTimeout(state.healthTimer);
      state.healthTimer = window.setTimeout(() => refineConnectionHealth(source), 1200);
    };
    for (const name of EVENT_NAMES) source.addEventListener(name, (event) => handleSseEvent(name, event));
  }

  function showBlockedState(unauthorized, message = "", { expired = false } = {}) {
    state.blocked = true;
    document.body.classList.toggle("is-login", Boolean(unauthorized));
    document.body.classList.toggle("is-blocked", true);
    state.viewRunningTurnId = null;
    clearViewSyncTimer();
    disposeAllLiveRuns();
    clearQuestionDock();
    closeEventSource();
    elements.loadingState.hidden = true;
    elements.timeline.hidden = true;
    elements.emptyState.hidden = true;
    elements.blockedState.hidden = false;
    elements.blockedTitle.textContent = unauthorized ? t("登录 Miyu") : t("无法载入 Miyu WebUI");
    elements.blockedMessage.textContent = unauthorized
      ? (expired ? t("登录已过期,请重新登录。") : t("输入用户名和密码以继续。"))
      : message || t("本地服务暂时无法访问");
    elements.loginForm.hidden = !unauthorized;
    elements.registerForm.hidden = true;
    elements.setupForm.hidden = true;
    elements.retryBootstrapButton.hidden = unauthorized;
    if (unauthorized) refreshLoginHint();
    elements.loginError.textContent = "";
    elements.loginError.hidden = true;
    elements.registerError.textContent = "";
    elements.registerError.hidden = true;
    setLoginSubmitting(false);
    setRegisterSubmitting(false);
    setConnectionStatus(unauthorized ? "blocked" : "offline");
    updateControlState();
    if (unauthorized) window.requestAnimationFrame(() => elements.loginPassword.focus());
  }

  /// 还没建管理员账号:登录页直说「输入内置口令」;之后就是普通的用户名+密码。
  async function refreshLoginHint() {
    try {
      const status = await fetch("/api/auth/status", { cache: "no-store" }).then((response) => response.json());
      if (!document.body.classList.contains("is-login") || !elements.loginForm || elements.loginForm.hidden) return;
      if (elements.blockedMessage.textContent === t("登录已过期,请重新登录。")) return;
      if (status?.setup_pending) {
        elements.blockedMessage.textContent = t("首次使用:用户名 miyu、密码 miyu 登录,然后创建管理员账号。");
        elements.loginUsername.placeholder = "miyu";
      } else {
        elements.blockedMessage.textContent = t("输入用户名和密码以继续。");
        elements.loginUsername.placeholder = t("用户名");
      }
    } catch (_) { /* 提示拿不到就用默认文案 */ }
  }

  /// 引导第 0 步:拿内置口令登进来、还没有管理员账号——先建号,建完直接以它登录。
  function showSetupAdmin() {
    state.blocked = true;
    document.body.classList.add("is-login", "is-blocked");
    elements.loadingState.hidden = true;
    elements.timeline.hidden = true;
    elements.emptyState.hidden = true;
    elements.blockedState.hidden = false;
    elements.blockedTitle.textContent = t("创建管理员账号");
    elements.blockedMessage.textContent = t("内置账号 miyu 只用这一次;建好账号后用它登录,别人凭邀请码注册。");
    elements.loginForm.hidden = true;
    elements.registerForm.hidden = true;
    elements.setupForm.hidden = false;
    elements.retryBootstrapButton.hidden = true;
    elements.setupError.textContent = "";
    elements.setupError.hidden = true;
    if (!elements.setupUsername.value) elements.setupUsername.value = state.account?.setup_username || "";
    setConnectionStatus("blocked");
    updateControlState();
    window.requestAnimationFrame(() => (elements.setupUsername.value ? elements.setupPassword : elements.setupUsername).focus());
  }

  async function submitSetupAdmin() {
    if (state.setupSubmitting) return;
    const username = elements.setupUsername.value.trim();
    const password = elements.setupPassword.value;
    const fail = (text, focus) => { elements.setupError.textContent = text; elements.setupError.hidden = false; focus?.focus(); };
    if (!username) return fail(t("先起个用户名"), elements.setupUsername);
    if (!password) return fail(t("请输入密码"), elements.setupPassword);
    if (password !== elements.setupPassword2.value) return fail(t("两次密码不一样"), elements.setupPassword2);
    elements.setupError.hidden = true;
    state.setupSubmitting = true;
    elements.setupSubmit.disabled = true;
    try {
      await apiRequest("/api/auth/setup-admin", {
        method: "POST",
        body: JSON.stringify({ username, display_name: elements.setupDisplayName.value.trim(), password }),
      });
      elements.setupPassword.value = "";
      elements.setupPassword2.value = "";
      await loadBootstrap();
    } catch (error) {
      fail(error.message || t("创建失败"), elements.setupUsername);
    } finally {
      state.setupSubmitting = false;
      elements.setupSubmit.disabled = false;
    }
  }

  const VIEW_SESSION_KEY = "miyu.web.viewSession";
  // 一次取多少轮：首屏、切会话、运行中每秒同步都只要最近这一页，往上翻到顶再补
  //（会话项目第 2 段；以前每次都整段拉，最重的会话一次约 3 MB）。
  const TURN_PAGE_SIZE = 30;

  /// 页面加载后该打开哪个会话。
  ///
  /// 不能直接用 daemon 的 `current_session`：那个指针归终端车道所有（shellhook
  /// 与 CLI 用它），而终端集成会话在 WebUI 的侧栏里是隐藏的——刷新一下就掉进
  /// 一个列表里根本看不到的会话，看着像「我的对话没了」。
  ///
  /// 顺序：上次浏览的 → 当前指针（如果它在列表里可见）→ 列表第一个。
  function preferredBootSession() {
    const remembered = safeStorageGet(VIEW_SESSION_KEY);
    if (remembered && findSession(remembered) && !isTerminalSession(remembered)) return remembered;
    if (state.currentSessionId
      && findSession(state.currentSessionId)
      && !isTerminalSession(state.currentSessionId)) {
      return state.currentSessionId;
    }
    const visible = state.sessions.find((session) => !isTerminalSession(session?.session_id));
    return visible ? String(visible.session_id) : "";
  }

  function applyBootstrap(snapshot) {
    // 守护进程换了构建(升级后重启):这一页跑的还是旧前端,整页刷新一次(用户 09-24
    // 拍板自动刷新)。旧页少了新功能不会报错,只会悄悄不对——比如没有在线心跳,
    // 别的会话看不见它。重启但没换构建时不刷。
    const buildId = String(snapshot?.build_id || "");
    if (state.buildId && buildId && buildId !== state.buildId) {
      location.reload();
      return;
    }
    if (buildId) state.buildId = buildId;
    if (snapshot?.account?.setup_pending) {
      state.account = snapshot.account;
      showSetupAdmin();
      return;
    }
    state.blocked = false;
    document.body.classList.remove("is-login", "is-blocked");
    clearViewSyncTimer();
    disposeAllLiveRuns();
    state.bootId = String(snapshot?.boot_id || "");
    state.latestEventId = Math.max(0, asFiniteNumber(snapshot?.latest_event_id));
    state.models = Array.isArray(snapshot?.models) ? snapshot.models : [];
    applyPersona(snapshot?.persona);
    state.display = snapshot?.display && typeof snapshot.display === "object" ? snapshot.display : state.display;
    state.context = snapshot?.context && typeof snapshot.context === "object" ? snapshot.context : { tokens: 0, window: null };
    state.usage = snapshot?.usage && typeof snapshot.usage === "object" ? snapshot.usage : {};
      state.capabilities = snapshot?.capabilities && typeof snapshot.capabilities === "object" ? snapshot.capabilities : {};
    state.account = snapshot?.account && typeof snapshot.account === "object" ? snapshot.account : null;
    applyRoleVisibility();
    if (state.account?.oobe_pending && !oobeState.open) window.setTimeout(() => openOobe({ reason: "first" }), 350);
    state.sessions = Array.isArray(snapshot?.sessions) ? snapshot.sessions : [];
    state.currentSessionId = typeof snapshot?.current_session_id === "string" && snapshot.current_session_id ? snapshot.current_session_id : null;
    state.sessionMenuFor = null;
    state.sessionRenaming = null;
    state.version = snapshot?.version ?? null;
    state.pendingSubmission = null;
    const allRuns = (Array.isArray(snapshot?.runs) ? snapshot.runs : []).filter((run) => run?.run_id && run?.session_id);
    state.runsBySession = new Map();
    for (const run of allRuns) trackRun(String(run.session_id), String(run.run_id));
    elements.loginForm.hidden = true;
    elements.registerForm.hidden = true;
    elements.setupForm.hidden = true;
    elements.retryBootstrapButton.hidden = false;
    elements.loginPassword.value = "";
    elements.loginError.textContent = "";
    elements.loginError.hidden = true;
    setLoginSubmitting(false);
    elements.versionLabel.textContent = state.version ? `v${state.version}` : "--";
    clearInlineError();
    renderModelMenu();
    updateCapabilities();
    updateContext();
    state.replayRunIds = null;
    state.replayCutoff = 0;
    const boot = preferredBootSession();
    if (boot && boot !== state.viewSessionId) state.viewSessionId = boot;
    const keepView = state.viewSessionId && state.viewSessionId !== state.currentSessionId && findSession(state.viewSessionId);
    if (keepView) {
      // 视图停留在非默认会话：全局重载不改变浏览位置，改用会话接口回填。
      state.lastEventId = state.latestEventId;
      connectEventSource(state.latestEventId);
      loadSessionView(state.viewSessionId, { quiet: true });
    } else if (state.currentSessionId && !isTerminalSession(state.currentSessionId)) {
      applySessionView({
        session_id: state.currentSessionId,
        turns: snapshot?.turns,
        queued_prompts: snapshot?.queued_prompts,
        running_turn_id: snapshot?.running_turn_id,
        runs: allRuns.filter((run) => String(run.session_id) === String(state.currentSessionId)),
        redo_candidate: snapshot?.redo_candidate,
        older: snapshot?.older,
        tokens_before: snapshot?.tokens_before,
        first_user_content: snapshot?.first_user_content
      });
      if (state.liveRuns.size === 0) {
        state.lastEventId = state.latestEventId;
        connectEventSource(state.latestEventId);
      }
    } else {
      // 单会话兜底：没有会话指针时直接使用 bootstrap 快照。指针指着隐藏的
      // 终端车道时快照里的 turns 属于那条车道，画出来就是把隐藏会话泄漏给
      // WebUI——那种情况按空状态处理。
      const hiddenLane = isTerminalSession(state.currentSessionId);
      state.viewSessionId = null;
      state.sessionModelOverride = null;
      state.sessionModelOverrideFor = "";
      updateCurrentModelDisplay();
      state.viewRunningTurnId = !hiddenLane && typeof snapshot?.running_turn_id === "string" && snapshot.running_turn_id ? snapshot.running_turn_id : null;
      state.turns = !hiddenLane && Array.isArray(snapshot?.turns) ? snapshot.turns.sort((a, b) => asFiniteNumber(a?.seq) - asFiniteNumber(b?.seq)) : [];
      state.queuedPrompts = !hiddenLane && Array.isArray(snapshot?.queued_prompts) ? snapshot.queued_prompts : [];
      state.redoCandidate = !hiddenLane && snapshot?.redo_candidate && typeof snapshot.redo_candidate === "object"
        ? snapshot.redo_candidate
        : null;
      renderConversation({ forceScroll: true });
      renderQueueTray();
      state.lastEventId = state.latestEventId;
      connectEventSource(state.latestEventId);
    }
    setConnectionStatus("connecting");
    updateRuntimeUsage();
    updateConversationChrome();
    updateControlState();
    loadThinkingVariants();
  }

  async function loadBootstrap() {
    if (state.bootstrapPromise) return state.bootstrapPromise;
    state.bootstrapPromise = (async () => {
      clearViewSyncTimer();
      closeEventSource();
      state.adminBusy = false;
      state.submitting = false;
      if (!state.turns.length && state.liveRuns.size === 0) {
        elements.loadingState.hidden = false;
        elements.blockedState.hidden = true;
        elements.emptyState.hidden = true;
        elements.timeline.hidden = true;
      }
      setConnectionStatus("connecting");
      updateControlState();
      try {
        const response = await apiRequest("/api/bootstrap");
        const snapshot = await response.json();
        applyBootstrap(snapshot);
        // 认证过了才拉外观偏好:未登录时这个接口本来就该 401。
        syncUiPrefs();
        // 命令清单与麦克风状态同理:WebUI 永远要登录(09-11),页面初始化那次
        // 拿到的是 401,登录之后必须重拿,否则 /reset /compact 全都当普通消息发出去。
        if (!state.blocked) {
          window.MiyuCommands?.load(apiRequest);
          refreshVoiceButton();
        }
      } catch (error) {
        showBlockedState(error.status === 401, error.message);
      }
    })();
    try {
      await state.bootstrapPromise;
    } finally {
      state.bootstrapPromise = null;
    }
  }

  /// 成员看不到管理台(供应商/密钥、共享人格、脚本、QQ、记忆库……),
  /// 只留数据统计(自己的)与账号页。没开口令时人人都是管理员。
  function isAdmin() {
    return state.capabilities?.admin !== false;
  }

  function applyRoleVisibility() {
    const admin = isAdmin();
    const multiUser = Boolean(state.capabilities?.multi_user);
    for (const element of document.querySelectorAll("[data-admin-only]")) element.hidden = !admin;
    for (const element of document.querySelectorAll("[data-multi-user-only]")) element.hidden = !multiUser;
    for (const element of document.querySelectorAll("[data-member-only]")) element.hidden = admin || !multiUser;
    // 成员的记忆/知识库/表情包/记账面板跟当前人格开了什么走(服务端算好的清单)。
    const dashboards = Array.isArray(state.account?.persona?.dashboards) ? state.account.persona.dashboards : [];
    for (const panel of ["memory", "kb", "memes", "ledger"]) {
      const item = elements.consoleView.querySelector(`.con-rail-item[data-console-panel="${panel}"]`);
      if (item) item.hidden = !admin && !dashboards.includes(panel);
    }
    if (!admin && consoleIsOpen() && isAdminOnlyPanel(state.consolePanel)) setConsolePanel("usage");
  }

  function isAdminOnlyPanel(panel) {
    const item = elements.consoleView.querySelector(`.con-rail-item[data-console-panel="${panel}"]`);
    return Boolean(item?.hasAttribute("data-admin-only") || item?.hidden);
  }

  function showRegisterForm(show) {
    elements.loginForm.hidden = show;
    elements.registerForm.hidden = !show;
    elements.blockedMessage.textContent = show ? t("凭管理员发的邀请码创建账号。") : t("输入用户名和密码以继续。");
    window.requestAnimationFrame(() => (show ? elements.registerInvite : elements.loginUsername).focus());
  }

  function setRegisterSubmitting(submitting) {
    state.registerSubmitting = Boolean(submitting);
    for (const input of [elements.registerInvite, elements.registerUsername, elements.registerDisplayName, elements.registerPassword]) {
      input.disabled = state.registerSubmitting;
    }
    elements.registerSubmit.disabled = state.registerSubmitting;
    elements.registerSubmit.classList.toggle("is-loading", state.registerSubmitting);
    elements.registerSubmitLabel.textContent = state.registerSubmitting ? t("正在注册") : t("注册并登录");
  }

  async function submitRegister() {
    if (state.registerSubmitting) return;
    const invite = elements.registerInvite.value.trim();
    const username = elements.registerUsername.value.trim();
    const display_name = elements.registerDisplayName.value.trim();
    const password = elements.registerPassword.value;
    const fail = (message, input) => {
      elements.registerError.textContent = message;
      elements.registerError.hidden = false;
      input?.focus();
    };
    if (!invite) return fail(t("请输入邀请码"), elements.registerInvite);
    if (!username) return fail(t("请输入用户名"), elements.registerUsername);
    if (!password) return fail(t("请输入密码"), elements.registerPassword);
    elements.registerError.hidden = true;
    setRegisterSubmitting(true);
    try {
      await apiRequest("/api/auth/register", {
        method: "POST",
        body: JSON.stringify({ invite, username, display_name, password })
      });
      elements.registerPassword.value = "";
      elements.registerInvite.value = "";
      await loadBootstrap();
    } catch (error) {
      fail(error.message || t("注册失败"), elements.registerInvite);
    } finally {
      setRegisterSubmitting(false);
    }
  }

  async function logout() {
    try {
      await apiRequest("/api/auth/logout", { method: "POST" });
    } catch (_) {
      // 令牌已失效也一样回到登录页
    }
    if (consoleIsOpen()) consoleClose();
    showBlockedState(true);
  }

  function setLoginSubmitting(submitting) {
    state.loginSubmitting = Boolean(submitting);
    elements.loginUsername.disabled = state.loginSubmitting;
    elements.loginPassword.disabled = state.loginSubmitting;
    elements.loginSubmit.disabled = state.loginSubmitting;
    elements.loginSubmit.classList.toggle("is-loading", state.loginSubmitting);
    elements.loginSubmitLabel.textContent = state.loginSubmitting ? t("正在登录") : t("登录");
    const icon = elements.loginSubmit.querySelector(".icon-slot");
    if (icon) icon.replaceChildren(createIcon(state.loginSubmitting ? "loader-circle" : "log-in"));
  }

  async function submitLogin() {
    if (state.loginSubmitting) return;
    const username = elements.loginUsername.value.trim();
    const password = elements.loginPassword.value;
    if (!username) {
      elements.loginError.textContent = t("请输入用户名");
      elements.loginError.hidden = false;
      elements.loginUsername.focus();
      return;
    }
    if (!password) {
      elements.loginError.textContent = t("请输入密码");
      elements.loginError.hidden = false;
      elements.loginPassword.focus();
      return;
    }
    elements.loginError.textContent = "";
    elements.loginError.hidden = true;
    setLoginSubmitting(true);
    try {
      await apiRequest("/api/auth/login", {
        method: "POST",
        body: JSON.stringify({ username, password })
      });
      elements.loginPassword.value = "";
      await loadBootstrap();
    } catch (error) {
      elements.loginError.textContent = error.status === 401
        ? t("用户名或密码不正确，请重试")
        : error.message || t("登录失败");
      elements.loginError.hidden = false;
      window.requestAnimationFrame(() => {
        elements.loginPassword.focus();
        elements.loginPassword.select();
      });
    } finally {
      setLoginSubmitting(false);
    }
  }

  /// 把面板里改过的思考档位一次写回。档位是**这个会话**钉住的(09-24:effort 做成
  /// 会话级,null = 拔掉钉子、跟随全局),和会话的模型选择是两份存储,所以是两次请求;
  /// 这里先写档位——它失败了就整个确认中止,不会出现「模型换了但档位没跟上」的半套状态。
  async function commitStagedVariants(sessionId) {
    if (!(state.stagedVariants instanceof Map)) return;
    const updates = [];
    for (const model of state.thinkingVariantModels) {
      const key = modelKey(model);
      if (!state.stagedVariants.has(key)) continue;
      const desired = state.stagedVariants.get(key);
      if (desired === sessionPinFor(key)) continue;
      updates.push({ provider_id: model.provider_id, model: model.model, selected: desired });
    }
    if (!updates.length) return;
    const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/thinking-variants`, {
      method: "PUT",
      body: JSON.stringify({ updates })
    });
    const payload = await response.json();
    setSessionThinkingPins(sessionId, payload?.pinned);
  }

  async function confirmModelSelection() {
    if (!(state.stagedModelKeys instanceof Set) || state.modelSelectionSubmitting) return;
    const sessionId = String(state.viewSessionId || state.currentSessionId || "");
    if (!sessionId) {
      state.modelMenuError = t("当前视图没有可设置的会话");
      updateModelMenuState();
      return;
    }
    const follow = state.stagedFollowGlobal || state.stagedModelKeys.size === 0;
    const selected = follow ? [] : state.models.filter((model) => state.stagedModelKeys.has(modelKey(model)));
    if (!follow && selected.length === 0) {
      state.modelMenuError = t("所选模型已不可用，请重新选择");
      updateModelMenuState();
      return;
    }
    state.modelSelectionSubmitting = true;
    state.modelMenuError = "";
    clearInlineError();
    updateModelMenuState();
    let applied = false;
    try {
      await commitStagedVariants(sessionId);
      const response = await apiRequest(`/api/sessions/${encodeURIComponent(sessionId)}/models`, {
        method: "PUT",
        body: JSON.stringify({
          models: selected.map((model) => ({
            provider_id: String(model.provider_id || ""),
            model: String(model.model || "")
          }))
        })
      });
      const payload = await response.json();
      applied = true;
      state.modelSelectionSubmitting = false;
      closeModelMenu();
      setSessionModelOverride(sessionId, payload?.model_override);
      // 换了模型池,窗口大小也跟着换;不拉的话上下文条要到跑完一轮才纠正。
      refreshSessionContext(sessionId);
      showToast(follow ? t("本会话已恢复跟随全局") : t("本会话模型已更新（下一轮生效）"));
    } catch (error) {
      state.modelMenuError = error.message || t("模型设置未保存");
      showInlineError(error.message);
      showToast(error.message, "error");
    } finally {
      state.modelSelectionSubmitting = false;
      updateControlState();
      if (applied) window.requestAnimationFrame(() => elements.modelButton.focus());
      else {
        updateModelMenuState();
        window.requestAnimationFrame(() => elements.modelMenu.querySelector(".model-confirm")?.focus());
      }
    }
  }

  async function submitTurn() {
    if (state.adminBusy || state.submitting || state.blocked) return;
    if (hasPendingQuestion()) return;
    const sessionId = state.viewSessionId;
    const queueing = conversationRunning();
    const updateTarget = queueing ? activeTurnUpdateTarget(sessionId) : null;
    // 只有确定了追加目标才走 /api/queue;否则(在跑但目标不唯一/还没定,常见于
    // 子代理执行中——手机端尤甚)改走 /api/turns,由后端按会话排进当前在跑的轮
    // (09-12 #10:手机端子代理执行时新消息/followup 发不出)。
    const canQueue = queueing && !!updateTarget;
    const content = elements.composerInput.value.trim();
    // 命中命令表就当命令执行，不当消息发。不命中的 `/xxx` 照常发给模型
    // ——与 REPL 同一语义（slash_commands::parse_repl_input）。
    if (window.MiyuCommands?.match(content)) {
      window.MiyuCommands.hide();
      // 同一条命令不能重入。命令往往要等服务端干完活（/reset 要清库、/compact
      // 要重算上下文），这期间用户看不出回车生效没有，很自然会再敲一次。
      if (state.commandRunning) return;
      state.commandRunning = true;
      // **先**清输入框，再去跑。原来是跑完才清，命令跑多久输入框就挂着原文
      // 多久——看着就像回车没反应，于是连按几次、连触发几次。
      elements.composerInput.value = "";
      resizeComposer();
      updateControlState();
      let handled = false;
      try {
        handled = await window.MiyuCommands.tryRun(content, {
          apiRequest,
          sessionId: state.viewSessionId,
          mode: viewSessionEntry()?.mode === "dev" ? "dev" : "normal",
          redraw: renderConversation,
          // 目标状态行不在对话流里，重绘对话动不到它。
          reloadGoal: () => loadGoal(state.viewSessionId),
          toast: (text) => showToast(text),
          // /stop：停掉当前视图里正在跑的回复；返回空串表示没有在跑的。
          stopRun: async () => {
            const live = [...state.liveRuns.values()].find((entry) => entry && !entry.ended);
            if (!live) return "";
            await cancelLiveRun(live);
            return live.cancellationRequested ? t("已请求停止当前回复") : "";
          },
          // 命令改了服务端状态（/reset 清空历史）时用它重拉，光重绘不够。
          reload: async () => {
            if (state.viewSessionId && state.viewSessionId !== state.currentSessionId) {
              await loadSessionView(state.viewSessionId, { quiet: true });
            } else {
              await loadBootstrap();
            }
          },
          // 敲命令那一刻排在最后的回合（含还在流式输出的）。回执插在它之后，
          // 之后来的新回合就不会把回执顶下去。
          anchorTurnId: commandAnchorTurnId(),
          // /pop、/compact 这类要重排上下文的命令不能插在运行中的回合上。
          // 只看当前查看的会话:别的会话在跑不该挡这里的 /reset /compact /pop(09-10 沙盒实测)
          isRunning: () => conversationRunning(),
          // /pop 无参数时的轮次多选器。
          openPopPicker: () => openPopPicker(),
        });
      } finally {
        state.commandRunning = false;
        updateControlState();
      }
      if (handled) return;
      // 命令表里有、却没被处理：把原文还给用户，别让它凭空消失。
      elements.composerInput.value = content;
      resizeComposer();
    }
    const readyAttachments = state.composerAttachments.filter((item) => item.status === "ready");
    const attachmentIds = readyAttachments.map((item) => item.id);
    const sentAttachments = readyAttachments.map((item) => ({
      id: item.id,
      url: item.url,
      name: item.name,
      mime: item.mime,
      kind: item.kind,
      size: item.size,
      width: item.width || 0,
      height: item.height || 0
    }));
    const count = countCharacters(content);
    if (!content && !attachmentIds.length) {
      elements.composerState.textContent = t("消息不能为空");
      elements.composerState.classList.add("is-error");
      return;
    }
    if (count > MAX_CONTENT_CHARS) {
      elements.composerState.textContent = t("消息不能超过 20,000 个字符");
      elements.composerState.classList.add("is-error");
      return;
    }
    state.submitting = true;
    if (!queueing) state.pendingSubmission = { content, attachments: sentAttachments };
    clearInlineError();
    updateControlState();
    try {
      const body = canQueue
        ? { content, run_id: updateTarget.runId, turn_id: updateTarget.turnId, attachment_ids: attachmentIds }
        : { content, attachment_ids: attachmentIds };
      if (sessionId) body.session_id = sessionId;
      const response = await apiRequest(canQueue ? "/api/queue" : "/api/turns", {
        method: "POST",
        body: JSON.stringify(body)
      });
      const payload = await response.json();
      const queuedPrompt = canQueue ? payload : payload?.queued ? payload.prompt : null;
      if (queuedPrompt) {
        if (!state.queuedPrompts.some((prompt) => String(prompt?.id) === String(queuedPrompt?.id))) {
          state.queuedPrompts.push(queuedPrompt);
        }
        state.pendingSubmission = null;
        elements.composerInput.value = "";
        committedComposerAttachments();
        resizeComposer();
        renderQueueTray();
        // 自己发的消息就该看着它:哪怕之前上滚过,也回到底部
        scrollToBottom({ force: true, smooth: true });
        if (!queueing) {
          // 服务端发现该会话已有 turn 在运行并自动转排队：同步该 run 的 live 状态。
          const runningRunId = String(payload?.run_id || "");
          if (runningRunId && sessionId) {
            trackRun(sessionId, runningRunId);
            if (!state.liveRuns.has(runningRunId) && !state.terminalRunIds.has(runningRunId)) {
              createLiveForRun(runningRunId);
              beginRunReplay();
            }
          } else {
            state.viewRunningTurnId = String(payload?.running_turn_id || "") || state.viewRunningTurnId;
            scheduleViewSync();
          }
          renderSessionList();
          updateConversationChrome();
        }
        return;
      }
      const runId = String(payload?.run_id || "");
      if (!runId) throw new ApiError(t("服务未返回运行标识"), response.status);
      if (state.terminalRunIds.has(runId)) {
        if (sessionId) await loadSessionView(sessionId, { quiet: true });
        else await loadBootstrap();
      } else {
        if (sessionId) trackRun(sessionId, runId);
        const live = createLiveForRun(runId, content);
        live.userText = content;
        live.userAttachments = sentAttachments;
        ensureLiveUser(live, content);
        showTypingIndicator(live);
        elements.composerInput.value = "";
        committedComposerAttachments();
        resizeComposer();
        // 自己发的消息就该看着它:哪怕之前上滚过,也回到底部
        scrollToBottom({ force: true, smooth: true });
        updateRuntimeUsage();
        updateConversationChrome();
        renderSessionList();
      }
    } catch (error) {
      if (!queueing) state.pendingSubmission = null;
      // 409 = 后端认为这个会话已经在跑，而前端以为没有。原文案（「正在同步」
      // ＋「请重新发送」）把机器的调度问题说成用户该重来一遍，而且说了两遍。
      // 现在只留一条，说清楚发生了什么。
      // 排队请求 409 = 盯着的那条轮已经跑完/被顶替,会话此刻空闲。别再弹
      // 「再发一次」让用户重来——直接改走 /api/turns 起一条新轮,消息不丢
      // (/api/turns 会自动排队或新建,09-12 用户报「排队消息却提示要等」)。
      if (canQueue && error.status === 409) {
        try {
          const body = { content, attachment_ids: attachmentIds };
          if (sessionId) body.session_id = sessionId;
          const retry = await apiRequest("/api/turns", { method: "POST", body: JSON.stringify(body) });
          const payload = await retry.json();
          const qp = payload?.queued ? payload.prompt : null;
          if (qp && !state.queuedPrompts.some((p) => String(p?.id) === String(qp?.id))) {
            state.queuedPrompts.push(qp);
          }
          elements.composerInput.value = "";
          committedComposerAttachments();
          resizeComposer();
          renderQueueTray();
          if (sessionId) await loadSessionView(sessionId, { quiet: true });
          else await loadBootstrap();
          return;
        } catch (retryError) {
          showToast(retryError.message || t("发送失败"), "error");
        }
      } else if (error.status === 409) {
        showToast(t("这条没发出去：会话刚开始新的一轮，再发一次"), "error");
      } else {
        showInlineError(error.message);
        showToast(error.message, "error");
      }
      if (error.status === 409) {
        if (sessionId) await loadSessionView(sessionId, { quiet: true });
        else await loadBootstrap();
      }
    } finally {
      state.submitting = false;
      updateControlState();
    }
  }

  function hasHistory() {
    for (const live of state.liveRuns.values()) {
      if (live.userRendered) return true;
    }
    return state.turns.length > 0 || Boolean(elements.timeline.querySelector(".user-message"));
  }

  function openResetDialog() {
    if (typeof elements.resetDialog.showModal === "function") elements.resetDialog.showModal();
    else elements.resetDialog.setAttribute("open", "");
    window.requestAnimationFrame(() => elements.resetCancelButton.focus());
  }

  function openModeChooser() {
    if (state.modeChooserOpen) return;
    state.modeChooserOpen = true;
    updateControlState();
    const overlay = document.createElement("div");
    overlay.className = "mode-chooser-overlay";
    overlay.id = "modeChooserOverlay";
    const panel = document.createElement("div");
    panel.className = "mode-chooser";
    panel.setAttribute("role", "dialog");
    panel.setAttribute("aria-label", t("选择新会话模式"));
    const title = document.createElement("strong");
    title.textContent = t("新会话");
    const hint = document.createElement("small");
    hint.textContent = t("选择模式后开始对话；会话模式创建后不可更改");
    panel.append(title, hint);
    const options = [
      { id: "normal", label: t("普通模式"), icon: "message-circle", desc: t("人格、记忆、全部工具") },
      { id: "dev", label: t("开发模式"), icon: "code", desc: t("极简提示词与编码工具，记忆独立") }
    ];
    for (const option of options) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "mode-chooser-option";
      button.dataset.mode = option.id;
      button.appendChild(makeIconSlot(option.icon));
      const copy = document.createElement("span");
      copy.className = "mode-chooser-copy";
      const label = document.createElement("strong");
      label.textContent = option.label;
      const desc = document.createElement("small");
      desc.textContent = option.desc;
      copy.append(label, desc);
      button.appendChild(copy);
      button.addEventListener("click", () => {
        closeModeChooser();
        closeSidebar();
        createSession(option.id);
      });
      panel.appendChild(button);
    }
    overlay.addEventListener("click", (event) => {
      if (event.target === overlay) closeModeChooser();
    });
    overlay.appendChild(panel);
    document.body.appendChild(overlay);
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeModeChooser();
      }
    };
    state.modeChooserKeyHandler = onKey;
    document.addEventListener("keydown", onKey, true);
    window.requestAnimationFrame(() => panel.querySelector("button")?.focus());
  }

  function closeModeChooser() {
    if (!state.modeChooserOpen) return;
    state.modeChooserOpen = false;
    if (state.modeChooserKeyHandler) {
      document.removeEventListener("keydown", state.modeChooserKeyHandler, true);
      state.modeChooserKeyHandler = null;
    }
    document.getElementById("modeChooserOverlay")?.remove();
    updateControlState();
  }

  function activeSessionMode() {
    const session = findSession(state.viewSessionId);
    return session?.mode === "dev" ? "dev" : "normal";
  }

  function requestNewConversation() {
    if (multiSessionEnabled()) {
      openModeChooser();
      return;
    }
    closeSidebar();
    if (!hasHistory()) {
      focusComposerIfDesktop();
      return;
    }
    if (conversationRunning() || state.adminBusy || state.submitting) return;
    openResetDialog();
  }

  function requestClearConversation() {
    if (conversationRunning() || state.adminBusy || state.submitting) return;
    if (!hasHistory()) {
      showToast(t("当前会话没有可清除的记录"));
      return;
    }
    openResetDialog();
  }

  async function resetConversation() {
    if (conversationRunning() || state.adminBusy || state.submitting) return;
    state.adminBusy = true;
    elements.resetConfirmButton.disabled = true;
    elements.resetCancelButton.disabled = true;
    elements.resetConfirmButton.textContent = t("正在清除");
    updateControlState();
    try {
      if (!state.viewSessionId) throw new Error(t("无法确定要清除的会话"));
      await apiRequest("/api/conversation/reset", {
        method: "POST",
        body: JSON.stringify({ session_id: state.viewSessionId })
      });
      if (elements.resetDialog.open) elements.resetDialog.close("confirmed");
      await loadBootstrap();
      focusComposerIfDesktop();
    } catch (error) {
      showInlineError(error.message);
      showToast(error.message, "error");
      if (error.status === 409) await loadBootstrap();
    } finally {
      state.adminBusy = false;
      elements.resetConfirmButton.disabled = false;
      elements.resetCancelButton.disabled = false;
      elements.resetConfirmButton.textContent = t("清空记录");
      updateControlState();
    }
  }

  /// 光标是不是已经在某个能打字的地方。
  ///
  /// `contenteditable` 也算——artifact 的源码视图和将来的富文本都是它,漏判
  /// 会让 `/` 快捷键在用户正打字时抢走焦点。
  function typingSomewhere() {
    const node = document.activeElement;
    if (!node) return false;
    if (node.isContentEditable) return true;
    const tag = node.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
  }

  function handleGlobalKeydown(event) {
    // `/` 直接跳到输入框(YouTube 那套)。只聚焦,不把斜杠本身送进去——
    // 快捷键是「跳过去」,不是「替我打一个字」;真要发命令,落到输入框之后
    // 再敲一次 `/` 就行,那一下会正常触发命令菜单。
    if (event.key === "/"
      && !event.ctrlKey && !event.metaKey && !event.altKey
      && !typingSomewhere()
      && !state.blocked
      && !consoleIsOpen()
      && !window.MiyuLightbox?.isOpen()
      && !elements.resetDialog.open
      && !elements.composerInput.disabled) {
      event.preventDefault();
      elements.composerInput.focus();
      const at = elements.composerInput.value.length;
      elements.composerInput.setSelectionRange(at, at);
      return;
    }
    if (event.key === "Escape") {
      if (elements.resetDialog.open) return;
      if (!elements.artifactResourceMenu.hidden) {
        event.preventDefault();
        closeArtifactResourceMenu();
        elements.artifactTitleButton.focus();
        return;
      }
      if (state.sessionMenuFor) {
        event.preventDefault();
        closeSessionMenu();
        return;
      }
      if (!elements.modelMenu.hidden) {
        event.preventDefault();
        closeModelMenu({ restoreFocus: true });
        return;
      }
      if (settingsIsOpen()) {
        event.preventDefault();
        closeSettings();
        return;
      }
      if (state.artifactOpen) {
        event.preventDefault();
        if (state.artifactMaximized) {
          toggleArtifactMaximized();
          return;
        }
        setArtifactWorkspaceOpen(false);
        elements.artifactToggleButton.focus();
        return;
      }
      if (elements.sidebar.classList.contains("open")) {
        event.preventDefault();
        closeSidebar();
        state.sidebarOpener?.focus?.();
      }
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k" && !event.shiftKey && !event.altKey) {
      event.preventDefault();
      requestNewConversation();
    }
  }

  /* ───────────────────────── 控制台 · 数据统计 ─────────────────────────
     数据源:GET /api/usage/stats?range= 与 /api/usage/details。
     图表全部手写 DOM/SVG,与整站同一套 token,离线自包含。 */
  const usageState = {
    range: "1d",
    stats: null,
    loadSeq: 0,
    platformTab: null,
    // 用途筛选:按来源分桶(src → all|main|<kind>)。全局一个值时,选中某个
    // 细项会把没有该细项的另一张卡清空(08-26 审查)。
    kindFilters: new Map(),
    modelColors: new Map(),
  };
  const USAGE_COLOR_VARS = ["var(--chart-1)", "var(--chart-2)", "var(--chart-4)", "var(--chart-3)"];
  const usageTip = document.createElement("div");
  usageTip.className = "u-chart-tip";
  document.body.appendChild(usageTip);

  // 可选的花费行:提示气泡里用,抽出来免得模板串里再套模板串。
  const usageCostRow = (cost) => usageFmtCost(cost) ? `<div class="row"><span data-i18n="消费">消费</span><em>≈${usageFmtCost(cost)}</em></div>` : ""; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用

  function usageTipShow(html, event) {
    usageTip.innerHTML = html;
    MiyuI18n.applyDom(usageTip);
    usageTip.style.display = "block";
    usageTipMove(event);
  }
  function usageTipMove(event) {
    const width = usageTip.offsetWidth;
    usageTip.style.left = `${Math.min(window.innerWidth - width - 12, event.clientX + 14)}px`;
    usageTip.style.top = `${Math.max(8, event.clientY - usageTip.offsetHeight - 12)}px`;
  }
  function usageTipHide() {
    usageTip.style.display = "none";
  }

  // 计费估算显示:None/0 → null(不渲染);极小值给足小数位。
  function usageFmtCost(usd) {
    if (!Number.isFinite(usd) || usd <= 0) return null;
    if (usd < 0.01) return `$${usd.toFixed(4)}`;
    if (usd < 1) return `$${usd.toFixed(3)}`;
    if (usd < 100) return `$${usd.toFixed(2)}`;
    return `$${usd.toFixed(1)}`;
  }

  function usageFmt(value) {
    if (value >= 1e9) return `${(value / 1e9).toFixed(2)}B`;
    if (value >= 1e6) return `${(value / 1e6).toFixed(2)}M`;
    if (value >= 1e3) return `${(value / 1e3).toFixed(1)}k`;
    return String(value);
  }
  function usageSourceName(src) {
    if (src === "agent") return t("智能体");
    if (src === "qq" || src === "onebot") return "QQ";
    return src;
  }

  // 来源内细项(后端 kinds):已含在来源合计里,只是拆出来看得见。
  function usageKindName(kind) {
    if (kind === "judge") return t("主动回复判断");
    if (kind === "affection") return t("好感度更新");
    if (kind === "group_join") return t("入群审批");
    return kind;
  }

  // 明细表列窄,用短名;没有短名就退回全名。
  function usageKindShortName(kind) {
    if (kind === "judge") return t("判断");
    if (kind === "affection") return t("好感度");
    if (kind === "group_join") return t("入群");
    return usageKindName(kind);
  }

  /* ── 图表色派生:跟随当前主题(含 matugen /theme.css 覆盖)──
     取 MD3 三色的"色相",明度错位+色度夹取整形成图表专用色;
     环邻 ΔE<15 时沿明度推开。内置双主题的派生结果已过校验脚本。 */
  const usageSrgbToLinear = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const usageLinearToSrgb = (c) => (c <= 0.0031308 ? c * 12.92 : 1.055 * c ** (1 / 2.4) - 0.055);
  function usageHexToOklch(hex) {
    const match = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!match) return null;
    const n = parseInt(match[1], 16);
    const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => usageSrgbToLinear(v / 255));
    const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
    const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
    const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
    const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
    const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
    const bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
    return { L, C: Math.hypot(a, bb), H: (Math.atan2(bb, a) * 180) / Math.PI };
  }
  function usageOklchToHex({ L, C, H }) {
    for (let c = C; c >= 0; c -= 0.004) {
      const h = (H * Math.PI) / 180;
      const a = c * Math.cos(h), bb = c * Math.sin(h);
      const l3 = L + 0.3963377774 * a + 0.2158037573 * bb;
      const m3 = L - 0.1055613458 * a - 0.0638541728 * bb;
      const s3 = L - 0.0894841775 * a - 1.291485548 * bb;
      const [l, m, s] = [l3 ** 3, m3 ** 3, s3 ** 3];
      const r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
      const g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
      const b = -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s;
      if ([r, g, b].every((v) => v >= -1e-4 && v <= 1 + 1e-4)) {
        const to255 = (v) => Math.round(Math.min(1, Math.max(0, usageLinearToSrgb(v))) * 255);
        return `#${[r, g, b].map((v) => to255(v).toString(16).padStart(2, "0")).join("")}`;
      }
    }
    return "#808080";
  }
  function usageOklabDelta(a, b) {
    const rad = (H) => (H * Math.PI) / 180;
    const [aa, ab] = [a.C * Math.cos(rad(a.H)), a.C * Math.sin(rad(a.H))];
    const [ba, bb] = [b.C * Math.cos(rad(b.H)), b.C * Math.sin(rad(b.H))];
    return Math.hypot(a.L - b.L, aa - ba, ab - bb) * 100;
  }
  function updateChartColors() {
    const styles = getComputedStyle(document.body);
    const read = (name) => usageHexToOklch(styles.getPropertyValue(name));
    const primary = read("--md-sys-color-primary");
    const secondary = read("--md-sys-color-secondary");
    const tertiary = read("--md-sys-color-tertiary");
    const surface = read("--md-sys-color-surface");
    if (!primary || !secondary || !tertiary || !surface) return; // 保底用 CSS 静态色
    const dark = surface.L < 0.5;
    const targetL = dark
      ? { c1: 0.60, c2: 0.66, c3: 0.55, c4: 0.64 }
      : { c1: 0.52, c2: 0.56, c3: 0.47, c4: 0.44 };
    const band = dark ? [0.49, 0.67] : [0.43, 0.62];
    const clampC = (c) => Math.min(0.17, Math.max(0.11, c));
    const colors = {
      c1: { L: targetL.c1, C: clampC(primary.C), H: primary.H },
      c2: { L: targetL.c2, C: clampC(secondary.C), H: secondary.H },
      c3: { L: targetL.c3, C: clampC(tertiary.C), H: tertiary.H },
      c4: { L: targetL.c4, C: clampC(primary.C), H: primary.H - 50 },
    };
    const ring = [["c1", "c2"], ["c2", "c4"], ["c4", "c3"], ["c3", "c1"]];
    for (let pass = 0; pass < 8; pass += 1) {
      let adjusted = false;
      for (const [xa, xb] of ring) {
        if (usageOklabDelta(colors[xa], colors[xb]) < 15) {
          const [lo, hi] = colors[xa].L <= colors[xb].L ? [xa, xb] : [xb, xa];
          colors[lo].L = Math.max(band[0], colors[lo].L - 0.025);
          colors[hi].L = Math.min(band[1], colors[hi].L + 0.025);
          adjusted = true;
        }
      }
      if (!adjusted) break;
    }
    ["c1", "c2", "c3", "c4"].forEach((key, index) =>
      document.body.style.setProperty(`--chart-${index + 1}`, usageOklchToHex(colors[key])));
    const top = dark
      ? { L: 0.72, C: Math.min(0.15, clampC(primary.C)) }
      : { L: 0.42, C: Math.min(0.16, clampC(primary.C)) };
    const base = dark
      ? { L: Math.min(0.34, surface.L + 0.06), C: 0.03 }
      : { L: Math.max(0.88, surface.L - 0.06), C: 0.025 };
    for (let i = 0; i < 5; i += 1) {
      const k = i / 4;
      document.body.style.setProperty(`--heat-${i}`, usageOklchToHex({
        L: base.L + (top.L - base.L) * k,
        C: base.C + (top.C - base.C) * k,
        H: primary.H,
      }));
    }
  }
  /* 色键含 provider:同名模型经不同网关(或模型名缺失)也能分色;
     同一 (provider, model) 跨栏目保持同色。 */
  function usageModelColor(provider, model) {
    const key = `${provider || ""}/${model || ""}`;
    if (!usageState.modelColors.has(key)) {
      usageState.modelColors.set(key, USAGE_COLOR_VARS[usageState.modelColors.size % USAGE_COLOR_VARS.length]);
    }
    return usageState.modelColors.get(key);
  }
  function usageCacheRate(cacheRead, prompt) {
    if (!prompt) return null;
    const rate = Math.min(100, (cacheRead / prompt) * 100);
    // 两位小数;逼近满分时(>99.99)直接封顶 100——命中率是这套缓存
    // 工程的成绩单,四舍五入吃掉小数没有冲击力(验收 08-16)。
    if (rate > 99.99) return "100";
    return rate.toFixed(2);
  }

  // 控制台位置写进 URL hash:#console/<面板> 与 #console/settings/<子页>。
  // 刷新、分享链接都能回到同一页;老的裸 #console 仍开数据统计。
  function consoleHashFor(panel, view) {
    return panel === "settings" && view ? `#console/settings/${view}` : `#console/${panel}`;
  }
  function writeConsoleHash(hash) {
    const target = hash || `${window.location.pathname}${window.location.search}`;
    if ((hash && window.location.hash === hash) || (!hash && !window.location.hash)) return;
    window.history.replaceState(null, "", target); // 不用 location.hash=,那会留个孤零零的 # 并滚动
  }
  function parseConsoleHash() {
    const match = /^#console(?:\/([a-z-]+))?(?:\/([a-z-]+))?$/.exec(window.location.hash || "");
    if (!match) return null;
    const panel = match[1] || "usage";
    // 面板清单只有 index.html 一份,这里查 DOM 而不是再抄一遍。
    const known = Boolean(elements.consoleView.querySelector(`.con-panel[data-console-panel="${panel}"]`));
    const allowed = known && (isAdmin() || !isAdminOnlyPanel(panel));
    return { panel: allowed ? panel : "usage", view: match[2] || "" };
  }

  function consoleOpen(panel = "usage") {
    elements.consoleView.hidden = false;
    elements.consoleView.setAttribute("aria-hidden", "false");
    setConsolePanel(panel);
  }
  function consoleClose() {
    elements.consoleView.hidden = true;
    elements.consoleView.setAttribute("aria-hidden", "true");
    usageTipHide();
    writeConsoleHash("");
  }
  function consoleIsOpen() {
    return !elements.consoleView.hidden;
  }

  /// 切控制台标签页。数据统计的图表要等真正显示了才量得到尺寸,配置也是进了
  /// 设置页才拉——都放在这里,免得开个控制台把两边的请求都打出去。
  function setConsolePanel(panel) {
    if (!isAdmin() && isAdminOnlyPanel(panel)) panel = "usage";
    state.consolePanel = panel;
    if (panel === "account") loadAccountPanel();
    for (const item of elements.consoleView.querySelectorAll(".con-rail-item[data-console-panel]")) {
      item.classList.toggle("active", item.dataset.consolePanel === panel);
    }
    for (const pane of elements.consoleView.querySelectorAll(".con-panel[data-console-panel]")) {
      pane.hidden = pane.dataset.consolePanel !== panel;
    }
    if (panel === "usage") {
      updateChartColors();
      loadUsageStats();
      loadUsageRecords();
    } else {
      usageTipHide();
    }
    if (panel === "settings" && !state.configLoaded && !state.configLoading) loadConfigDraft();
    // 插件 dashboard 面板各自独立文件,首次进入挂载、之后只刷新。
    if (window.MiyuDash?.has(panel)) window.MiyuDash.open(panel);
    writeConsoleHash(consoleHashFor(panel, panel === "settings" ? state.settingsView : ""));
  }

  async function loadUsageStats() {
    const seq = ++usageState.loadSeq;
    elements.usageStamp.textContent = t("正在载入…");
    try {
      const response = await apiRequest(`/api/usage/stats?range=${usageState.range}`);
      const data = await response.json();
      if (seq !== usageState.loadSeq) return;
      usageState.stats = data.stats;
      renderUsage();
      const now = new Date();
      const pad = (n) => String(n).padStart(2, "0");
      elements.usageStamp.textContent =
        t("更新于 {time}", {time: `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`});
    } catch (error) {
      if (seq !== usageState.loadSeq) return;
      elements.usageStamp.textContent = t("载入失败:{detail}", {detail: error.message || error});
    }
  }

  async function loadUsageRecords() {
    try {
      const params = new URLSearchParams({ limit: "50" });
      if (elements.usageSrcFilter.value) params.set("src", elements.usageSrcFilter.value);
      if (elements.usageModelFilter.value) params.set("model", elements.usageModelFilter.value);
      const response = await apiRequest(`/api/usage/details?${params}`);
      const data = await response.json();
      renderUsageRecords(data.records || []);
    } catch (_) {
      elements.usageRecords.innerHTML =
        `<tr class="u-day-row"><td colspan="7" data-i18n="明细载入失败">明细载入失败</td></tr>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
      MiyuI18n.applyDom(elements.usageRecords);
    }
  }

  /* 筛选选项来自"至今"聚合里出现过的来源与模型;保留当前选中值。 */
  function refreshUsageFilters(stats) {
    const sources = stats.sources || [];
    const fill = (select, entries) => {
      const current = select.value;
      const keepFirst = select.options[0];
      select.replaceChildren(keepFirst);
      for (const [value, label] of entries) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = label;
        select.appendChild(option);
      }
      select.value = entries.some(([value]) => value === current) ? current : "";
    };
    fill(elements.usageSrcFilter, sources.map((source) => [source.src, usageSourceName(source.src)]));
    const models = new Map();
    for (const source of sources) {
      for (const model of source.models || []) {
        if (model.model) models.set(model.model, true);
      }
    }
    fill(elements.usageModelFilter, [...models.keys()].sort().map((model) => [model, model]));
  }

  function renderUsage() {
    const stats = usageState.stats;
    if (!stats) return;
    renderUsageTiles(stats);
    renderUsageHeat(stats.daily || []);
    renderUsageBars(stats);
    renderUsageSources(stats);
    refreshUsageFilters(stats);
  }

  function renderUsageTiles(stats) {
    const totals = stats.totals || {};
    const prev = stats.prev_totals || null;
    const delta = (current, previous) => {
      if (!prev) return "";
      const base = previous || 0;
      if (!base) return "";
      const value = ((current || 0) / base - 1) * 100;
      const dir = value >= 0 ? "up" : "down";
      const sign = value >= 0 ? "+" : "";
      return `<span class="u-tl-right u-delta ${dir}" data-i18n-title="对比上一周期" title="对比上一周期">${sign}${value.toFixed(0)}%</span>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    };
    const hit = usageCacheRate(totals.cache_read || 0, totals.prompt || 0);
    const RING_R = 15;
    const RING_C = 2 * Math.PI * RING_R;
    const icon = (path) => `<svg viewBox="0 0 24 24" aria-hidden="true">${path}</svg>`;
    const dailyAvg = usageState.range === "1d"
      ? ""
      : t(" · 日均 {value} 次", {value: (Number(totals.requests || 0) / rangeDayCount(stats)).toFixed(1)});
    const costValue = usageFmtCost(totals.cost);
    const costCoverage = Number(totals.costed_requests || 0) < Number(totals.requests || 0)
      ? t("估算覆盖 {costed}/{total} 次", {costed: Number(totals.costed_requests || 0).toLocaleString(), total: Number(totals.requests || 0).toLocaleString()})
      : t("按 models.dev 价格估算");
    const ioLine = t("输入 {prompt} · 输出 {completion}", {prompt: usageFmt(totals.prompt || 0), completion: usageFmt(totals.completion || 0)});
    const requestsLine = t("全部请求:对话 + 辅助{avg}", {avg: dailyAvg});
    const hitLine = t("命中 {hit} / 输入侧 {prompt}", {hit: usageFmt(totals.cache_read || 0), prompt: usageFmt(totals.prompt || 0)});
    // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    elements.usageTiles.innerHTML = `
      <div class="u-tile"><div class="u-tile-label" data-i18n-text="总消耗">${icon('<path d="M18 5H7l6 7-6 7h11"/>')}总消耗${delta(totals.total, prev && prev.total)}</div>
        <div class="u-tile-value">${usageFmt(totals.total || 0)}<small>tokens</small></div>
        <div class="u-tile-sub">${ioLine}</div></div>
      <div class="u-tile"><div class="u-tile-label" data-i18n-text="总消费">${icon('<path d="M12 2v20"/><path d="M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"/>')}总消费${delta(totals.cost, prev && prev.cost)}</div>
        <div class="u-tile-value">${costValue ? "≈" + costValue : "—"}</div>
        <div class="u-tile-sub">${costValue ? costCoverage : t("暂无价格数据")}</div></div>
      <div class="u-tile"><div class="u-tile-label" data-i18n-text="请求数">${icon('<path d="M22 12h-4l-3 8L9 4l-3 8H2"/>')}请求数${delta(totals.requests, prev && prev.requests)}</div>
        <div class="u-tile-value">${Number(totals.requests || 0).toLocaleString()}</div>
        <div class="u-tile-sub">${requestsLine}</div></div>
      <div class="u-tile u-tile-flex"><div class="u-tf-main">
        <div class="u-tile-label" data-i18n-text="缓存命中率">${icon('<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="3.5"/>')}缓存命中率</div>
        <div class="u-tile-value">${hit == null ? "—" : hit + "<small>%</small>"}</div>
        <div class="u-tile-sub">${hitLine}</div></div>
        <svg class="u-ring" viewBox="0 0 40 40" aria-hidden="true">
          <circle cx="20" cy="20" r="${RING_R}" fill="none" stroke="var(--chart-1)" stroke-opacity=".22" stroke-width="5"/>
          <circle cx="20" cy="20" r="${RING_R}" fill="none" stroke="var(--chart-3)" stroke-width="5" stroke-linecap="round"
            stroke-dasharray="${(((hit || 0) / 100) * RING_C).toFixed(1)} ${RING_C.toFixed(1)}" transform="rotate(-90 20 20)"/></svg></div>`;
    MiyuI18n.applyDom(elements.usageTiles);
  }

  function rangeDayCount(stats) {
    if (usageState.range === "7d") return 7;
    if (usageState.range === "30d") return 30;
    if (usageState.range === "1d") return 1;
    const daily = stats.daily || [];
    const firstActive = daily.findIndex((day) => day.requests > 0);
    return firstActive === -1 ? 1 : Math.max(1, daily.length - firstActive);
  }

  function usageParseDate(key) {
    const [year, month, day] = key.split("-").map(Number);
    return new Date(year, month - 1, day);
  }

  function renderUsageHeat(daily) {
    const wrap = elements.usageHeatmap;
    const monthsEl = elements.usageHeatMonths;
    wrap.innerHTML = "";
    monthsEl.innerHTML = "";
    if (!daily.length) return;
    const max = Math.max(1, ...daily.map((day) => day.total));
    const firstDate = usageParseDate(daily[0].date);
    const lead = (firstDate.getDay() + 6) % 7;
    for (let index = 0; index < lead; index += 1) {
      const cell = document.createElement("i");
      cell.style.visibility = "hidden";
      wrap.appendChild(cell);
    }
    for (const day of daily) {
      const cell = document.createElement("i");
      cell.dataset.l = day.total === 0 ? 0 : Math.min(4, 1 + Math.floor((day.total / max) * 3.99));
      cell.addEventListener("mousemove", (event) => usageTipShow( // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
        `<b>${day.date}</b>
         <div class="row"><span>tokens</span><em>${usageFmt(day.total)}</em></div>
         <div class="row"><span data-i18n="请求">请求</span><em>${day.requests}</em></div>${usageCostRow(day.cost)}`, event));
      cell.addEventListener("mouseleave", usageTipHide);
      wrap.appendChild(cell);
    }
    const columns = Math.ceil((lead + daily.length) / 7);
    let previousMonth = -1;
    for (let column = 0; column < columns; column += 1) {
      const index = Math.min(Math.max(0, column * 7 - lead), daily.length - 1);
      const month = usageParseDate(daily[index].date).getMonth();
      if (month !== previousMonth) {
        const label = document.createElement("span");
        label.textContent = t("{month}月", {month: month + 1});
        label.style.left = `${(column / columns) * 100}%`;
        monthsEl.appendChild(label);
        previousMonth = month;
      }
    }
    const requests = daily.reduce((sum, day) => sum + day.requests, 0);
    const tokens = daily.reduce((sum, day) => sum + day.total, 0);
    elements.usageHeatTotal.textContent =
      t("共 {count} 次调用 · {tokens} tokens", {count: requests.toLocaleString(), tokens: usageFmt(tokens)});
  }

  function renderUsageBars(stats) {
    const daily = stats.daily || [];
    let slice;
    let weekly = false;
    if (usageState.range === "1d") slice = daily.slice(-2); // 滚动 24h 跨两个日历日
    else if (usageState.range === "7d") slice = daily.slice(-7);
    else if (usageState.range === "30d") slice = daily.slice(-30);
    else {
      weekly = true;
      slice = [];
      for (let week = 0; week < Math.floor(daily.length / 7); week += 1) {
        const chunk = daily.slice(daily.length - (Math.floor(daily.length / 7) - week) * 7,
          daily.length - (Math.floor(daily.length / 7) - week - 1) * 7);
        if (!chunk.length) continue;
        const merged = { date: chunk[0].date, requests: 0, prompt: 0, completion: 0, cache_read: 0, total: 0, cost: 0 };
        for (const day of chunk) {
          merged.requests += day.requests; merged.prompt += day.prompt;
          merged.completion += day.completion; merged.cache_read += day.cache_read;
          merged.total += day.total; merged.cost += day.cost || 0;
        }
        slice.push(merged);
      }
    }
    elements.usageBarsHint.textContent = weekly ? t("按周聚合 · 悬停看明细") : t("悬停看明细");
    const bars = elements.usageBars;
    const xs = elements.usageBarsX;
    const ys = elements.usageBarsY;
    bars.innerHTML = ""; xs.innerHTML = ""; ys.innerHTML = "";
    const max = Math.max(...slice.map((day) => day.total), 0);
    if (!max) {
      bars.innerHTML = `<div class="u-empty" style="width:100%" data-i18n="该范围内没有调用记录">该范围内没有调用记录</div>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
      MiyuI18n.applyDom(bars);
      return;
    }
    const HEIGHT = 200;
    // 自适应刻度:目标 3-5 条网格线。老的固定档位在单日过亿 token 时
    // 会摆出上百条虚线和重叠标签(条纹背景 bug)。
    const rawStep = max / 4;
    const stepPow = 10 ** Math.floor(Math.log10(Math.max(1, rawStep)));
    const stepUnit = rawStep / stepPow;
    const step = (stepUnit <= 1 ? 1 : stepUnit <= 2 ? 2 : stepUnit <= 5 ? 5 : 10) * stepPow;
    const yLabel = (value, text) => {
      const label = document.createElement("span");
      label.textContent = text;
      label.style.bottom = `${(value / max) * HEIGHT}px`;
      ys.appendChild(label);
    };
    yLabel(0, "0");
    for (let value = step; value <= max; value += step) {
      const grid = document.createElement("div");
      grid.className = "u-gridline";
      grid.style.bottom = `${(value / max) * HEIGHT}px`;
      bars.appendChild(grid);
      yLabel(value, usageFmt(value));
    }
    slice.forEach((day, index) => {
      const slot = document.createElement("div");
      slot.className = "u-bar-slot";
      const column = document.createElement("div");
      column.className = "u-bar-col";
      const fresh = Math.max(0, day.prompt - day.cache_read);
      for (const [value, cls] of [[fresh, "s1"], [day.completion, "s2"], [day.cache_read, "s3"]]) {
        const segment = document.createElement("i");
        segment.className = cls;
        segment.style.height = `${Math.max(value > 0 ? 1 : 0, (value / max) * HEIGHT)}px`;
        column.appendChild(segment);
      }
      slot.appendChild(column);
      column.addEventListener("mousemove", (event) => usageTipShow( // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
        `<b>${day.date.slice(5)}${weekly ? t(" 起当周") : ""}</b>
         <div class="row"><span data-i18n-text="新输入"><i style="background:var(--chart-1)"></i>新输入</span><em>${usageFmt(fresh)}</em></div>
         <div class="row"><span data-i18n-text="输出"><i style="background:var(--chart-2)"></i>输出</span><em>${usageFmt(day.completion)}</em></div>
         <div class="row"><span data-i18n-text="缓存命中"><i style="background:var(--chart-3)"></i>缓存命中</span><em>${usageFmt(day.cache_read)}</em></div>
         <div class="row"><span data-i18n="请求">请求</span><em>${day.requests}</em></div>
         <div class="row"><span data-i18n="合计">合计</span><em>${usageFmt(day.total)}</em></div>${usageCostRow(day.cost)}`, event));
      column.addEventListener("mouseleave", usageTipHide);
      bars.appendChild(slot);
      const label = document.createElement("span");
      label.textContent = weekly
        ? (index % 4 ? "" : day.date.slice(5))
        : slice.length > 16
          ? (index % 5 ? "" : day.date.slice(5))
          : slice.length === 1 ? day.date.slice(5) : day.date.slice(8);
      xs.appendChild(label);
    });
  }

  /* ── 账号面板(阶段 5 多用户) ── */
  const accountState = { names: new Map(), loadSeq: 0 };

  function accountLabel(accountId) {
    if (!accountId) return t("未署名");
    const entry = accountState.names.get(accountId);
    if (!entry) return accountId;
    return entry.display_name && entry.display_name !== entry.username
      ? `${entry.display_name} (${entry.username})`
      : entry.username;
  }

  async function loadAccountNames() {
    const response = await apiRequest("/api/admin/accounts");
    const data = await response.json();
    accountState.names = new Map((data.accounts || []).map((account) => [account.id, account]));
    return data.accounts || [];
  }

  function showAccountError(message) {
    elements.accountError.textContent = message || "";
    elements.accountError.hidden = !message;
  }

  async function loadAccountPanel() {
    const seq = ++accountState.loadSeq;
    showAccountError("");
    const account = state.account || {};
    const noRow = !account.account_id;
    elements.accountUsername.value = account.username || (noRow ? t("(访问密码登录)") : "");
    elements.accountDisplayName.value = account.display_name || "";
    elements.accountDisplayName.disabled = noRow;
    elements.accountCurrentPassword.disabled = noRow;
    elements.accountNewPassword.disabled = noRow;
    elements.accountSave.disabled = noRow;
    elements.accountSelfHint.textContent = noRow
      ? t("用访问密码登录的是机器级管理员,密码在启动参数里改;用管理员用户名登录可以改显示名。")
      : account.admin ? t("管理员") : t("普通成员");
    elements.accountSave.disabled = false;
    elements.accountStamp.textContent = "";
    try {
      const me = await apiRequest("/api/account").then((response) => response.json());
      if (seq !== accountState.loadSeq) return;
      elements.accountProfile.value = typeof me.profile === "string" ? me.profile : "";
      accountState.profile = elements.accountProfile.value;
    } catch (_) {
      // 档案读不到就留空,保存时再报
    }
    if (!isAdmin()) {
      loadPersonaCard();
      return;
    }
    elements.inviteFresh.hidden = true;
    try {
      const [accounts, invitesResponse, usageResponse] = await Promise.all([
        loadAccountNames(),
        apiRequest("/api/admin/invites").then((response) => response.json()),
        apiRequest("/api/admin/usage/accounts?range=30d").then((response) => response.json()).catch(() => ({ accounts: [] })),
      ]);
      if (seq !== accountState.loadSeq) return;
      renderInviteRows(invitesResponse.invites || []);
      renderAccountRows(accounts, usageResponse.accounts || []);
    } catch (error) {
      if (seq !== accountState.loadSeq) return;
      elements.accountStamp.textContent = t("载入失败:{detail}", {detail: error.message || error});
    }
  }

  function renderInviteRows(invites) {
    const body = elements.inviteRows;
    body.replaceChildren();
    if (!invites.length) {
      body.innerHTML = `<tr><td colspan="5" class="acct-muted" data-i18n="还没有邀请码">还没有邀请码</td></tr>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
      MiyuI18n.applyDom(body);
      return;
    }
    const statusLabel = { open: t("可用"), used: t("已使用"), expired: t("已过期") };
    for (const invite of invites) {
      const row = document.createElement("tr");
      const usedBy = invite.used_by ? accountLabel(invite.used_by) : "—";
      row.innerHTML = `<td>${statusLabel[invite.status] || invite.status}</td><td>${formatDateTime(invite.created_at)}</td><td>${formatDateTime(invite.expires_at)}</td><td></td><td></td>`;
      row.children[3].textContent = usedBy;
      if (invite.status !== "used") {
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "secondary-button acct-row-action";
        remove.textContent = t("作废");
        remove.addEventListener("click", async () => {
          remove.disabled = true;
          try {
            await apiRequest(`/api/admin/invites/${encodeURIComponent(invite.id)}`, { method: "DELETE" });
            loadAccountPanel();
          } catch (error) {
            showToast(error.message || t("作废失败"), "error");
            remove.disabled = false;
          }
        });
        row.children[4].appendChild(remove);
      }
      body.appendChild(row);
    }
  }

  function renderAccountRows(accounts, usage) {
    const body = elements.accountRows;
    body.replaceChildren();
    const usageById = new Map(usage.map((entry) => [entry.acct, entry]));
    for (const account of accounts) {
      const row = document.createElement("tr");
      const spent = usageById.get(account.id);
      const cells = [
        account.username,
        account.display_name,
        account.admin ? t("管理员") : t("普通成员"),
        account.last_login_at ? formatRelativeTime(account.last_login_at) : t("从未"),
        spent ? usageFmt(asFiniteNumber(spent.total)) : "0",
        spent ? (usageFmtCost(asFiniteNumber(spent.cost)) || "—") : "—",
      ];
      cells.forEach((text, index) => {
        const cell = document.createElement("td");
        if (index >= 4) cell.className = "num";
        cell.textContent = text;
        row.appendChild(cell);
      });
      if (account.disabled) row.classList.add("acct-muted");
      const actions = document.createElement("td");
      const isSelf = state.account?.account_id === account.id;
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = "secondary-button acct-row-action";
      toggle.textContent = account.disabled ? t("恢复") : t("停用");
      toggle.disabled = isSelf;
      toggle.addEventListener("click", () => patchAccount(account.id, { disabled: !account.disabled }, toggle));
      const reset = document.createElement("button");
      reset.type = "button";
      reset.className = "secondary-button acct-row-action";
      reset.textContent = t("重设密码");
      reset.addEventListener("click", () => {
        const password = window.prompt(t("给 {name} 设一个新密码:", {name: account.username}));
        if (password == null) return;
        patchAccount(account.id, { password }, reset);
      });
      actions.append(toggle, reset);
      row.appendChild(actions);
      body.appendChild(row);
    }
  }

  async function patchAccount(accountId, patch, button) {
    if (button) button.disabled = true;
    try {
      await apiRequest(`/api/admin/accounts/${encodeURIComponent(accountId)}`, {
        method: "PATCH",
        body: JSON.stringify(patch)
      });
      loadAccountPanel();
    } catch (error) {
      showToast(error.message || t("操作失败"), "error");
      if (button) button.disabled = false;
    }
  }

  async function createInvite() {
    elements.inviteCreate.disabled = true;
    try {
      const response = await apiRequest("/api/admin/invites", { method: "POST", body: JSON.stringify({}) });
      const data = await response.json();
      elements.inviteFresh.textContent = data.code || "";
      elements.inviteFresh.hidden = !data.code;
      const invitesResponse = await apiRequest("/api/admin/invites").then((r) => r.json());
      renderInviteRows(invitesResponse.invites || []);
    } catch (error) {
      showToast(error.message || t("生成失败"), "error");
    } finally {
      elements.inviteCreate.disabled = false;
    }
  }

  async function saveAccount() {
    const patch = {};
    const displayName = elements.accountDisplayName.value.trim();
    if (displayName && displayName !== (state.account?.display_name || "")) patch.display_name = displayName;
    const newPassword = elements.accountNewPassword.value;
    if (newPassword) {
      patch.password = newPassword;
      patch.current_password = elements.accountCurrentPassword.value;
    }
    const profile = elements.accountProfile.value;
    if (profile !== (accountState.profile ?? "")) patch.profile = profile;
    if (!Object.keys(patch).length) return showAccountError(t("没有要保存的改动"));
    elements.accountSave.disabled = true;
    try {
      const response = await apiRequest("/api/account", { method: "PATCH", body: JSON.stringify(patch) });
      const data = await response.json();
      if (data.account && state.account) {
        state.account.display_name = data.account.display_name;
      }
      elements.accountCurrentPassword.value = "";
      elements.accountNewPassword.value = "";
      if (patch.profile != null) accountState.profile = patch.profile;
      showAccountError("");
      showToast(t("已保存"), "success");
    } catch (error) {
      showAccountError(error.message || t("保存失败"));
    } finally {
      elements.accountSave.disabled = false;
    }
  }

  /* ── 欢迎引导 / 成员人格(阶段 8) ── */
  const oobeState = { open: false, step: 1, mode: "private", editing: null, avatarFile: null, boardFile: null, plugins: [], busy: false, reason: "first" };

  function oobeShowError(message) {
    elements.oobeError.textContent = message || "";
    elements.oobeError.hidden = !message;
  }

  function oobeSetStep(step) {
    oobeState.step = step;
    for (const pane of elements.oobePanes.querySelectorAll(".oobe-pane")) {
      const active = Number(pane.dataset.oobeStep) === step;
      pane.classList.toggle("is-active", active);
      if (active) {
        pane.style.animation = "none";
        void pane.offsetWidth; // 重新触发入场动画
        pane.style.animation = "";
      }
    }
    for (const item of elements.oobeSteps.querySelectorAll("li")) {
      const n = Number(item.dataset.step);
      item.classList.toggle("on", n === step);
      item.classList.toggle("done", n < step);
    }
    const last = step === 3;
    elements.oobeBack.hidden = step === 1 || step === 4;
    elements.oobeNext.hidden = step === 4;
    // 首启是「先跳过」(跳过建号引导);新建/编辑人格是「取消」(直接关掉不保存)——
    // 之前这两种模式下这颗键整个藏了,于是新建人格没有任何退出口(用户 #164)。
    elements.oobeSkip.hidden = step === 4;
    elements.oobeSkip.textContent = oobeState.reason === "first" ? t("先跳过") : t("取消");
    elements.oobeNextLabel.textContent = last ? (oobeState.editing ? t("保存") : t("开始聊天")) : t("下一步");
    oobeShowError("");
    if (step === 1) window.requestAnimationFrame(() => elements.oobeName.focus());
    if (step === 3) window.requestAnimationFrame(() => elements.oobeProfile.focus());
  }

  function oobeSetMode(mode) {
    oobeState.mode = mode;
    for (const option of elements.oobe.querySelectorAll(".oobe-option")) {
      const on = option.dataset.personaMode === mode;
      option.classList.toggle("is-on", on);
      option.setAttribute("aria-checked", on ? "true" : "false");
    }
    elements.oobePersonaForm.hidden = mode !== "private";
  }

  function oobeRenderPlugins(options, enabled) {
    elements.oobePlugins.replaceChildren();
    const on = new Set(enabled || options.map((option) => option.id));
    for (const option of options) {
      const label = document.createElement("label");
      label.className = "oobe-plugin";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.value = option.id;
      input.checked = on.has(option.id);
      const text = document.createElement("span");
      const title = document.createElement("b");
      title.textContent = option.label || option.id;
      text.appendChild(title);
      text.append(option.hint || "");
      label.append(input, text);
      elements.oobePlugins.appendChild(label);
    }
    if (!options.length) elements.oobePlugins.innerHTML = `<p class="u-hint" data-i18n="没有可选的功能。">没有可选的功能。</p>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    if (!options.length) MiyuI18n.applyDom(elements.oobePlugins);
    bindOobeSelectAll();
  }

  // Ctrl/Cmd + A:这一页全勾 / 全不勾来回切(用户 09-14)。挂在 document 上但只在
  // 这一步可见时才接管,不然会把页面别处的「全选文字」也抢掉。只绑一次。
  let oobeSelectAllBound = false;
  function bindOobeSelectAll() {
    if (oobeSelectAllBound) return;
    oobeSelectAllBound = true;
    document.addEventListener("keydown", (event) => {
      if (event.key !== "a" && event.key !== "A") return;
      if (!(event.ctrlKey || event.metaKey) || event.altKey || event.shiftKey) return;
      const container = elements.oobePlugins;
      if (!container || !container.offsetParent) return;
      const boxes = [...container.querySelectorAll('input[type="checkbox"]')];
      if (!boxes.length) return;
      event.preventDefault();
      // 有没勾的就全勾上;已经全勾了才是全取消——半勾状态下按一下的意图是「都要」。
      const target = boxes.some((box) => !box.checked);
      for (const box of boxes) box.checked = target;
    });
  }

  /// 脚本/技能这类「逐个勾」的块:没有条目就整块藏起来;enabled 为 null = 全勾。
  function oobeRenderChecklist(wrapId, containerId, items, enabled) {
    const wrap = document.getElementById(wrapId);
    const container = document.getElementById(containerId);
    container.replaceChildren();
    wrap.hidden = !items.length;
    const on = enabled ? new Set(enabled) : null;
    for (const item of items) {
      const label = document.createElement("label");
      label.className = "oobe-plugin";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.value = item.id;
      input.checked = on ? on.has(item.id) : true;
      const text = document.createElement("span");
      const title = document.createElement("b");
      title.textContent = item.label || item.id;
      text.appendChild(title);
      text.append(item.hint || "");
      label.append(input, text);
      container.appendChild(label);
    }
  }

  /// 块藏着(没东西可勾)= null = 全部;摆出来了就按勾选发明细。
  function oobeSelectedChecklist(wrapId, containerId) {
    if (document.getElementById(wrapId).hidden) return null;
    return [...document.querySelectorAll(`#${containerId} input:checked`)].map((input) => input.value);
  }

  function oobeRenderScripts(scripts, enabled) {
    oobeRenderChecklist("oobeScriptsWrap", "oobeScripts", scripts, enabled);
  }

  function oobeRenderSkills(skills, enabled) {
    oobeRenderChecklist("oobeSkillsWrap", "oobeSkills", skills, enabled);
  }

  function oobeSelectedScripts() {
    return oobeSelectedChecklist("oobeScriptsWrap", "oobeScripts");
  }

  function oobeSelectedSkills() {
    return oobeSelectedChecklist("oobeSkillsWrap", "oobeSkills");
  }

  function oobeSelectedPlugins() {
    return [...elements.oobePlugins.querySelectorAll("input:checked")].map((input) => input.value);
  }

  function previewImageFile(file, image) {
    if (!file) return;
    const url = URL.createObjectURL(file);
    image.onload = () => URL.revokeObjectURL(url);
    image.src = url;
    image.hidden = false;
  }

  /// reason: first(注册后)/create(账号页新建)/edit(改一个已有的)
  async function openOobe({ reason = "first", persona = null } = {}) {
    if (oobeState.open || isAdmin()) return;
    oobeState.open = true;
    oobeState.reason = reason;
    oobeState.editing = persona ? persona.slug : null;
    oobeState.avatarFile = null;
    oobeState.boardFile = null;
    elements.oobe.hidden = false;
    document.body.classList.add("is-oobe");
    elements.oobeName.value = persona?.name || "";
    elements.oobeDesc.value = persona?.description || "";
    elements.oobePrompt.value = "";
    elements.oobeAvatarPreview.hidden = true;
    elements.oobeAvatarPreview.removeAttribute("src");
    elements.oobeProfile.value = "";
    oobeSetMode("private");
    elements.oobe.querySelector(".oobe-choice").hidden = reason !== "first";
    let options = [];
    let scripts = [];
    let skills = [];
    try {
      const data = await apiRequest("/api/account/personas").then((response) => response.json());
      options = data.plugins || [];
      scripts = data.scripts || [];
      skills = data.skills || [];
      if (data.shared?.name) elements.oobeSharedName.textContent = data.shared.name;
      elements.oobeSharedHint.textContent = data.shared?.maintainer
        ? t("{maintainer} 维护的预置人格,不可修改", {maintainer: data.shared.maintainer})
        : t("预置人格,不可修改");
      if (data.shared?.name) elements.oobeSharedName.textContent = data.shared.name;
      elements.oobeSharedHint.textContent = data.shared?.maintainer
        ? t("{maintainer} 维护的预置人格,不可修改", {maintainer: data.shared.maintainer})
        : t("预置人格,不可修改");
      elements.oobeProfile.value = data.prompt || "";
      if (data.member_personas === false && reason !== "first") {
        showToast(t("管理员关闭了成员自建人格"), "error");
        closeOobe();
        return;
      }
      if (data.member_personas === false) oobeSetMode("shared");
      if (persona) {
        elements.oobePrompt.value = persona.prompt || "";
        if (persona.avatar_url) { elements.oobeAvatarPreview.src = `${persona.avatar_url}&v=${Date.now()}`; elements.oobeAvatarPreview.hidden = false; }
      }
    } catch (error) {
      oobeShowError(error.message || t("载入失败"));
    }
    oobeRenderPlugins(options, persona ? persona.plugins : null);
    oobeRenderScripts(scripts, persona ? persona.scripts : null);
    oobeRenderSkills(skills, persona ? persona.skills : null);
    oobeSetStep(1);
  }

  function closeOobe() {
    oobeState.open = false;
    elements.oobe.hidden = true;
    document.body.classList.remove("is-oobe");
  }

  async function uploadPersonaImage(slug, file, board) {
    if (!file) return;
    await apiRequest(`/api/account/personas/${encodeURIComponent(slug)}/image${board ? "?board=1" : ""}`, {
      method: "PUT",
      headers: { "Content-Type": file.type || "application/octet-stream" },
      body: file,
    });
  }

  async function oobeFinish() {
    if (oobeState.busy) return;
    oobeState.busy = true;
    elements.oobeNext.disabled = true;
    elements.oobeNext.classList.add("is-loading");
    try {
      let slug = null;
      let displayName = "Miyu";
      if (oobeState.mode === "private") {
        const name = elements.oobeName.value.trim();
        const prompt = elements.oobePrompt.value.trim();
        if (!name) { oobeSetStep(1); throw new Error(t("先起个名字")); }
        const body = {
          name, prompt,
          description: elements.oobeDesc.value.trim(),
          plugins: oobeSelectedPlugins(),
          scripts: oobeSelectedScripts(),
          skills: oobeSelectedSkills(),
          activate: true,
        };
        let persona;
        if (oobeState.editing) {
          const response = await apiRequest(`/api/account/personas/${encodeURIComponent(oobeState.editing)}`, { method: "PUT", body: JSON.stringify(body) });
          persona = (await response.json()).persona;
        } else {
          const response = await apiRequest("/api/account/personas", { method: "POST", body: JSON.stringify(body) });
          persona = (await response.json()).persona;
        }
        slug = persona.slug;
        displayName = persona.name;
        await uploadPersonaImage(slug, oobeState.avatarFile, false);
      }
      const profile = elements.oobeProfile.value;
      await apiRequest("/api/account", { method: "PATCH", body: JSON.stringify({ profile }) });
      await apiRequest("/api/account/active-persona", { method: "PUT", body: JSON.stringify({ slug, oobe_done: true }) });
      accountState.profile = profile;
      await loadBootstrap();
      if (oobeState.editing) {
        // 编辑现有人格=直接保存关闭,不走 onboarding 的「已准备好」庆祝页(#146:
        // 编辑不该重新进 OOBE 的那套开场/收尾)。
        closeOobe();
        if (consoleIsOpen()) loadAccountPanel();
        showToast(t("{name} 已更新", {name: displayName}), "success");
      } else {
        elements.oobeDoneTitle.textContent = t("{name} 准备好了", {name: displayName});
        elements.oobeDoneText.textContent = oobeState.mode === "private"
          ? t("接下来的会话用这个人格。改设定、换头像在控制台的账号页。")
          : t("你用的是共享的 Miyu;想要自己的人格,随时在账号页里创建。");
        const avatar = oobeState.avatarFile ? URL.createObjectURL(oobeState.avatarFile) : (slug ? `/api/persona/avatar?scope=${encodeURIComponent(slug)}` : "/assets/miyu-logo.png");
        elements.oobeDoneAvatar.onerror = () => { elements.oobeDoneAvatar.hidden = true; };
        elements.oobeDoneAvatar.src = avatar;
        elements.oobeDoneAvatar.hidden = false;
        oobeSetStep(4);
        window.setTimeout(() => {
          closeOobe();
          if (consoleIsOpen()) loadAccountPanel();
          else if (state.sessions.length) focusComposerIfDesktop();
        }, 1400);
      }
    } catch (error) {
      oobeShowError(error.message || t("保存失败"));
    } finally {
      oobeState.busy = false;
      elements.oobeNext.disabled = false;
      elements.oobeNext.classList.remove("is-loading");
    }
  }

  function bindOobeEvents() {
    for (const option of elements.oobe.querySelectorAll(".oobe-option")) {
      option.addEventListener("click", () => oobeSetMode(option.dataset.personaMode));
    }
    elements.oobeAvatarInput.addEventListener("change", () => {
      oobeState.avatarFile = elements.oobeAvatarInput.files?.[0] || null;
      previewImageFile(oobeState.avatarFile, elements.oobeAvatarPreview);
    });
    elements.oobeBack.addEventListener("click", () => oobeSetStep(Math.max(1, oobeState.step - 1)));
    elements.oobeNext.addEventListener("click", () => {
      if (oobeState.step === 1 && oobeState.mode === "private") {
        if (!elements.oobeName.value.trim()) return oobeShowError(t("先起个名字"));
      }
      if (oobeState.step === 1 && oobeState.mode === "shared") return oobeSetStep(3);
      if (oobeState.step < 3) return oobeSetStep(oobeState.step + 1);
      oobeFinish();
    });
    elements.oobeSkip.addEventListener("click", async () => {
      // 新建/编辑人格模式:这颗是「取消」,直接关掉、什么都不动(#164)。
      if (oobeState.reason !== "first") {
        closeOobe();
        return;
      }
      try {
        await apiRequest("/api/account/active-persona", { method: "PUT", body: JSON.stringify({ slug: null, oobe_done: true }) });
      } catch (_) {}
      closeOobe();
      showToast(t("随时可以在账号页里创建自己的人格"), "info");
    });
  }

  async function loadPersonaCard() {
    if (!elements.personaList) return;
    try {
      const data = await apiRequest("/api/account/personas").then((response) => response.json());
      renderPersonaList(data);
    } catch (error) {
      const hintText = t("载入失败:{detail}", {detail: error.message || error});
      elements.personaList.innerHTML = `<p class="u-hint">${escapeText(hintText)}</p>`;
    }
  }

  function escapeText(value) {
    const span = document.createElement("span");
    span.textContent = String(value);
    return span.innerHTML;
  }

  /// 账号页人格卡的一行摘要:插件数,脚本/技能勾了明细才报数(null = 全开)。
  /// 记忆对新建的人格常开,只有旧人格关着时才提一句。
  function personaSummary(persona) {
    const parts = [t("{count} 个插件", {count: (persona.plugins || []).length})];
    if (Array.isArray(persona.scripts)) parts.push(t("{count} 个脚本", {count: persona.scripts.length}));
    if (Array.isArray(persona.skills)) parts.push(t("{count} 个技能", {count: persona.skills.length}));
    if (persona.memory === false) parts.unshift(t("记忆关"));
    return parts.join(" · ");
  }

  function renderPersonaList(data) {
    const list = elements.personaList;
    list.replaceChildren();
    elements.personaCreate.hidden = data.member_personas === false;
    const rows = [{ slug: null, name: "Miyu", description: t("管理员发布的共享人格"), shared: true }, ...(data.personas || [])];
    for (const persona of rows) {
      const row = document.createElement("div");
      row.className = "persona-row";
      const active = (data.active || null) === (persona.slug || null);
      row.classList.toggle("is-active", active);
      if (persona.avatar_url || persona.shared) {
        const image = document.createElement("img");
        image.src = persona.shared ? "/assets/miyu-logo.png" : `${persona.avatar_url}&v=${Date.now()}`;
        image.alt = "";
        row.appendChild(image);
      } else {
        const initial = document.createElement("div");
        initial.className = "persona-initial";
        initial.textContent = String(persona.name || "?").slice(0, 1);
        row.appendChild(initial);
      }
      const text = document.createElement("div");
      const title = document.createElement("b");
      title.textContent = persona.name + (active ? t("(当前)") : "");
      const sub = document.createElement("small");
      sub.textContent = persona.description || (persona.shared ? "" : personaSummary(persona));
      text.append(title, sub);
      row.appendChild(text);
      const actions = document.createElement("div");
      actions.className = "persona-row-actions";
      if (!active) {
        const use = document.createElement("button");
        use.type = "button";
        use.className = "secondary-button acct-row-action";
        use.textContent = t("使用");
        use.addEventListener("click", async () => {
          use.disabled = true;
          try {
            await apiRequest("/api/account/active-persona", { method: "PUT", body: JSON.stringify({ slug: persona.slug, oobe_done: true }) });
            await loadBootstrap();
            loadPersonaCard();
            showToast(t("新会话将使用 {name}", {name: persona.name}), "success");
          } catch (error) {
            showToast(error.message || t("切换失败"), "error");
            use.disabled = false;
          }
        });
        actions.appendChild(use);
      }
      if (!persona.shared) {
        const edit = document.createElement("button");
        edit.type = "button";
        edit.className = "secondary-button acct-row-action";
        edit.textContent = t("编辑");
        edit.addEventListener("click", async () => {
          try {
            const detail = await apiRequest("/api/account/personas").then((response) => response.json());
            const full = (detail.personas || []).find((item) => item.slug === persona.slug) || persona;
            // 提示词不在列表里:按 slug 再取一次文件内容
            const promptResponse = await apiRequest(`/api/account/personas/${encodeURIComponent(persona.slug)}/prompt`);
            full.prompt = (await promptResponse.json()).prompt || "";
            openOobe({ reason: "edit", persona: full });
          } catch (error) {
            showToast(error.message || t("载入失败"), "error");
          }
        });
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "secondary-button acct-row-action";
        remove.textContent = t("删除");
        remove.addEventListener("click", async () => {
          if (!window.confirm(t("删除人格「{name}」?记忆一起删,会话保留。", {name: persona.name}))) return;
          try {
            await apiRequest(`/api/account/personas/${encodeURIComponent(persona.slug)}`, { method: "DELETE" });
            await loadBootstrap();
            loadPersonaCard();
          } catch (error) {
            showToast(error.message || t("删除失败"), "error");
          }
        });
        actions.append(edit, remove);
      }
      row.appendChild(actions);
      list.appendChild(row);
    }
  }

  function renderUsageSources(stats) {
    const container = elements.usageSources;
    container.innerHTML = "";
    const sources = stats.sources || [];
    if (!sources.length) {
      container.innerHTML = `<div class="u-card"><div class="u-empty" data-i18n="该范围内没有调用记录">该范围内没有调用记录</div></div>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
      MiyuI18n.applyDom(container);
      return;
    }
    const agent = sources.find((source) => source.src === "agent");
    const platforms = sources.filter((source) => source.src !== "agent");
    if (isAdmin() && Array.isArray(stats.accounts) && stats.accounts.length > 1) {
      container.appendChild(buildUsageAccountsCard(stats.accounts));
    }
    if (agent) {
      container.appendChild(buildUsageSourceCard(
        t("模型消耗明细 · 智能体"),
        t("终端 / WebUI / 定时任务 / 子代理 · 悬停环形图或表行看联动"),
        agent,
        stats,
        null,
      ));
    }
    if (platforms.length) {
      if (!usageState.platformTab || !platforms.some((source) => source.src === usageState.platformTab)) {
        usageState.platformTab = platforms[0].src;
      }
      const active = platforms.find((source) => source.src === usageState.platformTab) || platforms[0];
      container.appendChild(buildUsageSourceCard(
        t("模型消耗明细 · 通讯平台"),
        t("按平台分页 · 同一模型全页同色"),
        active,
        stats,
        platforms,
      ));
    }
  }

  /// 总表按人拆(管理员):空账号 = 管理员自己 + 终端 + 通讯平台。名字来自
  /// 成员表,没拉到之前先显示 id。
  function buildUsageAccountsCard(accounts) {
    const card = document.createElement("div");
    card.className = "u-card";
    card.innerHTML = `<div class="u-card-head"><h3 data-i18n="按人拆分">按人拆分</h3><span class="u-hint" data-i18n="成员的 WebUI 会话各记各的 · 未署名 = 管理员/终端/通讯平台">成员的 WebUI 会话各记各的 · 未署名 = 管理员/终端/通讯平台</span></div>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    MiyuI18n.applyDom(card);
    const scroll = document.createElement("div");
    scroll.className = "u-table-scroll";
    const table = document.createElement("table");
    table.className = "u-table";
    table.innerHTML = `<thead><tr><th data-i18n="账号">账号</th><th class="num" data-i18n="调用">调用</th><th class="num" data-i18n="输入">输入</th><th class="num" data-i18n="输出">输出</th><th class="num" data-i18n="合计">合计</th><th class="num" data-i18n="消费">消费</th></tr></thead>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    MiyuI18n.applyDom(table);
    const body = document.createElement("tbody");
    for (const entry of accounts) {
      const row = document.createElement("tr");
      const cells = [
        accountLabel(entry.acct),
        formatInteger(entry.requests),
        usageFmt(asFiniteNumber(entry.prompt)),
        usageFmt(asFiniteNumber(entry.completion)),
        usageFmt(asFiniteNumber(entry.total)),
        usageFmtCost(asFiniteNumber(entry.cost)) || "—",
      ];
      cells.forEach((text, index) => {
        const cell = document.createElement("td");
        if (index > 0) cell.className = "num";
        cell.textContent = text;
        row.appendChild(cell);
      });
      body.appendChild(row);
    }
    table.appendChild(body);
    scroll.appendChild(table);
    card.appendChild(scroll);
    if (!accountState.names.size && isAdmin()) loadAccountNames().then(() => renderUsageSources(usageState.stats)).catch(() => {});
    return card;
  }

  function buildUsageSourceCard(title, hint, source, stats, platformTabs) {
    const card = document.createElement("div");
    card.className = "u-card";
    const head = document.createElement("div");
    head.className = "u-card-head";
    head.innerHTML = `<h3>${title}</h3><span class="u-hint">${hint}</span>`;
    if (platformTabs && platformTabs.length) {
      const seg = document.createElement("div");
      seg.className = "con-segmented";
      seg.style.marginLeft = "auto";
      for (const platform of platformTabs) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = usageSourceName(platform.src);
        button.classList.toggle("on", platform.src === source.src);
        button.addEventListener("click", () => {
          usageState.platformTab = platform.src;
          renderUsageSources(usageState.stats);
        });
        seg.appendChild(button);
      }
      head.appendChild(seg);
    }
    const filterKinds = (source.kinds || []).filter((kind) => Number(kind.total || 0) > 0);
    if (filterKinds.length) {
      const seg = document.createElement("div");
      seg.className = "con-segmented";
      seg.style.marginLeft = platformTabs && platformTabs.length ? "8px" : "auto";
      const active = usageState.kindFilters.get(source.src) || "all";
      const choices = [["all", t("全部")], ["main", t("其它")]];
      for (const kind of filterKinds) choices.push([kind.kind, usageKindName(kind.kind)]);
      for (const [value, label] of choices) {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = label;
        button.title = value === "main"
          ? t("未标注用途的调用:主线回复,以及好感度、入群审批等尚未打标的辅助调用")
          : label;
        button.classList.toggle("on", active === value);
        button.addEventListener("click", () => {
          usageState.kindFilters.set(source.src, value);
          renderUsageSources(usageState.stats);
        });
        seg.appendChild(button);
      }
      head.appendChild(seg);
    }
    card.appendChild(head);

    const body = document.createElement("div");
    body.className = "u-model-body";
    const donutWrap = document.createElement("div");
    donutWrap.className = "u-donut-wrap";
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 120 120");
    const center = document.createElement("div");
    center.className = "u-donut-center";
    donutWrap.appendChild(svg);
    donutWrap.appendChild(center);
    body.appendChild(donutWrap);

    const scroll = document.createElement("div");
    scroll.className = "u-table-scroll";
    const table = document.createElement("table");
    table.className = "u-table u-models-table";
    // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    table.innerHTML = `<thead><tr><th data-i18n="模型">模型</th><th class="num" data-i18n="占比">占比</th><th class="num" data-i18n="请求">请求</th>
      <th class="num" data-i18n="输入">输入</th><th class="num" data-i18n="输出">输出</th><th class="num" data-i18n="消费">消费</th><th data-i18n="缓存命中">缓存命中</th></tr></thead>`;
    MiyuI18n.applyDom(table);
    const tbody = document.createElement("tbody");
    const tfoot = document.createElement("tfoot");
    table.appendChild(tbody);
    table.appendChild(tfoot);
    scroll.appendChild(table);
    body.appendChild(scroll);
    card.appendChild(body);

    // 细项(主动回复判断等)摊进饼图与模型表:每个模型先扣掉细项占用的部分
    // 作为"主线",细项再各自成段——细项只当页脚摆着就看不出它吃掉了多少。
    const kinds = (source.kinds || []).filter((kind) => Number(kind.total || 0) > 0);
    const kindShare = new Map();
    for (const kind of kinds) {
      for (const model of kind.models || []) {
        const key = `${model.provider}\u0000${model.model}`;
        const prev = kindShare.get(key) || { total: 0, requests: 0, prompt: 0, completion: 0, cost: 0, cache_read: 0 };
        kindShare.set(key, {
          total: prev.total + Number(model.total || 0),
          requests: prev.requests + Number(model.requests || 0),
          prompt: prev.prompt + Number(model.prompt || 0),
          completion: prev.completion + Number(model.completion || 0),
          cache_read: prev.cache_read + Number(model.cache_read || 0),
          cost: prev.cost + Number(model.cost || 0),
        });
      }
    }
    const models = [];
    for (const model of source.models || []) {
      const taken = kindShare.get(`${model.provider}\u0000${model.model}`);
      if (!taken) {
        models.push(model);
        continue;
      }
      const rest = {
        ...model,
        total: Number(model.total || 0) - taken.total,
        requests: Number(model.requests || 0) - taken.requests,
        prompt: Number(model.prompt || 0) - taken.prompt,
        completion: Number(model.completion || 0) - taken.completion,
        cache_read: Number(model.cache_read || 0) - taken.cache_read,
        cost: Number(model.cost || 0) - taken.cost,
      };
      if (rest.total > 0 || rest.requests > 0) models.push(rest);
    }
    for (const kind of kinds) {
      for (const model of kind.models || []) {
        if (!Number(model.total || 0)) continue;
        models.push({ ...model, kindId: kind.kind, kindLabel: usageKindName(kind.kind) });
      }
    }
    models.sort((a, b) => Number(b.total || 0) - Number(a.total || 0));
    // 筛选只在有细项的卡上生效;合计随筛选重算,否则占比会拿全量当分母。
    const filter = filterKinds.length
      ? usageState.kindFilters.get(source.src) || "all"
      : "all";
    const visible = models.filter((model) => {
      if (filter === "all") return true;
      if (filter === "main") return !model.kindLabel;
      return model.kindId === filter;
    });
    const aggregate = filter === "all"
      ? source
      : visible.reduce((sum, model) => ({
          requests: sum.requests + Number(model.requests || 0),
          prompt: sum.prompt + Number(model.prompt || 0),
          completion: sum.completion + Number(model.completion || 0),
          cache_read: sum.cache_read + Number(model.cache_read || 0),
          total: sum.total + Number(model.total || 0),
          cost: sum.cost + Number(model.cost || 0),
        }), { requests: 0, prompt: 0, completion: 0, cache_read: 0, total: 0, cost: 0 });
    const requests = Number(aggregate.requests || 0);
    const requestsLine = t("{count} 次请求", {count: requests.toLocaleString()});
    // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    const defCenter = `<div><b>${usageFmt(aggregate.total || 0)}</b><small data-i18n="token 合计">token 合计</small>
      <span class="u-donut-sub">${requestsLine}</span></div>`;
    center.innerHTML = defCenter;
    MiyuI18n.applyDom(center);
    if (!visible.length || !aggregate.total) {
      tbody.innerHTML = `<tr><td colspan="7"><div class="u-empty" data-i18n="暂无记录">暂无记录</div></td></tr>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
      MiyuI18n.applyDom(tbody);
      return card;
    }

    const globalShare = stats.totals && stats.totals.total
      ? Math.round((aggregate.total / stats.totals.total) * 100)
      : null;
    const sourceHit = usageCacheRate(aggregate.cache_read || 0, aggregate.prompt || 0);
    const shareLabel = globalShare == null ? null : t("占全局 {share}%", {share: globalShare});
    // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
    tfoot.innerHTML = `<tr><td data-i18n-text="合计">合计${shareLabel == null ? "" :
      ' <small style="color:var(--text-faint);font-weight:400">' + shareLabel + "</small>"}</td>
      <td></td><td class="num">${requests}</td>
      <td class="num">${usageFmt(aggregate.prompt || 0)}</td>
      <td class="num">${usageFmt(aggregate.completion || 0)}</td>
      <td class="num">${usageFmtCost(aggregate.cost) ? "≈" + usageFmtCost(aggregate.cost) : "—"}</td>
      <td>${sourceHit == null ? "" : '<span class="u-cache-pill">' + sourceHit + "%</span>"}</td></tr>`;
    MiyuI18n.applyDom(tfoot);

    const RADIUS = 44;
    const CIRCUM = 2 * Math.PI * RADIUS;
    // 单段就是完整圆环;分段间隙只在真的有多段时存在,且不超过最小段
    // 的一半,防止小切片被间隙吃掉。
    const minShare = Math.min(...visible.map((model) => model.total / aggregate.total));
    const GAP = visible.length > 1 ? Math.min(3, Math.max(0.5, (minShare * CIRCUM) / 2)) : 0;
    let accumulated = 0;
    visible.forEach((model, index) => {
      const share = model.total / aggregate.total;
      const baseName = model.model || t("(未标模型)");
      const modelName = model.kindLabel ? `${baseName} · ${model.kindLabel}` : baseName;
      const color = usageModelColor(model.provider, model.model);
      const hit = usageCacheRate(model.cache_read || 0, model.prompt || 0);
      const row = document.createElement("tr");
      // 细项行:同模型同色(全页同色规则),用虚线点与徽章区分用途。
      const dot = model.kindLabel
        ? `<i class="u-dot u-dot-kind" style="background:${color}"></i>`
        : `<i class="u-dot" style="background:${color}"></i>`;
      row.innerHTML = `<td class="u-model-name"><b>${dot}${baseName}${
          model.kindLabel ? '<span class="u-kind-tag">' + model.kindLabel + "</span>" : ""
        }</b>
          <small><i class="u-dot" style="visibility:hidden"></i>${model.provider || "—"}</small></td>
        <td class="num">${Math.round(share * 100)}%</td>
        <td class="num">${model.requests}</td>
        <td class="num">${usageFmt(model.prompt || 0)}</td>
        <td class="num">${usageFmt(model.completion || 0)}</td>
        <td class="num">${usageFmtCost(model.cost) ? "≈" + usageFmtCost(model.cost) : "—"}</td>
        <td>${hit == null ? "—" : '<span class="u-cache-pill">' + hit + "%</span>"}</td>`;
      tbody.appendChild(row);

      const length = Math.max(0.5, share * CIRCUM - GAP);
      const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
      circle.setAttribute("cx", "60");
      circle.setAttribute("cy", "60");
      circle.setAttribute("r", String(RADIUS));
      circle.setAttribute("fill", "none");
      circle.setAttribute("stroke", color);
      circle.setAttribute("stroke-width", "18");
      if (model.kindLabel) circle.setAttribute("stroke-opacity", "0.5");
      circle.setAttribute("stroke-dasharray", `${length.toFixed(2)} ${(CIRCUM - length).toFixed(2)}`);
      circle.setAttribute("stroke-dashoffset", `${(-(accumulated * CIRCUM + GAP / 2)).toFixed(2)}`);
      circle.setAttribute("transform", "rotate(-90 60 60)");
      circle.addEventListener("mousemove", (event) => {
        circle.setAttribute("stroke-width", "21");
        row.classList.add("hl");
        center.innerHTML = `<div><b>${Math.round(share * 100)}%</b><small>${modelName}</small></div>`;
        usageTipShow( // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
          `<b>${modelName}</b>
           <div class="row"><span data-i18n="占比">占比</span><em>${Math.round(share * 100)}%</em></div>
           <div class="row"><span data-i18n="请求">请求</span><em>${model.requests}</em></div>
           <div class="row"><span data-i18n="输入">输入</span><em>${usageFmt(model.prompt || 0)}</em></div>
           <div class="row"><span data-i18n="输出">输出</span><em>${usageFmt(model.completion || 0)}</em></div>${usageCostRow(model.cost)}
           <div class="row"><span data-i18n="缓存命中">缓存命中</span><em>${hit == null ? "—" : hit + "%"}</em></div>`, event);
      });
      circle.addEventListener("mouseleave", () => {
        circle.setAttribute("stroke-width", "18");
        row.classList.remove("hl");
        center.innerHTML = defCenter;
        MiyuI18n.applyDom(center);
        usageTipHide();
      });
      row.addEventListener("mouseenter", () => circle.setAttribute("stroke-width", "21"));
      row.addEventListener("mouseleave", () => circle.setAttribute("stroke-width", "18"));
      svg.appendChild(circle);
      accumulated += share;
    });
    return card;
  }

  function renderUsageRecords(records) {
    const tbody = elements.usageRecords;
    tbody.innerHTML = "";
    if (!records.length) {
      tbody.innerHTML = `<tr class="u-day-row"><td colspan="8" data-i18n="还没有任何调用记录">还没有任何调用记录</td></tr>`; // i18n-allow: HTML 骨架走 data-i18n 标记,插入后 applyDom 统一应用
      MiyuI18n.applyDom(tbody);
      return;
    }
    const today = new Date();
    const dayKey = (date) =>
      `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
    const todayKey = dayKey(today);
    const yesterday = new Date(today);
    yesterday.setDate(today.getDate() - 1);
    const yesterdayKey = dayKey(yesterday);
    let currentDay = null;
    for (const record of records) {
      const date = new Date(record.ts * 1000);
      const key = dayKey(date);
      if (key !== currentDay) {
        currentDay = key;
        const label = key === todayKey ? t("今天") : key === yesterdayKey ? t("昨天") : "";
        const row = document.createElement("tr");
        row.className = "u-day-row";
        row.innerHTML = `<td colspan="8">${label ? label + " · " : ""}${key.slice(5)}</td>`;
        tbody.appendChild(row);
      }
      const pad = (n) => String(n).padStart(2, "0");
      const hit = usageCacheRate(record.cache_read || 0, record.prompt || 0);
      const modelLabel = record.model || t("(未标模型)");
      const kindLabel = record.kind ? usageKindShortName(record.kind) : record.aux ? t("辅助") : t("对话");
      const row = document.createElement("tr");
      row.innerHTML = `<td class="time">${pad(date.getHours())}:${pad(date.getMinutes())}</td>
        <td><span class="u-src-pill">${usageSourceName(record.src || "agent")}</span></td>
        <td class="u-model-name"><b>${modelLabel}</b><small>${record.provider || "—"}</small></td>
        <td class="num">${usageFmt(record.prompt || 0)}</td>
        <td class="num">${usageFmt(record.completion || 0)}</td>
        <td class="num">${usageFmtCost(record.cost) ? "≈" + usageFmtCost(record.cost) : "—"}</td>
        <td>${hit == null ? "—" : '<span class="u-cache-pill">' + hit + "%</span>"}</td>
        <td><span class="u-type-pill ${record.kind ? "t-kind" : record.aux ? "t-aux" : "t-chat"}">${kindLabel}</span></td>`;
      tbody.appendChild(row);
    }
  }

  function bindConsoleEvents() {
    elements.consoleButton.addEventListener("click", () => consoleOpen());
    elements.consoleBack.addEventListener("click", () => consoleClose());
    elements.conRailToggle.addEventListener("click", () =>
      elements.consoleView.classList.toggle("rail-collapsed"));
    for (const item of elements.consoleView.querySelectorAll(".con-rail-item[data-console-panel]")) {
      item.addEventListener("click", () => setConsolePanel(item.dataset.consolePanel));
    }
    elements.usageRangeSeg.addEventListener("click", (event) => {
      const button = event.target.closest("button");
      if (!button) return;
      elements.usageRangeSeg.querySelectorAll("button").forEach((other) =>
        other.classList.toggle("on", other === button));
      usageState.range = button.dataset.range;
      loadUsageStats();
    });
    elements.usageRefresh.addEventListener("click", () => {
      updateChartColors();
      loadUsageStats();
      loadUsageRecords();
    });
    elements.usageClear.addEventListener("click", async () => {
      // 本页(卡片/热力/每日/模型明细/最近调用)全部派生自 usage-history.jsonl,
      // 删它就是清空整页;usage.json 只喂聊天界面的会话累计,不在本页上。
      if (!window.confirm(t("清空数据统计？\n\n本页所有数据（总消耗、热力图、每日 token、模型明细、最近调用）都会归零，且不可恢复。"))) {
        return;
      }
      elements.usageClear.disabled = true;
      try {
        await apiRequest("/api/usage/clear", { method: "POST" });
        usageState.kindFilters.clear();
        usageState.platformTab = null;
        loadUsageStats();
        loadUsageRecords();
      } catch (error) {
        elements.usageStamp.textContent = t("清空失败:{detail}", {detail: error.message || error});
      } finally {
        elements.usageClear.disabled = false;
      }
    });
    elements.usageSrcFilter.addEventListener("change", () => loadUsageRecords());
    elements.usageModelFilter.addEventListener("change", () => loadUsageRecords());
    document.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      const fullscreenVideo = document.querySelector(".video-shell.webfs");
      if (fullscreenVideo) {
        fullscreenVideo.classList.remove("webfs");
        return;
      }
      if (consoleIsOpen()) consoleClose();
    });
  }

  function bindEvents() {
    bindConsoleEvents();
    elements.mobileMenuButton.addEventListener("click", (event) => openSidebar(event.currentTarget));
    elements.sidebarClose.addEventListener("click", closeSidebar);
    elements.sidebarScrim.addEventListener("click", closeSidebar);
    elements.sidebarCollapseButton?.addEventListener("click", () => setSidebarCollapsed(true));
    elements.sidebarExpandButton?.addEventListener("click", () => setSidebarCollapsed(false));
    elements.sidebarSettingsButton.addEventListener("click", (event) => openSettings(event.currentTarget));
    elements.artifactToggleButton.addEventListener("click", () => setArtifactWorkspaceOpen(!state.artifactOpen));
    elements.artifactCloseButton.addEventListener("click", () => setArtifactWorkspaceOpen(false));
    // 聊天正文选中文字的右键菜单(selectionmenu.js)。
    window.MiyuSelectionMenu?.mount({
      root: elements.chatScroll,
      composer: elements.composerInput,
      resizeComposer,
      apiRequest,
      renderMarkdown,
      // 浮窗里的图标与流式思考块直接用主页这两件,别在划词那边另画一套。
      makeIconSlot,
      createReasoningBlock,
      // 思考块要挂进时间线(.proc-line)才是现在的样子,不然还是旧的独立标签块。
      procLineAttach,
      procLineBreak,
      getSessionId: () => state.viewSessionId || state.currentSessionId,
      toast: showToast,
    });
    elements.artifactPreviewButton.addEventListener("click", () => setArtifactMode("preview"));
    elements.artifactSourceButton.addEventListener("click", () => setArtifactMode("source"));
    elements.artifactImageZoomOutButton.addEventListener("click", () => changeArtifactImageZoom(-0.25));
    elements.artifactImageZoomInButton.addEventListener("click", () => changeArtifactImageZoom(0.25));
    elements.artifactCopyButton.addEventListener("click", copySelectedArtifact);
    elements.artifactMaximizeButton.addEventListener("click", toggleArtifactMaximized);
    elements.artifactTitleButton.addEventListener("click", (event) => {
      event.stopPropagation();
      if (elements.artifactTitleButton.disabled) return;
      const opening = elements.artifactResourceMenu.hidden;
      elements.artifactResourceMenu.hidden = !opening;
      elements.artifactTitleButton.setAttribute("aria-expanded", String(opening));
    });
    elements.artifactResizeHandle.addEventListener("pointerdown", (event) => {
      if (layoutViewportWidth() <= 760 || state.artifactMaximized) return;
      event.preventDefault();
      elements.artifactResizeHandle.setPointerCapture(event.pointerId);
      const startX = event.clientX;
      const startWidth = elements.artifactWorkspace.offsetWidth;
      let resizeFrame = null;
      let nextRatio = state.artifactWidthRatio;
      const applyResize = () => {
        resizeFrame = null;
        state.artifactWidthRatio = nextRatio;
        syncArtifactLayout();
      };
      const move = (moveEvent) => {
        const viewportWidth = Math.max(320, layoutViewportWidth());
        const pointerDelta = visualPixelsToLayout(startX - moveEvent.clientX);
        const width = Math.min(viewportWidth - 20, Math.max(320, startWidth + pointerDelta));
        nextRatio = width / viewportWidth;
        if (!resizeFrame) resizeFrame = window.requestAnimationFrame(applyResize);
      };
      const finish = () => {
        if (resizeFrame) {
          window.cancelAnimationFrame(resizeFrame);
          applyResize();
        }
        safeStorageSet("miyu.web.artifactWidthRatio.v2", String(state.artifactWidthRatio));
        elements.artifactResizeHandle.removeEventListener("pointermove", move);
        elements.artifactResizeHandle.removeEventListener("pointerup", finish);
        elements.artifactResizeHandle.removeEventListener("pointercancel", finish);
      };
      elements.artifactResizeHandle.addEventListener("pointermove", move);
      elements.artifactResizeHandle.addEventListener("pointerup", finish);
      elements.artifactResizeHandle.addEventListener("pointercancel", finish);
    });
    elements.settingsNav.querySelectorAll("[data-settings-view]").forEach((button) => {
      button.addEventListener("click", () => setSettingsView(button.dataset.settingsView));
    });
    document.getElementById("openGroupsPanel")?.addEventListener("click", () => setConsolePanel("groups"));
    window.MiyuSettings?.init({
      state,
      configValue,
      setConfigValue,
      markConfigDirty,
      updateAdvancedConfigEditor,
      updateSettingsControls,
      refreshProviderSecretStates,
      clearProviderSecretChanges,
      apiRequest,
      showToast,
      renderModelMenu,
      setSettingsView,
      DEFAULT_BOARD_TITLE,
      DEFAULT_BOARD_SUBTITLE,
      defaultComposerPlaceholder,
      DEFAULT_STARTER_PROMPTS
    });
    elements.reloadConfigButton.addEventListener("click", loadConfigDraft);
    elements.saveConfigButton.addEventListener("click", saveConfigDraft);
    elements.applyAdvancedConfigButton.addEventListener("click", applyAdvancedConfig);
    elements.sidebarThemeButton.addEventListener("click", () => setTheme(elements.body.dataset.theme === "graphite" ? "linen" : "graphite"));
    document.querySelectorAll("[data-theme-choice]").forEach((button) => button.addEventListener("click", () => setTheme(button.dataset.themeChoice)));
    document.querySelectorAll("[data-scheme-choice]").forEach((button) => button.addEventListener("click", () => setColorScheme(button.dataset.schemeChoice)));
    document.querySelectorAll("[data-chat-font]").forEach((button) => button.addEventListener("click", () => setChatFontSize(button.dataset.chatFont)));
    // 界面语言:写进 config 草稿,由「保存配置」落盘;保存后 saveConfigDraft
    // 会发现语言变了并整页重载(2026-09-23)。
    elements.languageSeg?.querySelectorAll("[data-language]").forEach((button) => button.addEventListener("click", () => {
      if (!state.configDraft) return;
      state.configDraft.display = state.configDraft.display || {};
      state.configDraft.display.language = button.dataset.language;
      markConfigDirty();
    }));
    elements.reasoningExpandToggle?.addEventListener("click", () => setReasoningExpanded(!state.reasoningExpanded));
    elements.toolExpandToggle?.addEventListener("click", () => setToolExpanded(!state.toolExpanded));
    elements.procCollapseToggle?.addEventListener("click", () => setProcCollapse(!state.procCollapse));
    elements.modelButton.addEventListener("click", (event) => {
      event.stopPropagation();
      if (elements.modelMenu.hidden) openModelMenu();
      else closeModelMenu({ restoreFocus: true });
    });
    elements.modelMenu.addEventListener("keydown", (event) => {
      const items = Array.from(elements.modelMenu.querySelectorAll("button:not(:disabled)"));
      const index = items.indexOf(document.activeElement);
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const direction = event.key === "ArrowDown" ? 1 : -1;
        items[(index + direction + items.length) % items.length]?.focus();
      } else if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        items[event.key === "Home" ? 0 : items.length - 1]?.focus();
      } else if (event.key === "Escape") {
        event.preventDefault();
        closeModelMenu({ restoreFocus: true });
      }
    });
    document.addEventListener("pointerdown", (event) => {
    });
    document.addEventListener("click", (event) => {
      if (!elements.modelLevelMenu.hidden && !event.target.closest("#modelLevelMenu")) {
        closeLevelMenu();
      }
      if (!elements.modelMenu.hidden
        && !event.target.closest("#modelMenuWrap")
        && !event.target.closest("#modelMenu")
        && !event.target.closest("#modelLevelMenu")) {
        closeModelMenu();
      }
      if (state.sessionMenuFor && !event.target.closest(".session-menu") && !event.target.closest(".session-menu-button")) closeSessionMenu();
      if (!elements.artifactResourceMenu.hidden && !event.target.closest(".artifact-resource-wrap")) closeArtifactResourceMenu();
    });
    elements.promptGrid.querySelectorAll("[data-prompt]").forEach((button) => {
      button.addEventListener("click", () => {
        if (elements.composerInput.disabled) return;
        elements.composerInput.value = button.dataset.prompt || "";
        resizeComposer();
        elements.composerInput.focus();
      });
    });
    elements.composerInput.addEventListener("input", resizeComposer);
    // 斜杠命令的补全菜单（逻辑在 commands.js，这里只喂输入、收回填）
    elements.composerInput.addEventListener("input", () => {
      window.MiyuCommands?.onInput(elements.composerInput.value, elements.composerDock, (name) => {
        elements.composerInput.value = name;
        elements.composerInput.focus();
        resizeComposer();
      });
    });
    elements.composerInput.addEventListener("blur", () => window.MiyuCommands?.hide());
    elements.attachButton.addEventListener("click", () => elements.attachmentInput.click());
    wireMicButton();
    elements.attachmentInput.addEventListener("change", () => {
      addComposerFiles(elements.attachmentInput.files);
      elements.attachmentInput.value = "";
    });
    elements.composerForm.addEventListener("dragenter", (event) => {
      if (!event.dataTransfer?.types?.includes("Files")) return;
      event.preventDefault();
      elements.composerForm.classList.add("is-dragging");
    });
    elements.composerForm.addEventListener("dragover", (event) => {
      if (!event.dataTransfer?.types?.includes("Files")) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
      elements.composerForm.classList.add("is-dragging");
    });
    elements.composerForm.addEventListener("dragleave", (event) => {
      if (!elements.composerForm.contains(event.relatedTarget)) elements.composerForm.classList.remove("is-dragging");
    });
    elements.composerForm.addEventListener("drop", (event) => {
      elements.composerForm.classList.remove("is-dragging");
      const files = collectTransferFiles(event.dataTransfer);
      if (!files.length) return;
      event.preventDefault();
      addComposerFiles(files);
    });
    elements.composerInput.addEventListener("paste", (event) => {
      const files = collectTransferFiles(event.clipboardData);
      if (!files.length) {
        const hasUriList = Array.from(event.clipboardData?.items || []).some((item) => item.type === "text/uri-list");
        if (hasUriList) showToast(t("浏览器没有提供文件内容，请直接拖入输入框"), "error");
        return;
      }
      event.preventDefault();
      addComposerFiles(files);
    });
    elements.composerInput.addEventListener("compositionstart", () => {
      state.composing = true;
    });
    elements.composerInput.addEventListener("compositionend", () => {
      state.composing = false;
    });
    elements.composerInput.addEventListener("keydown", (event) => {
      // 菜单开着时它先吃掉上下键与 Tab/Enter：补全后再按一次回车才执行，
      // 与 REPL 一致，用户有机会反悔。
      if (window.MiyuCommands?.handleKey(event)) {
        event.preventDefault();
        return;
      }
      if (event.key === "Enter" && !event.shiftKey && !event.isComposing && !state.composing && event.keyCode !== 229) {
        // 触屏设备上回车是换行:软键盘没有 Shift+Enter,回车即发送就没法
        // 打多行了。发送用按钮;Ctrl/Cmd+Enter 仍然发送。
        if (isTouchComposer() && !(event.ctrlKey || event.metaKey)) return;
        event.preventDefault();
        if (!elements.sendButton.disabled) elements.composerForm.requestSubmit();
      }
    });
    elements.composerForm.addEventListener("submit", (event) => {
      event.preventDefault();
      submitTurn();
    });
    elements.loginForm.addEventListener("submit", (event) => {
      event.preventDefault();
      submitLogin();
    });
    elements.setupForm.addEventListener("submit", (event) => {
      event.preventDefault();
      submitSetupAdmin();
    });
    elements.registerForm.addEventListener("submit", (event) => {
      event.preventDefault();
      submitRegister();
    });
    elements.showRegisterButton.addEventListener("click", () => showRegisterForm(true));
    elements.showLoginButton.addEventListener("click", () => showRegisterForm(false));
    elements.accountSave.addEventListener("click", saveAccount);
    elements.accountLogout.addEventListener("click", logout);
    elements.inviteCreate.addEventListener("click", createInvite);
    elements.personaCreate.addEventListener("click", () => openOobe({ reason: "create" }));
    bindOobeEvents();
    elements.newChatButton.addEventListener("click", requestNewConversation);
    elements.retryBootstrapButton.addEventListener("click", loadBootstrap);
    elements.resetConfirmButton.addEventListener("click", resetConversation);
    elements.chatScroll.addEventListener("scroll", () => {
      // 程序滚动的守卫由这条事件自己解除:以前用 setTimeout(0) 清,而 scroll
      // 事件要等到下一帧才派发,处理器等于裸跑,把一次跟随当成用户上滚关掉,
      // 下一帧又认为到底重新打开——来回翻转就是抖动的第二半。
      const programmatic = state.programmaticScroll;
      // 非 smooth:这一条事件就是那次滚动的回执,吃完即解除。
      if (programmatic && !programmaticScrollSmooth) {
        state.programmaticScroll = false;
        window.clearTimeout(programmaticScrollTimer);
      }
      state.nearBottom = isNearBottom();
      if (programmatic) return;
      // 翻到顶了，更早的还在库里：往前补一页（会话项目第 2 段）。
      if (elements.chatScroll.scrollTop < 200) loadOlderTurns();
      if (!state.followOutput && isAtBottom()) {
        state.followOutput = true;
        elements.jumpBottomButton.hidden = true;
      } else if (!state.followOutput || !state.nearBottom) {
        suspendOutputFollowing();
      }
    }, { passive: true });
    elements.chatScroll.addEventListener("wheel", (event) => {
      if (event.deltaY < 0) suspendOutputFollowing();
    }, { passive: true });
    elements.chatScroll.addEventListener("touchmove", () => {
      suspendOutputFollowing();
    }, { passive: true });
    elements.jumpBottomButton.addEventListener("click", () => scrollToBottom({ force: true, smooth: true }));
    window.addEventListener("resize", () => {
      updateJumpButtonOffset();
      syncArtifactLayout();
      positionModelMenu();
    }, { passive: true });
    // 「回到底部」的 bottom 是按 composerDock 高度写的内联值。后台任务条
    // 出现/增行、软键盘顶起视口时 dock 会变高,但那些路径并不都经过
    // updateJumpButtonOffset,按钮就留在旧高度、压在任务条上——手机上一点
    // 就误触。直接盯 dock 的尺寸,谁改都跟上。
    if (typeof ResizeObserver === "function") {
      new ResizeObserver(() => updateJumpButtonOffset()).observe(elements.composerDock);
    }
    window.visualViewport?.addEventListener("resize", updateJumpButtonOffset, { passive: true });
    new ResizeObserver(syncArtifactLayout).observe(elements.mainStage);
    if (window.visualViewport) {
      window.visualViewport.addEventListener("resize", syncAppHeight, { passive: true });
      // iOS 只把可视视口平移、不改尺寸时不发 resize,只发 scroll。
      window.visualViewport.addEventListener("scroll", syncAppHeight, { passive: true });
      syncAppHeight();
    }
    document.addEventListener("keydown", handleGlobalKeydown);
  }

  function syncAppHeight() {
    const viewport = window.visualViewport;
    if (!viewport) return;
    document.documentElement.style.setProperty("--app-height", `${Math.round(viewport.height * viewport.scale / UI_SCALE)}px`);
    // 外壳缩到可视视口之后文档已经没得可滚,但 Safari 在键盘弹出的瞬间已经
    // 先滚过一次了,那段偏移要收回来,否则页面停在外壳底部的空白上。捏合放大
    // 时用户是在自己平移视口,这时不能抢方向盘。
    if (viewport.scale <= 1.01 && (window.scrollY || window.scrollX)) window.scrollTo(0, 0);
  }

  function initialize() {
    renderIconSlots();
    // 设置子页的默认值要先落定,深链再按 hash 覆盖,否则默认值会把深链盖掉。
    setSettingsView("interface");
    const deepLink = parseConsoleHash();
    if (deepLink) {
      if (deepLink.panel === "settings" && deepLink.view) setSettingsView(deepLink.view);
      consoleOpen(deepLink.panel);
    }
    setTheme(safeStorageGet("miyu.web.theme") || "graphite", false);
    const storedScheme = safeStorageGet("miyu.web.colorScheme");
    if (storedScheme) setColorScheme(storedScheme, false);
    probeMatugenTheme();
    setChatFontSize(safeStorageGet("miyu.web.chatFontSize") || "15px", false);
    setReasoningExpanded(safeStorageGet("miyu.web.reasoningExpanded") === "true", false);
    setToolExpanded(safeStorageGet("miyu.web.toolExpanded") === "true", false);
    // 没存过就是开(默认开),所以只认显式的 "false"
    setProcCollapse(safeStorageGet("miyu.web.procCollapse") !== "false", false);
    const artifactRatio = Number(safeStorageGet("miyu.web.artifactWidthRatio.v2"));
    if (Number.isFinite(artifactRatio) && artifactRatio >= 0.25 && artifactRatio <= 0.9) {
      state.artifactWidthRatio = artifactRatio;
    }
    setSidebarCollapsed(safeStorageGet("miyu.web.sidebarCollapsed") === "true");
    syncArtifactLayout();
    bindEvents();
    resizeComposer();
    updateSettingsControls();
    // 命令目录从服务端拉，前端不维护第二份清单。拉失败就当没有命令，
    // 所有 / 开头的输入照常发给模型。
    window.MiyuCommands?.load(apiRequest);
    // 灯箱自己不会画图标（图标集在这边），把工厂函数递过去。
    window.MiyuLightbox?.init({ makeIconSlot });
    window.MiyuPreview?.init({ makeIconSlot, formatFileSize });
    window.MiyuLinkCards?.init({ makeIconSlot, contentAdded });
    // 高亮和链接卡片的 settle 通道会在流停下来之后才改正文高度,那时已经没有
    // 下一条 delta 来触发滚动了,得让它们自己叫一声。
    window.MiyuHighlight?.init({ contentAdded });
    // 跨会话消息(09-23):在线登记(别的会话里的 AI 列「开着的会话」靠它,没登录不报),
    // 以及收到/发出那两种块的画法要用的几样东西。
    window.MiyuCrossSession?.init({
      makeIconSlot,
      renderMarkdown,
      formatDateTime,
      previewLines: () => state.display?.cross_session_preview_lines ?? 10
    });
    window.MiyuCrossSession?.startPresence(() => (state.blocked ? null : state.viewSessionId));
    window.MiyuSubagents?.init({ open: (sessionId) => openSessionView(sessionId) });
    window.MiyuSessionSelect?.init({
      render: renderSessionList,
      listedIds: () => state.sessions
        .map((session) => String(session?.session_id || ""))
        .filter((id) => id && !isTerminalSession(id)),
      deleteSessions
    });
    startBrailleTicker();
    // G2:页面不可见时给 body 挂 miyu-paused,CSS 据此暂停全部装饰动画。
    // 实测(Xvfb+Chrome)不挂这个时隐藏窗口的合成负载与可见时完全一样。
    const syncPaused = () => document.body.classList.toggle("miyu-paused", document.hidden);
    document.addEventListener("visibilitychange", syncPaused);
    syncPaused();
    loadBootstrap();
  }

  initialize();
})();
