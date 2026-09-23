// WebUI 双语运行时的最小单测(node + vm,零依赖)。
//
//   node testkit/webui-i18n/runtime_check.js
//
// 钉三件事:英文界面查词典、`{占位符}` 插值、缺词条回退中文原文(宁可混一句
// 中文,不能空文案)。走查(testkit/webui-i18n/run.py)盖的是真浏览器里的整页
// 表现,这里盖的是运行时本身,跑得快、CI 也能跑。
const fs = require("fs");
const path = require("path");
const vm = require("vm");

const ROOT = path.resolve(__dirname, "../..");

function load(lang) {
  const sandbox = {
    console,
    window: { MIYU_LANG: lang },
    document: {
      readyState: "complete",
      documentElement: {},
      querySelectorAll: () => [],
      addEventListener: () => {},
    },
    Node: { TEXT_NODE: 3 },
  };
  sandbox.window.document = sandbox.document;
  vm.createContext(sandbox);
  vm.runInContext(fs.readFileSync(path.join(ROOT, "web/i18n-en.js"), "utf8"), sandbox);
  vm.runInContext(fs.readFileSync(path.join(ROOT, "web/i18n.js"), "utf8"), sandbox);
  return { api: sandbox.window.MiyuI18n, win: sandbox.window };
}

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "✅" : "❌"} ${name}${detail ? "  " + detail : ""}`);
  if (!ok) failures.push(name);
}

const en = load("en");
const zh = load("zh");

check("en_lang", en.api.lang === "en" && en.api.intlLocale === "en-US", `${en.api.lang}/${en.api.intlLocale}`);
check("zh_lang", zh.api.lang === "zh" && zh.api.intlLocale === "zh-CN", `${zh.api.lang}/${zh.api.intlLocale}`);
check("en_lookup", en.api.t("新对话") === "New chat", en.api.t("新对话"));
check("zh_identity", zh.api.t("新对话") === "新对话");
check("placeholder", en.api.t("已选 {count} {noun}", { count: 3, noun: "items" }) === "3 items selected");
check("placeholder_zh", zh.api.t("已选 {count} {noun}", { count: 3, noun: "项" }) === "已选 3 项");
check("missing_falls_back", en.api.t("这条没有词条") === "这条没有词条");
check("missing_keeps_placeholder", en.api.t("没有 {x} 词条", { x: 1 }) === "没有 1 词条");
check("global_t_alias", typeof en.win.t === "function" && en.win.t("新对话") === "New chat");
check("dict_is_frozen", Object.isFrozen(en.win.MIYU_I18N_EN));

const passed = 10 - failures.length;
console.log(`\n${passed}/10 passed`);
process.exit(failures.length ? 1 : 0);
