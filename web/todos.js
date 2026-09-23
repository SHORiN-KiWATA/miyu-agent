"use strict";

/*
 * 任务列表面板。
 *
 * `todowrite` / `todoupdate` 的输出是一坨 JSON。REPL 那边有 `write_todo_table`
 * 专门画成表格,WebUI 一直没有对应的东西——列表折在工具签里,不展开根本看不见,
 * 而 AI 列的待办本来就是给人看的产出,不是调试信息。
 *
 * 这里把那坨 JSON 画成一张卡片,挂在工具签下方(收起态也可见)。
 * 单独成文件:app.js 已经九千多行。
 */
window.MiyuTodos = (() => {
  const STATUS_ORDER = ["in_progress", "pending", "completed", "cancelled"];
  const STATUS_LABEL = {
    pending: t("待处理"),
    in_progress: t("进行中"),
    completed: t("已完成"),
    cancelled: t("已取消"),
  };

  function isTodoTool(name) {
    const value = String(name || "").toLowerCase();
    return value === "todowrite" || value === "todoupdate";
  }

  /// 从工具输出里取出待办数组。取不到就返回 null,调用方照常走原来的路。
  function parse(output) {
    const text = String(output || "").trim();
    if (!text.startsWith("{")) return null;
    let payload;
    try {
      payload = JSON.parse(text);
    } catch (_) {
      return null;
    }
    return normalize(payload?.todos);
  }

  /// 把一串待办规整成渲染要的形状。工具输出和 `GET /api/sessions/{id}/todos`
  /// 是同一个数组形状,所以两条路共用这一份。
  function normalize(list) {
    if (!Array.isArray(list)) return null;
    const todos = list.flatMap((item) => {
      const content = String(item?.content ?? item?.task ?? "").trim();
      if (!content) return [];
      const status = String(item?.status || "pending").toLowerCase();
      return [{ content, status, priority: String(item?.priority || "").trim() }];
    });
    return todos.length ? todos : null;
  }

  function statusRank(status) {
    const index = STATUS_ORDER.indexOf(status);
    return index === -1 ? STATUS_ORDER.length : index;
  }

  function render(output) {
    return renderList(parse(output));
  }

  function renderList(todos) {
    if (!todos?.length) return null;

    const panel = document.createElement("div");
    panel.className = "todo-panel";

    const head = document.createElement("div");
    head.className = "todo-panel-head";
    const done = todos.filter((todo) => todo.status === "completed").length;
    const title = document.createElement("strong");
    title.textContent = t("任务列表");
    const count = document.createElement("small");
    count.textContent = `${done} / ${todos.length}`;
    head.append(title, count);
    panel.appendChild(head);

    const track = document.createElement("div");
    track.className = "todo-progress";
    track.setAttribute("role", "progressbar");
    track.setAttribute("aria-valuemin", "0");
    track.setAttribute("aria-valuemax", String(todos.length));
    track.setAttribute("aria-valuenow", String(done));
    const fill = document.createElement("i");
    fill.style.width = `${Math.round((done / todos.length) * 100)}%`;
    track.appendChild(fill);
    panel.appendChild(track);

    const list = document.createElement("ol");
    list.className = "todo-list";
    // 进行中的排最前——那是「现在在干什么」,列表存在的意义。
    for (const todo of [...todos].sort((a, b) => statusRank(a.status) - statusRank(b.status))) {
      const item = document.createElement("li");
      item.className = `todo-item is-${todo.status}`;
      const mark = document.createElement("span");
      mark.className = "todo-mark";
      mark.setAttribute("aria-hidden", "true");
      const text = document.createElement("span");
      text.className = "todo-text";
      text.textContent = todo.content;
      item.append(mark, text);
      // 只有「进行中」写字。「待处理」是列表的默认状态,每行写一遍是噪点;
      // 「已完成」也用不着——划掉的文字加对勾已经说清楚了。
      if (todo.status === "in_progress") {
        const status = document.createElement("small");
        status.className = "todo-status";
        status.textContent = STATUS_LABEL.in_progress;
        item.appendChild(status);
      }
      list.appendChild(item);
    }
    panel.appendChild(list);
    return panel;
  }

  return { isTodoTool, parse, normalize, render, renderList };
})();
