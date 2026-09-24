"use strict";

/*
 * 侧栏会话批量删除(用户 09-24)。
 *
 * 从某一条会话的「…」菜单里点「多选…」进入选择模式,这一条先勾上。选择模式下每行
 * 开头的指示格换成勾选框,点一行就是勾上或取消;列表顶上一条操作栏:已选几个、全选、
 * 删除、取消。Esc 也能退出。
 *
 * 真正的删除在 app.js(`deleteSessions`):逐个走单删接口、一个等一个——服务端删会话
 * 要占一把全局的管理锁,并发发出去除了第一个都是 409。这里只管「选了哪些」和那条
 * 操作栏。单独成文件:app.js 已经一万三千多行。
 */
window.MiyuSessionSelect = (() => {
  let selected = null; // Set<string>;null = 不在选择模式
  let ctx = null;

  /// ctx:{ render(), listedIds(): string[], deleteSessions(ids): Promise }
  function init(context) {
    ctx = context;
    document.addEventListener("keydown", (event) => {
      if (event.key !== "Escape" || !selected || event.defaultPrevented) return;
      const target = event.target;
      if (target?.closest?.("input, textarea, [contenteditable='true']")) return;
      exit();
    });
  }

  function active() {
    return selected !== null;
  }

  function has(id) {
    return Boolean(selected?.has(String(id)));
  }

  function enter(id) {
    selected = new Set(id ? [String(id)] : []);
    ctx?.render();
  }

  function exit() {
    if (!selected) return;
    selected = null;
    ctx?.render();
  }

  function toggle(id) {
    if (!selected) return;
    const key = String(id);
    if (selected.has(key)) selected.delete(key);
    else selected.add(key);
    ctx?.render();
  }

  /// 选择模式下的一行:指示格换成勾选框,整行点击改成勾选(点击本身在 app.js 里分流)。
  function decorateItem(item, main, lead, id) {
    const checked = has(id);
    item.classList.add("is-selecting");
    item.classList.toggle("is-selected", checked);
    main.setAttribute("aria-pressed", String(checked));
    const box = document.createElement("span");
    box.className = "session-select-box";
    box.setAttribute("aria-hidden", "true");
    box.textContent = checked ? "✓" : "";
    lead.replaceChildren(box);
  }

  function button(label, onClick, extraClass = "") {
    const element = document.createElement("button");
    element.type = "button";
    element.className = `session-bulk-button${extraClass ? ` ${extraClass}` : ""}`;
    element.textContent = label;
    element.addEventListener("click", (event) => {
      event.stopPropagation();
      onClick();
    });
    return element;
  }

  /// 列表顶上那条操作栏。
  function buildBar() {
    const ids = ctx?.listedIds() || [];
    const chosen = ids.filter((id) => selected?.has(id));
    const bar = document.createElement("div");
    bar.className = "session-bulk-bar";
    bar.setAttribute("role", "toolbar");
    bar.setAttribute("aria-label", t("批量操作"));
    const count = document.createElement("span");
    count.className = "session-bulk-count";
    count.textContent = t("已选 {count} 个", { count: chosen.length });
    const everything = chosen.length === ids.length && ids.length > 0;
    const all = button(everything ? t("全不选") : t("全选"), () => {
      selected = new Set(everything ? [] : ids);
      ctx?.render();
    });
    const remove = button(t("删除"), () => deleteSelected(), "is-danger");
    remove.disabled = chosen.length === 0;
    const cancel = button(t("取消"), exit);
    bar.append(count, all, remove, cancel);
    return bar;
  }

  async function deleteSelected() {
    const ids = (ctx?.listedIds() || []).filter((id) => selected?.has(id));
    if (!ids.length) return;
    if (!window.confirm(t("删除选中的 {count} 个会话？此操作无法撤销。", { count: ids.length }))) return;
    selected = null;
    await ctx?.deleteSessions(ids);
    ctx?.render();
  }

  return { init, active, has, enter, exit, toggle, decorateItem, buildBar };
})();
