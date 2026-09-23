/*
 * 表情包面板(09-04)。
 *
 * 库作用域(默认当前人格映射的库)→ 统计卡 → 过滤条(状态 / 动图 / 来源 / 标签 /
 * 搜索,搜索用后端同款打分在前端复算)→ 画廊 → 抽屉(大图、编辑、来源、平台引用、
 * 启停、重分类、删除)。上传抽屉支持 AI 分类与手填两种模式,逐张排队。
 * 数据来自 /api/dash/memes/*。
 */
(() => {
  const D = window.MiyuDash;
  if (!D) return;

  const state = {
    library: D.recall("memes.library"),
    libraries: null,
    listing: null,
    refs: new Map(),
    filter: { state: "all", animated: "all", origin: "all", tag: "" },
    q: "",
    loadSeq: 0,
    selecting: false,
    selected: new Set(),
    galleryObserver: null
  };
  const ui = {};

  const STATE_LABEL = { builtin: t("内置"), user: t("自有"), shadowed: t("已覆盖"), disabled: t("已禁用") };

  function itemState(item) {
    if (item.disabled) return "disabled";
    if (item.shadowed) return "shadowed";
    return item.source;
  }
  const stateChip = (item) => {
    const s = itemState(item);
    const cls = { builtin: "is-builtin", user: "is-active", shadowed: "is-warn", disabled: "is-muted" }[s];
    return D.el(`span.dash-chip.${cls}`, { text: STATE_LABEL[s] });
  };
  const imageUrl = (item) => `/api/dash/memes/image?${new URLSearchParams({ library: state.library, id: item.id })}`;
  const libQuery = () => `library=${encodeURIComponent(state.library)}`;

  /* 与后端 score_meme 同一套打分,前端复算给"模型会看到的前三"提示。 */
  function normalize(value) {
    return value.toLowerCase().replace(/[!-\/:-@\[-`{-~，。！？、；：（）“”]/g, " ");
  }
  function terms(query) {
    const out = new Set();
    for (const token of query.split(/\s+/).filter(Boolean)) {
      if ([...token].length > 1) out.add(token);
      if (/[^\x00-\x7f]/.test(token)) {
        const chars = [...token];
        for (let i = 0; i + 1 < chars.length; i += 1) out.add(chars[i] + chars[i + 1]);
      }
    }
    return [...out];
  }
  function score(item, query) {
    const q = normalize(query);
    const ts = terms(q);
    if (!ts.length) return 0.1;
    const name = normalize(`${item.name.zh} ${item.name.en}`);
    const desc = normalize(item.description);
    const usage = normalize(item.usage);
    const tags = normalize(item.tags.join(" "));
    let s = 0;
    for (const t of ts) {
      if (tags.includes(t)) s += 3;
      if (name.includes(t)) s += 2.5;
      if (usage.includes(t)) s += 2;
      if (desc.includes(t)) s += 1.2;
    }
    if (q && `${name} ${desc} ${usage} ${tags}`.includes(q)) s += 2;
    return s;
  }

  /* ── 挂载 ─────────────────────────────────────────────── */
  function mount(root) {
    root.textContent = "";
    ui.stamp = D.el("small", { text: "" });
    ui.library = D.select([], state.library, (value) => { state.library = value; D.remember("memes.library", value); loadItems(); }, t("表情库"));
    ui.mapping = D.el("small.dash-scope-hint", { text: "" });
    const head = D.el("div.con-head", null,
      D.el("h2", { text: t("表情包") }),
      D.iconButton("refresh-cw", t("刷新"), () => reloadAll()),
      ui.stamp,
      D.el("span.dash-scope", null, D.el("span.dash-scope-label", { text: t("库") }), ui.library, ui.mapping,
        D.el("button.dash-button.is-primary", { type: "button", onclick: openUpload }, D.icon("plus"), t("上传"))));

    ui.cards = D.el("div");
    ui.tags = D.el("div.dash-tag-cloud");
    ui.search = D.el("input.dash-search", { type: "search", placeholder: t("按名称、描述、用法、标签搜索…"), oninput: () => {
      clearTimeout(ui.searchTimer);
      ui.searchTimer = setTimeout(() => { state.q = ui.search.value.trim(); renderGallery(); }, 200);
    } });
    const filterSelect = (key, options) => D.select(options, state.filter[key], (value) => { state.filter[key] = value; renderGallery(); });
    const toolbar = D.el("div.dash-toolbar", null,
      filterSelect("state", [{ value: "all", label: t("全部状态") }, { value: "builtin", label: t("内置") }, { value: "user", label: t("自有") }, { value: "shadowed", label: t("已覆盖") }, { value: "disabled", label: t("已禁用") }]),
      filterSelect("animated", [{ value: "all", label: t("静图 + 动图") }, { value: "yes", label: t("仅动图") }, { value: "no", label: t("仅静图") }]),
      filterSelect("origin", [{ value: "all", label: t("全部来源") }, { value: "collected", label: t("QQ 收集") }, { value: "manual", label: t("手工添加") }]),
      D.el("label.dash-search-box", null, D.icon("search"), ui.search),
      D.el("button.dash-button", { type: "button", title: t("进入选择模式,批量禁用 / 启用 / 删除"), onclick: () => setSelecting(!state.selecting) }, D.icon("check-square"), t("选择")));
    ui.selectButton = toolbar.lastChild;
    ui.hint = D.el("p.dash-search-hint", { hidden: true });
    ui.bulk = D.el("div");
    ui.gallery = D.el("div.dash-gallery.is-masonry");
    root.append(head, ui.cards, toolbar, ui.tags, ui.hint, ui.bulk, ui.gallery);
    watchGalleryWidth();
    reloadAll();
  }

  async function reloadAll() {
    await loadLibraries();
    await loadItems();
  }

  async function loadLibraries() {
    try {
      const payload = await D.api("/api/dash/memes/libraries");
      state.libraries = payload;
      const names = payload.libraries.map((entry) => entry.name);
      if (!state.library || !names.includes(state.library)) state.library = payload.active;
      ui.library.textContent = "";
      for (const entry of payload.libraries) {
        const marks = [entry.builtin ? t("内置") : null, entry.user ? t("自有") : null].filter(Boolean).join("+");
        ui.library.append(D.el("option", { value: entry.name, text: t("{name}{active}{marks}", { name: entry.name, active: entry.name === payload.active ? t("(当前人格)") : "", marks: marks ? ` · ${marks}` : "" }) }));
      }
      ui.library.value = state.library;
      const persona = payload.active_persona || t("默认人格");
      ui.mapping.textContent = t("人格 {persona} → {library}", { persona, library: payload.active });
    } catch (error) {
      ui.stamp.textContent = t("库清单加载失败:{error}", { error: error.message });
    }
  }

  async function loadItems() {
    const seq = ++state.loadSeq;
    ui.stamp.textContent = t("载入中…");
    try {
      const listing = await D.api(`/api/dash/memes/items?${libQuery()}`);
      if (seq !== state.loadSeq) return;
      state.listing = listing;
      state.refs = new Map((listing.refs || []).map((r) => [r.meme_id, r]));
      renderCards();
      renderTags();
      renderGallery();
      ui.stamp.textContent = t("{library} · {count} 张", { library: listing.library, count: listing.stats.total });
    } catch (error) {
      if (seq !== state.loadSeq) return;
      ui.gallery.replaceChildren(D.el("p.dash-empty", { text: t("加载失败:{error}", { error: error.message }) }));
      ui.stamp.textContent = "";
    }
  }

  function renderCards() {
    const s = state.listing.stats;
    const mtime = state.listing.index_mtime ? D.formatTime(state.listing.index_mtime * 1000) : "—";
    ui.cards.replaceChildren(D.statCards([
      { label: t("总数"), value: s.total, hint: t("内置 {builtin} · 自有 {own}", { builtin: s.builtin, own: s.user }) },
      { label: t("已覆盖"), value: s.shadowed, hint: t("自有条目盖住同图内置项") },
      { label: t("已禁用"), value: s.disabled, hint: t("模型看不到,面板仍列出") },
      { label: t("QQ 收集"), value: s.collected, hint: t("近 7 天 {count}", { count: s.collected_7d }) },
      { label: t("索引更新"), value: mtime.slice(5), hint: state.listing.user_dir }
    ]));
  }

  function renderTags() {
    const counts = new Map();
    for (const item of state.listing.items) for (const tag of item.tags) counts.set(tag, (counts.get(tag) || 0) + 1);
    const top = [...counts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 24);
    ui.tags.textContent = "";
    if (!top.length) return;
    for (const [tag, count] of top) {
      ui.tags.append(D.el(`button.dash-chip.is-clickable${state.filter.tag === tag ? ".is-active" : ""}`, { type: "button", text: `${tag} ${count}`, onclick: () => { state.filter.tag = state.filter.tag === tag ? "" : tag; renderTags(); renderGallery(); } }));
    }
  }

  function visibleItems() {
    const f = state.filter;
    let items = state.listing.items.filter((item) => {
      if (f.state !== "all" && itemState(item) !== f.state) return false;
      if (f.animated === "yes" && !item.animated) return false;
      if (f.animated === "no" && item.animated) return false;
      if (f.origin === "collected" && !item.origin) return false;
      if (f.origin === "manual" && item.origin) return false;
      if (f.tag && !item.tags.includes(f.tag)) return false;
      return true;
    });
    if (state.q) {
      items = items.map((item) => ({ item, s: score(item, state.q) })).filter((e) => e.s > 0).sort((a, b) => b.s - a.s).map((e) => e.item);
    }
    return items;
  }

  function renderGallery() {
    const items = visibleItems();
    ui.gallery.textContent = "";
    if (state.q) {
      const enabled = items.filter((item) => !item.disabled).slice(0, 3).map((item) => item.name.zh);
      ui.hint.hidden = false;
      ui.hint.textContent = enabled.length ? t("模型搜索“{query}”会拿到前 3 张:{names}", { query: state.q, names: enabled.join(" · ") }) : t("模型搜索“{query}”拿不到任何表情", { query: state.q });
    } else {
      ui.hint.hidden = true;
    }
    if (!items.length) {
      ui.gallery.append(D.el("p.dash-empty", { text: state.listing.items.length ? t("没有匹配的表情。") : t("这个库还是空的,上传几张吧。") }));
      return;
    }
    renderBulk(items);
    for (const item of items) {
      const refs = state.refs.get(item.id);
      const picked = state.selected.has(item.id);
      const activate = () => { if (state.selecting) toggleSelected(item, card); else openDetail(item); };
      const thumbImage = D.el("img", { src: imageUrl(item), alt: item.name.zh, loading: "lazy", decoding: "async" });
      const card = D.el("figure.dash-meme", { tabindex: "0", onclick: activate, onkeydown: (event) => { if (event.key === "Enter" || (state.selecting && event.key === " ")) { event.preventDefault(); activate(); } } },
        D.el("div.dash-meme-thumb", null,
          state.selecting ? D.el("span.dash-meme-check", { "aria-hidden": "true" }, D.icon("check")) : null,
          thumbImage,
          item.animated ? D.el("span.dash-meme-badge", { text: "GIF" }) : null,
          refs?.outbound ? D.el("span.dash-meme-badge.is-count", { text: `↑${refs.outbound}` }) : null),
        D.el("figcaption.dash-meme-cap", null, D.el("span.dash-meme-name", { text: item.name.zh }), stateChip(item)));
      card.classList.toggle("is-disabled", item.disabled);
      card.classList.toggle("is-selectable", state.selecting);
      card.classList.toggle("is-selected", picked);
      ui.gallery.append(card);
      fitThumb(card, thumbImage);
    }
    relayout();
  }

  /* ── 瀑布流 ──────────────────────────────────────────
     表情包的长宽比什么都有。整齐的方格网格意味着两件事同时发生:高的图被塞
     进方框里、四周留白,而它又把整行的行高顶起来——同一行的方图只填得满三
     分之一。这里改成每张按自己的比例占位,行高互不牵连。

     做法是「1px 行高 + 按实际高度算跨行数」这套瀑布流:网格列还是自动填充,
     所以从左到右的顺序保持不变(搜索命中的前三张仍然在最前面,CSS 多列布局
     做不到这点)。 */

  /** 极端比例要收一收:1:5 的长条会把一整列拉成走廊。
      0.55 ≈ 9:16,常见的竖图正好不被裁,再瘦才开始留边。 */
  const THUMB_MIN_RATIO = 0.55;
  const THUMB_MAX_RATIO = 1.9;

  function applyRatio(card, image) {
    const thumb = card.firstElementChild;
    if (!thumb || !image.naturalWidth || !image.naturalHeight) return;
    const ratio = Math.min(Math.max(image.naturalWidth / image.naturalHeight, THUMB_MIN_RATIO), THUMB_MAX_RATIO);
    thumb.style.aspectRatio = String(ratio);
    layoutCard(card);
  }

  function fitThumb(card, image) {
    if (image.complete) applyRatio(card, image);
    else image.addEventListener("load", () => applyRatio(card, image), { once: true });
    // 加载失败就按方图占位,别留一个 1px 高的空档。
    image.addEventListener("error", () => layoutCard(card), { once: true });
  }

  function layoutCard(card) {
    if (!card.isConnected) return;
    const height = Math.ceil(card.getBoundingClientRect().height);
    if (height <= 0) return;
    // 行单位是 1px,所以跨行数就是像素高;竖直间距靠卡片自己的下外边距,
    // 读实际计算值而不是写死——窄屏那套间距不一样。
    const gap = Math.ceil(parseFloat(getComputedStyle(card).marginBottom) || 0);
    card.style.gridRowEnd = `span ${height + gap}`;
  }

  function relayout() {
    for (const card of ui.gallery.querySelectorAll(".dash-meme")) layoutCard(card);
  }

  /** 面板宽度一变(侧栏折叠、窗口缩放),列宽跟着变,所有卡片都要重算。 */
  function watchGalleryWidth() {
    if (!window.ResizeObserver || state.galleryObserver) return;
    let width = 0;
    state.galleryObserver = new ResizeObserver((entries) => {
      const next = Math.round(entries[0]?.contentRect?.width || 0);
      if (next === width) return;
      width = next;
      relayout();
    });
    state.galleryObserver.observe(ui.gallery);
  }

  /* ── 选择模式 / 批量 ────────────────────────────────── */
  function setSelecting(on) {
    state.selecting = on;
    if (!on) state.selected.clear();
    ui.selectButton.classList.toggle("is-primary", on);
    ui.selectButton.lastChild.textContent = on ? t("退出选择") : t("选择");
    renderGallery();
  }

  function toggleSelected(item, card) {
    if (state.selected.has(item.id)) state.selected.delete(item.id); else state.selected.add(item.id);
    card.classList.toggle("is-selected", state.selected.has(item.id));
    renderBulk(visibleItems());
  }

  function renderBulk(visible) {
    ui.bulk.textContent = "";
    if (!state.selecting) return;
    const count = state.selected.size;
    ui.bulk.append(D.bulkBar({
      count, total: visible.length, noun: t("张"),
      onAll: () => { for (const item of visible) state.selected.add(item.id); renderGallery(); },
      onNone: () => { state.selected.clear(); renderGallery(); },
      actions: [
        { label: t("启用"), onClick: () => bulkPatch(true) },
        { label: t("禁用"), onClick: () => bulkPatch(false) },
        { label: t("删除"), icon: "trash-2", danger: true, onClick: bulkRemove }
      ]
    }));
  }

  function selectedItems() {
    return state.listing.items.filter((item) => state.selected.has(item.id));
  }

  async function bulkPatch(enabled) {
    const items = selectedItems();
    if (!items.length) return;
    await D.runBatch(items, (item) => D.api(`/api/dash/memes/items/${encodeURIComponent(item.id)}?${libQuery()}`, { method: "PATCH", body: { enabled } }), enabled ? t("启用") : t("禁用"));
    state.selected.clear();
    await loadItems();
  }

  async function bulkRemove() {
    const items = selectedItems();
    if (!items.length) return;
    const builtin = items.filter((item) => item.source === "builtin").length;
    const own = items.length - builtin;
    const parts = [];
    if (own) parts.push(t("删除 {count} 张自有表情(图片进回收站)", { count: own }));
    if (builtin) parts.push(t("禁用 {count} 张内置表情(文件不删)", { count: builtin }));
    const ok = await D.confirmAction(t("{parts}?平台引用记录保留。", { parts: parts.join(",") }), t("执行"));
    if (!ok) return;
    await D.runBatch(items, (item) => item.source === "builtin"
      ? D.api(`/api/dash/memes/items/${encodeURIComponent(item.id)}?${libQuery()}`, { method: "PATCH", body: { enabled: false } })
      : D.api(`/api/dash/memes/items/${encodeURIComponent(item.id)}?${libQuery()}&hard=false`, { method: "DELETE" }), t("删除"));
    state.selected.clear();
    await loadItems();
  }

  /* ── 详情抽屉 ────────────────────────────────────────── */
  function openDetail(item) {
    const refs = state.refs.get(item.id);
    const form = {
      name_zh: D.el("input.dash-select.dash-wide", { type: "text", value: item.name.zh, maxlength: "80" }),
      name_en: D.el("input.dash-select.dash-wide", { type: "text", value: item.name.en, maxlength: "80", placeholder: t("可选") }),
      description: D.el("textarea.dash-textarea", { rows: "3", maxlength: "500" }),
      usage: D.el("textarea.dash-textarea", { rows: "3", maxlength: "500" }),
      tags: D.el("input.dash-select.dash-wide", { type: "text", value: item.tags.join(", "), placeholder: t("逗号分隔,最多 16 个") })
    };
    form.description.value = item.description;
    form.usage.value = item.usage;
    const image = D.el("img.dash-meme-full", { src: imageUrl(item), alt: item.name.zh });
    const meta = [
      ["ID", item.short_id], [t("完整"), item.id.replace("sha256:", "")], [t("文件"), item.file], [t("类型"), t("{mime}{animated}", { mime: item.mime_type, animated: item.animated ? t(" · 动图") : "" })],
      [t("状态"), STATE_LABEL[itemState(item)]]
    ];
    if (item.origin) {
      const o = item.origin;
      meta.push([t("来源"), `${o.platform} ${o.conversation_kind} ${o.conversation_id}`], [t("发送者"), `${o.sender_name || "?"}(${o.sender_id})`]);
      if (o.sent_at) meta.push([t("发送于"), D.formatTime(o.sent_at)]);
      if (o.collected_at) meta.push([t("收集于"), D.formatTime(o.collected_at)]);
    } else {
      meta.push([t("来源"), item.source === "builtin" ? t("内置库") : t("手工添加")]);
    }
    if (refs) meta.push([t("平台引用"), t("收到 {inbound} · 发出 {outbound} · 最近 {last}", { inbound: refs.inbound, outbound: refs.outbound, last: D.formatTime(refs.last_seen_at) })]);

    const body = D.el("div", null,
      D.el("div.dash-meme-hero", null, image),
      D.el("div.dash-field-row", null, D.field(t("中文名"), form.name_zh), D.field(t("英文名"), form.name_en)),
      D.field(t("描述(图上是什么)"), form.description),
      D.field(t("用法(什么时候发)"), form.usage),
      D.field(t("标签"), form.tags),
      item.source === "builtin" ? D.el("p.dash-banner", { text: t("这是内置库条目:保存会把图片复制到自有库并生成覆盖项;删除只会禁用。") }) : null,
      item.origin?.reason ? D.el("div.dash-meme-reason", null, D.el("span.dash-meme-reason-label", { text: t("偷这张的理由") }), D.el("p", { text: item.origin.reason })) : null,
      D.el("h4.dash-section", { text: t("元数据") }),
      D.el("dl.dash-meta", null, meta.flatMap(([key, value]) => [D.el("dt", { text: key }), D.el("dd", { text: String(value) })])));

    const classify = D.el("button.dash-button", { type: "button", text: t("让模型重看"), title: t("调用视觉模型重新生成描述、用法、标签,填进表单不直接保存"), onclick: async () => {
      classify.disabled = true; classify.textContent = t("看图中…");
      try {
        const result = await D.api(`/api/dash/memes/items/${encodeURIComponent(item.id)}/classify?${libQuery()}`, { method: "POST" });
        form.name_zh.value = result.name?.zh || form.name_zh.value;
        form.name_en.value = result.name?.en || form.name_en.value;
        form.description.value = result.description || form.description.value;
        form.usage.value = result.usage || form.usage.value;
        form.tags.value = (result.tags || []).join(", ");
        D.toast(t("模型建议已填入(置信 {confidence})", { confidence: result.confidence }));
      } catch (error) {
        D.toast(t("重看失败:{error}", { error: error.message }), "error");
      } finally {
        classify.disabled = false; classify.textContent = t("让模型重看");
      }
    } });
    const toggle = D.el("button.dash-button", { type: "button", text: item.disabled ? t("启用") : t("禁用"), onclick: () => patch(item, { enabled: item.disabled }) });
    const remove = D.el("button.dash-button.is-danger", { type: "button", text: item.source === "builtin" ? t("禁用(内置)") : t("删除"), onclick: () => removeItem(item) });
    const save = D.el("button.dash-button.is-primary", { type: "button", text: t("保存"), onclick: () => patch(item, {
      name_zh: form.name_zh.value, name_en: form.name_en.value, description: form.description.value, usage: form.usage.value,
      tags: form.tags.value.split(/[,,、]/).map((t) => t.trim()).filter(Boolean)
    }) });
    D.openDrawer(item.name.zh, body, [classify, toggle, remove, save]);
  }

  async function patch(item, body) {
    try {
      await D.api(`/api/dash/memes/items/${encodeURIComponent(item.id)}?${libQuery()}`, { method: "PATCH", body });
      D.toast(t("已保存"));
      D.closeDrawer();
      await loadItems();
    } catch (error) {
      D.toast(t("保存失败:{error}", { error: error.message }), "error");
    }
  }

  async function removeItem(item) {
    if (item.source === "builtin") {
      const ok = await D.confirmAction(t("禁用内置表情「{name}」?内置库文件不会删除,模型将看不到它;之后可在“已禁用”里重新启用。", { name: item.name.zh }), t("禁用"));
      if (!ok) return;
      return patch(item, { enabled: false });
    }
    const hard = await D.confirmAction(t("删除「{name}」?\n\n图片文件会进回收站,索引条目移除;平台引用记录保留。", { name: item.name.zh }), t("删除"));
    if (!hard) return;
    try {
      await D.api(`/api/dash/memes/items/${encodeURIComponent(item.id)}?${libQuery()}&hard=false`, { method: "DELETE" });
      D.toast(t("已删除"));
      D.closeDrawer();
      await loadItems();
    } catch (error) {
      D.toast(t("删除失败:{error}", { error: error.message }), "error");
    }
  }

  /* ── 上传抽屉 ────────────────────────────────────────── */
  function openUpload() {
    const files = D.el("input", { type: "file", multiple: true, accept: "image/png,image/jpeg,image/gif,image/webp" });
    let mode = "ai";
    const modeSeg = D.segmented([{ value: "ai", label: t("模型分类") }, { value: "manual", label: t("手填元数据") }], mode, (value) => { mode = value; manualBox.hidden = value !== "manual"; });
    const form = {
      name_zh: D.el("input.dash-select.dash-wide", { type: "text", maxlength: "80", placeholder: t("必填") }),
      name_en: D.el("input.dash-select.dash-wide", { type: "text", maxlength: "80", placeholder: t("可选") }),
      description: D.el("textarea.dash-textarea", { rows: "2", maxlength: "500", placeholder: t("图上是什么(必填)") }),
      usage: D.el("textarea.dash-textarea", { rows: "2", maxlength: "500", placeholder: t("什么时候发(必填)") }),
      tags: D.el("input.dash-select.dash-wide", { type: "text", placeholder: t("逗号分隔") })
    };
    const manualBox = D.el("div", { hidden: true }, D.field(t("中文名"), form.name_zh), D.field(t("英文名"), form.name_en), D.field(t("描述"), form.description), D.field(t("用法"), form.usage), D.field(t("标签"), form.tags),
      D.el("p.dash-field-hint", { text: t("手填模式下多张图共用同一套元数据,适合一次传一张。") }));
    const log = D.el("ul.dash-upload-list");
    const body = D.el("div", null,
      D.field(t("图片"), files, t("PNG / JPEG / GIF / WebP;每边 32–4096 px;GIF ≤120 帧 15 秒;单张 ≤ 配置上限")),
      D.field(t("入库方式"), modeSeg.el, t("模型分类会用视觉模型看图并严格把关,不合格会拒绝;拒绝后可切手填强制入库")),
      manualBox,
      D.el("h4.dash-section", { text: t("结果") }), log);
    const submit = D.el("button.dash-button.is-primary", { type: "button", text: t("开始上传"), onclick: async () => {
      const list = Array.from(files.files || []);
      if (!list.length) { D.toast(t("先选图片"), "error"); return; }
      if (mode === "manual" && (!form.name_zh.value.trim() || !form.description.value.trim() || !form.usage.value.trim())) { D.toast(t("手填模式要填中文名、描述、用法"), "error"); return; }
      submit.disabled = true;
      let added = 0;
      for (const file of list) {
        const row = D.el("li", null, D.el("span.dash-cell-mono", { text: file.name }), D.el("span.dash-chip", { text: t("上传中…") }));
        log.append(row);
        const chip = row.lastChild;
        try {
          const params = new URLSearchParams({ library: state.library, mode });
          if (mode === "manual") {
            params.set("name_zh", form.name_zh.value); params.set("name_en", form.name_en.value);
            params.set("description", form.description.value); params.set("usage", form.usage.value); params.set("tags", form.tags.value);
          }
          const response = await fetch(`/api/dash/memes/items?${params}`, { method: "POST", body: await file.arrayBuffer(), headers: { "content-type": "application/octet-stream" } });
          const payload = await response.json().catch(() => null);
          if (!response.ok) throw new Error(payload?.error?.message || `HTTP ${response.status}`);
          if (payload.already_exists) { chip.textContent = t("已存在:{name}", { name: payload.name?.zh || "" }); chip.className = "dash-chip is-warn"; }
          else if (payload.rejected) { chip.textContent = t("模型拒绝:{reason}", { reason: payload.error || "" }); chip.className = "dash-chip is-danger"; chip.title = payload.error || ""; }
          else if (payload.needs_user_info) { chip.textContent = t("模型看不出来,切手填重传"); chip.className = "dash-chip is-danger"; chip.title = payload.error || ""; }
          else if (payload.success) { chip.textContent = t("已入库:{name}", { name: payload.name?.zh || "" }); chip.className = "dash-chip is-active"; added += 1; }
          else { chip.textContent = payload.message || t("未知结果"); chip.className = "dash-chip is-warn"; }
        } catch (error) {
          chip.textContent = t("失败:{error}", { error: error.message }); chip.className = "dash-chip is-danger";
        }
      }
      submit.disabled = false;
      if (added) { D.toast(t("入库 {count} 张", { count: added })); await loadItems(); }
    } });
    D.openDrawer(t("上传到 {library}", { library: state.library }), body, [submit]);
  }

  D.register({ name: "memes", root: "dashMemesRoot", mount, refresh: () => reloadAll() });
})();
