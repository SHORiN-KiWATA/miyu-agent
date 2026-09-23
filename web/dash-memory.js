/*
 * 记忆面板(09-04 扩完整)。
 *
 * 人格作用域 → 统计卡 → 事实 / 经历 / 归档回合三个标签 → 过滤条 → 表 → 分页。
 * 点行开抽屉:事实/经历可编辑(内容、状态、重要度、类型、真值、标签),
 * 事实带修订历史与来源经历;归档回合看全文。顶部动作:新增事实、清空待处理、
 * 清空归档、清空人格的全部记忆。数据全部来自 /api/dash/memory/*。
 */
(() => {
  const D = window.MiyuDash;
  if (!D) return;

  const state = {
    selected: new Set(),
    persona: D.recall("memory.persona"),
    active: "",
    personas: [],
    tab: "facts",
    q: "",
    filters: { facts: {}, episodes: {}, evicted: {} },
    offset: 0,
    limit: 50,
    total: 0,
    items: [],
    stats: null,
    loadSeq: 0
  };
  const ui = {};

  const STATUS = { active: t("活跃"), forgotten: t("已遗忘") };
  const STATUS_CLASS = { active: "is-active", forgotten: "is-muted" };
  const TYPE = { fact: t("事实"), preference: t("偏好"), relationship: t("关系"), task: t("任务"), self: t("自我"), other: t("其他") };
  const TRUTH = { accepted: t("已确认"), reported: t("转述"), uncertain: t("不确定"), fictional: t("虚构"), rejected: t("已否定") };
  const TRUTH_CLASS = { accepted: "is-active", uncertain: "is-warn", fictional: "is-warn", rejected: "is-danger" };
  const VISIBILITY = { public: t("公开"), principal: t("本人"), privileged: t("特权") };
  const RETENTION = { short_term: t("短期"), long_term: t("长期") };
  const ORIGIN = { local: t("本地"), platform: t("平台"), "": "—" };
  const ROLE = { user: t("用户"), assistant: t("助手") };
  const TAB_LABEL = { facts: t("事实"), episodes: t("经历"), evicted: t("归档回合") };

  const opts = (map, allLabel) => [{ value: "all", label: allLabel }, ...Object.entries(map).map(([value, label]) => ({ value, label }))];

  function stage(item) {
    if (item.promoted_at) return { label: t("已晋升"), cls: "is-active" };
    if (item.promotion_pending) return { label: t("待晋升"), cls: "is-warn" };
    if (item.consolidated_at) return { label: t("已整理"), cls: "" };
    if (item.retention === "short_term") return { label: t("未整理"), cls: "is-warn" };
    return { label: "—", cls: "" };
  }

  /* ── 挂载 ─────────────────────────────────────────────── */
  function mount(root) {
    root.textContent = "";
    ui.stamp = D.el("small", { text: "" });
    ui.persona = D.select([], state.persona, (value) => { state.persona = value; D.remember("memory.persona", value); state.offset = 0; reloadAll(); }, t("人格"));
    const head = D.el("div.con-head", null,
      D.el("h2", { text: t("记忆") }),
      D.iconButton("refresh-cw", t("刷新"), () => reloadAll()),
      ui.stamp,
      D.el("span.dash-scope", null, D.el("span.dash-scope-label", { text: t("人格") }), ui.persona));

    ui.cards = D.el("div");
    ui.actions = D.el("div.dash-actions", null,
      D.el("button.dash-button.is-primary", { type: "button", onclick: openAddFact }, D.icon("plus"), t("新增事实")),
      D.el("span.dash-actions-gap"),
      D.el("button.dash-button", { type: "button", onclick: clearPending }, D.icon("eraser"), t("清空待处理事件")),
      D.el("button.dash-button", { type: "button", onclick: clearEvicted }, D.icon("archive"), t("清空归档回合")),
      D.el("button.dash-button.is-danger", { type: "button", onclick: resetPersona }, D.icon("rotate-ccw"), t("清空此人格全部记忆")));

    ui.tabs = D.segmented(Object.entries(TAB_LABEL).map(([value, label]) => ({ value, label })), state.tab, (value) => {
      state.tab = value; state.offset = 0; state.q = ""; ui.search.value = ""; state.selected.clear(); renderFilters(); loadItems();
    });
    ui.filters = D.el("div.dash-filters");
    ui.search = D.el("input.dash-search", { type: "search", placeholder: t("搜索内容…"), oninput: () => {
      clearTimeout(ui.searchTimer);
      ui.searchTimer = setTimeout(() => { state.q = ui.search.value.trim(); state.offset = 0; loadItems(); }, 250);
    } });
    const toolbar = D.el("div.dash-toolbar", null, ui.tabs.el, ui.filters, D.el("label.dash-search-box", null, D.icon("search"), ui.search));

    ui.list = D.el("div.dash-table-wrap");
    ui.pager = D.el("div");
    ui.bulk = D.el("div");
    root.append(head, ui.cards, ui.actions, toolbar, ui.bulk, ui.list, ui.pager);
    renderFilters();
    reloadAll();
  }

  function filterSelect(key, options, allLabel) {
    const current = state.filters[state.tab][key] || "all";
    return D.select(opts(options, allLabel), current, (value) => { state.filters[state.tab][key] = value; state.offset = 0; loadItems(); });
  }

  function renderFilters() {
    ui.filters.textContent = "";
    const f = state.filters[state.tab];
    if (state.tab === "facts") {
      ui.filters.append(
        filterSelect("status", STATUS, t("全部状态")),
        filterSelect("memory_type", TYPE, t("全部类型")),
        filterSelect("truth_status", TRUTH, t("全部真值")),
        filterSelect("visibility", VISIBILITY, t("全部可见性")),
        tagInput(f));
    } else if (state.tab === "episodes") {
      ui.filters.append(
        filterSelect("status", STATUS, t("全部状态")),
        filterSelect("retention", RETENTION, t("全部保留")),
        filterSelect("stage", { unconsolidated: t("未整理"), consolidated: t("已整理"), promotion_pending: t("待晋升"), promoted: t("已晋升") }, t("全部阶段")),
        filterSelect("origin_kind", { local: t("本地"), platform: t("平台") }, t("全部来源")),
        tagInput(f));
    } else {
      const start = D.el("input.dash-select", { type: "date", title: t("起始日期"), value: f.startDate || "", onchange: () => { f.startDate = start.value; state.offset = 0; loadItems(); } });
      const end = D.el("input.dash-select", { type: "date", title: t("截止日期"), value: f.endDate || "", onchange: () => { f.endDate = end.value; state.offset = 0; loadItems(); } });
      ui.filters.append(filterSelect("role", ROLE, t("全部角色")), start, D.el("span.dash-filter-sep", { text: "→" }), end);
    }
  }

  function tagInput(f) {
    const input = D.el("input.dash-select.dash-tag-input", { type: "text", placeholder: t("标签"), value: f.tag || "", title: t("按标签过滤(精确匹配一个标签)"), oninput: () => {
      clearTimeout(ui.tagTimer);
      ui.tagTimer = setTimeout(() => { f.tag = input.value.trim(); state.offset = 0; loadItems(); }, 300);
    } });
    return input;
  }

  /* ── 数据 ─────────────────────────────────────────────── */
  async function reloadAll() {
    await loadPersonas();
    await Promise.all([loadStats(), loadItems()]);
  }

  async function loadPersonas() {
    try {
      const payload = await D.api("/api/dash/memory/personas");
      state.personas = payload.personas || [];
      state.active = payload.active || "";
      if (!state.persona || !state.personas.includes(state.persona)) state.persona = state.active;
      ui.persona.textContent = "";
      for (const name of state.personas) {
        ui.persona.append(D.el("option", { value: name, text: name === state.active ? t("{name}(当前)", { name }) : name }));
      }
      ui.persona.value = state.persona;
    } catch (error) {
      ui.stamp.textContent = t("人格列表加载失败:{message}", { message: error.message });
    }
  }

  const personaQuery = () => `persona=${encodeURIComponent(state.persona)}`;

  async function loadStats() {
    try {
      const s = await D.api(`/api/dash/memory/stats?${personaQuery()}`);
      state.stats = s;
      const coverage = s.evicted_turns ? Math.round((s.evicted_embeddings / s.evicted_turns) * 100) : 0;
      ui.cards.replaceChildren(D.statCards([
        { label: t("事实"), value: s.facts, hint: t("已遗忘 {count}", { count: s.facts_forgotten ?? 0 }) },
        { label: t("经历"), value: s.episodes, hint: t("短期 {short} · 长期 {long} · 已遗忘 {forgotten}", { short: s.short_diaries ?? 0, long: s.long_diaries ?? 0, forgotten: s.episodes_forgotten ?? 0 }) },
        { label: t("待整理"), value: s.unconsolidated_diaries, hint: t("待晋升 {pending} · 待处理事件 {events}", { pending: s.promotion_pending ?? 0, events: s.unprocessed_pending_events ?? 0 }) },
        { label: t("修订记录"), value: s.revisions, hint: t("半衰期 {days} 天 · 遗忘阈 {threshold}", { days: s.half_life_days, threshold: s.min_strength }) },
        { label: t("归档回合"), value: s.evicted_turns, hint: s.evicted_turns ? t("向量覆盖 {percent}% · {date} 起", { percent: coverage, date: D.formatTime(s.evicted_first).slice(0, 10) }) : t("尚无归档") }
      ]));
    } catch (error) {
      ui.cards.replaceChildren(D.el("p.dash-empty", { text: t("统计加载失败:{message}", { message: error.message }) }));
    }
  }

  function dayBounds(f) {
    const start = f.startDate ? new Date(`${f.startDate}T00:00:00`).toISOString() : "";
    const end = f.endDate ? new Date(`${f.endDate}T23:59:59.999`).toISOString() : "";
    return { start, end };
  }

  async function loadItems() {
    const seq = ++state.loadSeq;
    ui.stamp.textContent = t("载入中…");
    const f = state.filters[state.tab];
    let url;
    if (state.tab === "evicted") {
      const { start, end } = dayBounds(f);
      url = `/api/dash/memory/evicted?${new URLSearchParams({ persona: state.persona, q: state.q, role: f.role || "all", start, end, limit: String(state.limit), offset: String(state.offset) })}`;
    } else {
      const params = new URLSearchParams({ persona: state.persona, table: state.tab, q: state.q, limit: String(state.limit), offset: String(state.offset) });
      for (const key of ["status", "memory_type", "truth_status", "visibility", "retention", "stage", "origin_kind"]) {
        if (f[key] && f[key] !== "all") params.set(key, f[key]);
      }
      if (f.tag) params.set("tag", f.tag);
      url = `/api/dash/memory/items?${params}`;
    }
    try {
      const payload = await D.api(url);
      if (seq !== state.loadSeq) return;
      state.items = payload.items || [];
      state.total = payload.total || 0;
      // 翻页 / 换筛选后,列表里已经没有的条目不再算选中。
      const present = new Set(state.items.map((item) => `${state.tab}:${item.id}`));
      for (const key of [...state.selected]) if (!present.has(key)) state.selected.delete(key);
      renderList();
      ui.stamp.textContent = t("{persona} · {kind} {total} 条", { persona: state.persona || t("当前人格"), kind: TAB_LABEL[state.tab], total: state.total });
    } catch (error) {
      if (seq !== state.loadSeq) return;
      ui.list.replaceChildren(D.el("p.dash-empty", { text: t("加载失败:{message}", { message: error.message }) }));
      ui.stamp.textContent = "";
    }
  }

  /* ── 列表 ─────────────────────────────────────────────── */
  function renderList() {
    ui.list.textContent = "";
    if (!state.items.length) {
      ui.list.append(D.el("p.dash-empty", { text: state.q ? t("没有匹配的条目。") : t("这个人格还没有{kind}。", { kind: TAB_LABEL[state.tab] }) }));
      ui.pager.replaceChildren();
      return;
    }
    let grid;
    if (state.tab === "facts") {
      grid = D.table([
        { label: "", width: "36px" }, { label: t("内容"), width: "minmax(260px, 3fr)" }, { label: t("类型"), width: "72px" }, { label: t("真值"), width: "80px" },
        { label: t("重要"), width: "56px" }, { label: t("状态"), width: "76px" }, { label: t("置信 / 强度"), width: "108px" },
        { label: t("召回"), width: "56px" }, { label: t("更新"), width: "128px" }, { label: "", width: "40px" }]);
      for (const item of state.items) grid.append(factRow(item));
    } else if (state.tab === "episodes") {
      grid = D.table([
        { label: "", width: "36px" }, { label: t("内容"), width: "minmax(260px, 3fr)" }, { label: t("保留期"), width: "64px" }, { label: t("阶段"), width: "76px" },
        { label: t("来源"), width: "110px" }, { label: t("状态"), width: "76px" }, { label: t("强度"), width: "64px" },
        { label: t("召回"), width: "56px" }, { label: t("更新"), width: "128px" }, { label: "", width: "40px" }]);
      for (const item of state.items) grid.append(episodeRow(item));
    } else {
      grid = D.table([
        { label: "", width: "36px" }, { label: t("时间"), width: "140px" }, { label: t("角色"), width: "64px" }, { label: t("内容"), width: "minmax(260px, 4fr)" },
        { label: state.q ? t("分数") : t("向量"), width: "64px" }, { label: "", width: "40px" }]);
      for (const item of state.items) grid.append(evictedRow(item));
    }
    // 表头第一格放全选框(D.table 只认文字表头,这里事后换掉)。
    const allBox = D.el("input.dash-row-check", { type: "checkbox", title: t("全选本页"), "aria-label": t("全选本页") });
    allBox.checked = state.items.length > 0 && state.items.every((item) => state.selected.has(`${state.tab}:${item.id}`));
    allBox.addEventListener("change", () => {
      for (const item of state.items) { const key = `${state.tab}:${item.id}`; if (allBox.checked) state.selected.add(key); else state.selected.delete(key); }
      renderList();
    });
    grid.firstChild.firstChild.replaceChildren(allBox);
    ui.list.append(grid);
    renderBulk();
    const pagerNode = state.tab === "evicted" && state.q
      ? D.el("div.dash-pager", null, D.el("span.dash-pager-text", { text: t("搜索命中 {count} 条(最多 50)", { count: state.total }) }))
      : D.pager({ offset: state.offset, limit: state.limit, total: state.total, onChange: (offset) => { state.offset = offset; loadItems(); } });
    ui.pager.replaceChildren(pagerNode);
  }

  const chip = (label, cls) => D.el(`span.dash-chip${cls ? `.${cls}` : ""}`, { text: label });
  const rowAttrs = (open) => ({ role: "row", tabindex: "0", onclick: open, onkeydown: (event) => { if (event.key === "Enter") open(); } });
  const trashCell = (onDelete) => D.el("span.dash-cell-actions", null, D.iconButton("trash-2", t("删除"), (event) => { event.stopPropagation(); onDelete(); }, "is-danger"));
  const checkCell = (item) => {
    const key = `${state.tab}:${item.id}`;
    const box = D.el("input.dash-row-check", { type: "checkbox", "aria-label": t("选择") });
    box.checked = state.selected.has(key);
    box.addEventListener("click", (event) => event.stopPropagation());
    box.addEventListener("change", () => { if (box.checked) state.selected.add(key); else state.selected.delete(key); renderBulk(); });
    return D.el("span.dash-cell-check", { onclick: (event) => event.stopPropagation() }, box);
  };

  function renderBulk() {
    ui.bulk.textContent = "";
    if (!state.selected.size) return;
    ui.bulk.append(D.bulkBar({
      count: state.selected.size, noun: t("条"),
      onNone: () => { state.selected.clear(); renderList(); },
      actions: [{ label: t("删除所选{kind}", { kind: TAB_LABEL[state.tab] }), icon: "trash-2", danger: true, onClick: bulkRemove }]
    }));
  }

  async function bulkRemove() {
    const ids = [...state.selected].filter((key) => key.startsWith(`${state.tab}:`)).map((key) => key.slice(state.tab.length + 1));
    if (!ids.length) return;
    const ok = await D.confirmAction(t("删除选中的 {count} 条{kind}?此操作不可撤销。", { count: ids.length, kind: TAB_LABEL[state.tab] }));
    if (!ok) return;
    const url = (id) => state.tab === "evicted"
      ? `/api/dash/memory/evicted/${id}?${personaQuery()}`
      : `/api/dash/memory/items/${state.tab}/${id}?${personaQuery()}`;
    await D.runBatch(ids, (id) => D.api(url(id), { method: "DELETE" }), t("删除"));
    state.selected.clear();
    await Promise.all([loadStats(), loadItems()]);
  }

  function factRow(item) {
    return D.el("div.dash-row", rowAttrs(() => openDetail("facts", item.id)),
      checkCell(item),
      D.el("span.dash-cell-main", { text: item.content }),
      D.el("span.dash-cell-muted", { text: TYPE[item.memory_type] || item.memory_type || "—" }),
      D.el("span", null, chip(TRUTH[item.truth_status] || item.truth_status || "—", TRUTH_CLASS[item.truth_status])),
      D.el("span.dash-cell-mono", { text: "★".repeat(item.importance || 0) }),
      D.el("span", null, chip(STATUS[item.status] || item.status, STATUS_CLASS[item.status])),
      D.el("span.dash-cell-mono", { text: `${Number(item.confidence).toFixed(2)} / ${Number(item.strength).toFixed(2)}` }),
      D.el("span.dash-cell-mono", { text: String(item.recall_count ?? 0) }),
      D.el("span.dash-cell-muted", { text: D.formatTime(item.updated_at) }),
      trashCell(() => removeItem("facts", item)));
  }

  function episodeRow(item) {
    const st = stage(item);
    const origin = item.origin_kind === "platform"
      ? ((item.origin_platform || t("平台")) + " " + (item.origin_conversation_id || "")).trim()
      : (ORIGIN[item.origin_kind] || item.source || "—");
    return D.el("div.dash-row", rowAttrs(() => openDetail("episodes", item.id)),
      checkCell(item),
      D.el("span.dash-cell-main", { text: item.content }),
      D.el("span.dash-cell-muted", { text: RETENTION[item.retention] || "—" }),
      D.el("span", null, chip(st.label, st.cls)),
      D.el("span.dash-cell-muted", { text: origin }),
      D.el("span", null, chip(STATUS[item.status] || item.status, STATUS_CLASS[item.status])),
      D.el("span.dash-cell-mono", { text: Number(item.strength).toFixed(2) }),
      D.el("span.dash-cell-mono", { text: String(item.recall_count ?? 0) }),
      D.el("span.dash-cell-muted", { text: D.formatTime(item.updated_at) }),
      trashCell(() => removeItem("episodes", item)));
  }

  function evictedRow(item) {
    return D.el("div.dash-row", rowAttrs(() => openEvicted(item.id)),
      checkCell(item),
      D.el("span.dash-cell-muted", { text: D.formatTime(item.timestamp) }),
      D.el("span", null, chip(ROLE[item.role] || item.role, item.role === "assistant" ? "is-active" : "")),
      D.el("span.dash-cell-main", { text: item.snippet }),
      D.el("span.dash-cell-mono", { text: state.q ? Number(item.score).toFixed(1) : (item.embedded ? "✓" : "—") }),
      trashCell(() => removeEvicted(item)));
  }

  /* ── 抽屉:事实 / 经历 ───────────────────────────────── */
  async function openDetail(table, id) {
    let detail;
    try {
      detail = await D.api(`/api/dash/memory/items/${table}/${id}?${personaQuery()}`);
    } catch (error) {
      D.toast(t("加载详情失败:{message}", { message: error.message }), "error");
      return;
    }
    const item = detail.item;
    const form = {};
    form.content = D.el("textarea.dash-textarea", { rows: "5", value: item.content });
    form.content.value = item.content;
    form.status = D.select(Object.entries(STATUS).map(([value, label]) => ({ value, label })), item.status, () => {});
    form.importance = D.select([1, 2, 3, 4, 5].map((n) => ({ value: String(n), label: `${n} ${"★".repeat(n)}` })), String(item.importance || 3), () => {});
    form.tags = D.el("input.dash-select.dash-wide", { type: "text", value: (item.tags || []).join(", "), placeholder: t("逗号分隔") });
    const body = D.el("div", null,
      D.field(t("内容"), form.content),
      D.el("div.dash-field-row", null, D.field(t("状态"), form.status), D.field(t("重要度"), form.importance)));
    if (table === "facts") {
      form.memory_type = D.select(Object.entries(TYPE).map(([value, label]) => ({ value, label })), item.memory_type || "fact", () => {});
      form.truth_status = D.select(Object.entries(TRUTH).map(([value, label]) => ({ value, label })), item.truth_status || "reported", () => {});
      body.append(D.el("div.dash-field-row", null, D.field(t("类型"), form.memory_type), D.field(t("真值"), form.truth_status)));
    }
    body.append(D.field(t("标签"), form.tags));

    const meta = [
      ["ID", item.id], [t("来源"), item.source || "—"],
      [t("置信度"), Number(item.confidence).toFixed(2)], [t("强度"), t("{strength}(半衰期 {days} 天)", { strength: Number(item.strength).toFixed(2), days: state.stats?.half_life_days ?? "?" })],
      [t("召回"), t("{count} 次{recent}", { count: item.recall_count ?? 0, recent: item.last_recalled_at ? t(" · 最近 {time}", { time: D.formatTime(item.last_recalled_at) }) : "" })],
      [t("可见性"), VISIBILITY[item.visibility] || item.visibility], [t("归属"), item.owner || "—"],
      [t("主体"), Array.isArray(item.subjects) && item.subjects.length ? item.subjects.join(t("、")) : "—"],
      [t("创建"), D.formatTime(item.created_at)], [t("更新"), D.formatTime(item.updated_at)]
    ];
    if (table === "episodes") {
      const st = stage(item);
      meta.push([t("保留期"), RETENTION[item.retention] || "—"], [t("阶段"), st.label]);
      if (item.expires_at) meta.push([t("到期"), D.formatTime(item.expires_at)]);
      if (item.consolidated_at) meta.push([t("整理于"), D.formatTime(item.consolidated_at)]);
      if (item.promoted_at) meta.push([t("晋升于"), D.formatTime(item.promoted_at)]);
      if (item.origin_kind) {
        meta.push([t("来源类型"), ORIGIN[item.origin_kind] || item.origin_kind]);
        if (item.origin_kind === "platform") {
          meta.push([t("平台"), `${item.origin_platform} · ${item.origin_conversation_kind} ${item.origin_conversation_id}`]);
          if (item.origin_sender_display_name) meta.push([t("发送者"), item.origin_sender_display_name]);
        }
      }
    }
    body.append(D.el("h4.dash-section", { text: t("元数据") }), D.el("dl.dash-meta", null, meta.flatMap(([key, value]) => [D.el("dt", { text: key }), D.el("dd", { text: String(value) })])));

    if (table === "episodes" && (item.user_message || item.assistant_message)) {
      body.append(D.el("h4.dash-section", { text: t("原始对话") }),
        item.user_message ? D.el("p.dash-detail-content.is-quote", { text: t("用户:{message}", { message: item.user_message }) }) : null,
        item.assistant_message ? D.el("p.dash-detail-content.is-quote", { text: t("助手:{message}", { message: item.assistant_message }) }) : null);
    }
    if (table === "facts") {
      body.append(D.el("h4.dash-section", { text: t("修订历史({count})", { count: detail.revisions.length }) }),
        D.timeline(detail.revisions.map((rev) => ({
          time: D.formatTime(rev.created_at),
          body: D.el("div", null,
            D.el("p.dash-timeline-body.is-old", { text: rev.old_content }),
            D.el("p.dash-timeline-body", { text: rev.new_content }))
        })), t("整理器还没改过这条。")));
    }
    if (detail.source_episodes?.length) {
      body.append(D.el("h4.dash-section", { text: t("来源经历({count})", { count: detail.source_episodes.length }) }),
        D.el("ul.dash-linked", null, detail.source_episodes.map((ep) => D.el("li", { onclick: () => openDetail("episodes", ep.id) },
          D.el("span.dash-cell-muted", { text: `#${ep.id} · ${D.formatTime(ep.created_at)}` }), D.el("span", { text: ep.content })))));
    }

    const save = D.el("button.dash-button.is-primary", { type: "button", text: t("保存"), onclick: async () => {
      const patch = {
        content: form.content.value,
        status: form.status.value,
        importance: Number(form.importance.value),
        tags: form.tags.value.split(/[,,、]/).map((t) => t.trim()).filter(Boolean)
      };
      if (form.memory_type) patch.memory_type = form.memory_type.value;
      if (form.truth_status) patch.truth_status = form.truth_status.value;
      try {
        await D.api(`/api/dash/memory/items/${table}/${id}?${personaQuery()}`, { method: "PATCH", body: patch });
        D.toast(t("已保存"));
        D.closeDrawer();
        await Promise.all([loadStats(), loadItems()]);
      } catch (error) {
        D.toast(t("保存失败:{message}", { message: error.message }), "error");
      }
    } });
    const remove = D.el("button.dash-button.is-danger", { type: "button", text: t("删除"), onclick: () => removeItem(table, item) });
    D.openDrawer(`${TAB_LABEL[table]} #${item.id}`, body, [remove, save]);
  }

  async function openEvicted(id) {
    let payload;
    try {
      payload = await D.api(`/api/dash/memory/evicted/${id}?${personaQuery()}`);
    } catch (error) {
      D.toast(t("加载失败:{message}", { message: error.message }), "error");
      return;
    }
    const item = payload.item;
    const body = D.el("div", null,
      D.el("p.dash-detail-content", { text: item.content }),
      D.el("dl.dash-meta", null, [["ID", item.id], [t("时间"), D.formatTime(item.timestamp)], [t("角色"), ROLE[item.role] || item.role],
        [t("可见性"), VISIBILITY[item.visibility] || item.visibility], [t("归属"), item.owner_display_name || "—"]]
        .flatMap(([key, value]) => [D.el("dt", { text: key }), D.el("dd", { text: String(value) })])));
    const remove = D.el("button.dash-button.is-danger", { type: "button", text: t("删除这条归档"), onclick: () => removeEvicted(item) });
    D.openDrawer(t("归档回合 #{id}", { id: item.id }), body, [remove]);
  }

  /* ── 动作 ─────────────────────────────────────────────── */
  async function removeItem(table, item) {
    const ok = await D.confirmAction(t("删除这条{kind}?此操作不可撤销。\n\n{preview}", { kind: TAB_LABEL[table], preview: String(item.content || "").slice(0, 120) }));
    if (!ok) return;
    try {
      await D.api(`/api/dash/memory/items/${table}/${item.id}?${personaQuery()}`, { method: "DELETE" });
      D.closeDrawer();
      D.toast(t("已删除"));
      await Promise.all([loadStats(), loadItems()]);
    } catch (error) {
      D.toast(t("删除失败:{message}", { message: error.message }), "error");
    }
  }

  async function removeEvicted(item) {
    const ok = await D.confirmAction(t("删除这条归档回合?\n\n{preview}", { preview: String(item.snippet || item.content || "").slice(0, 120) }));
    if (!ok) return;
    try {
      await D.api(`/api/dash/memory/evicted/${item.id}?${personaQuery()}`, { method: "DELETE" });
      D.closeDrawer();
      D.toast(t("已删除"));
      await Promise.all([loadStats(), loadItems()]);
    } catch (error) {
      D.toast(t("删除失败:{message}", { message: error.message }), "error");
    }
  }

  function openAddFact() {
    const content = D.el("textarea.dash-textarea", { rows: "6", placeholder: t("写一条希望她记住的事实……") });
    const source = D.el("input.dash-select.dash-wide", { type: "text", value: "dashboard", placeholder: t("来源标记") });
    const body = D.el("div", null, D.field(t("内容"), content), D.field(t("来源"), source, t("记录这条事实是从哪来的,默认 dashboard")));
    const save = D.el("button.dash-button.is-primary", { type: "button", text: t("保存"), onclick: async () => {
      if (!content.value.trim()) { D.toast(t("内容不能为空"), "error"); return; }
      try {
        await D.api(`/api/dash/memory/facts?${personaQuery()}`, { method: "POST", body: { content: content.value, source: source.value } });
        D.toast(t("已新增"));
        D.closeDrawer();
        state.tab = "facts"; ui.tabs.set("facts"); renderFilters();
        await Promise.all([loadStats(), loadItems()]);
      } catch (error) {
        D.toast(t("新增失败:{message}", { message: error.message }), "error");
      }
    } });
    D.openDrawer(t("新增事实"), body, [save]);
    content.focus();
  }

  async function clearPending() {
    const ok = await D.confirmAction(t("清空 {persona} 的待处理事件({count} 条)?这些事件还没被整理成经历。", { persona: state.persona, count: state.stats?.unprocessed_pending_events ?? "?" }), t("清空"));
    if (!ok) return;
    try {
      await D.api(`/api/dash/memory/pending/clear?${personaQuery()}`, { method: "POST" });
      D.toast(t("已清空待处理事件"));
      loadStats();
    } catch (error) {
      D.toast(t("失败:{message}", { message: error.message }), "error");
    }
  }

  async function clearEvicted() {
    const ok = await D.confirmAction(t("清空 {persona} 的归档回合({count} 条)?清掉后 search_evicted_context 找不回它们。", { persona: state.persona, count: state.stats?.evicted_turns ?? "?" }), t("清空"));
    if (!ok) return;
    try {
      await D.api(`/api/dash/memory/evicted/clear?${personaQuery()}`, { method: "POST" });
      D.toast(t("已清空归档回合"));
      await Promise.all([loadStats(), state.tab === "evicted" ? loadItems() : null]);
    } catch (error) {
      D.toast(t("失败:{message}", { message: error.message }), "error");
    }
  }

  async function resetPersona() {
    const scope = state.persona;
    const typed = window.prompt(t("这会删除人格「{persona}」的全部事实、经历、待处理事件、修订记录与归档回合(技能不动),不分会话、不可恢复。\n\n输入人格名 {persona} 确认:", { persona: scope }));
    if (typed === null) return;
    if (typed.trim() !== scope) { D.toast(t("人格名不匹配,已取消"), "error"); return; }
    try {
      await D.api(`/api/dash/memory/reset?${personaQuery()}`, { method: "POST", body: { confirm: scope } });
      D.toast(t("已清空全部记忆"));
      await Promise.all([loadStats(), loadItems()]);
    } catch (error) {
      D.toast(t("清空失败:{message}", { message: error.message }), "error");
    }
  }

  D.register({ name: "memory", root: "dashMemoryRoot", mount, refresh: () => reloadAll() });
})();
