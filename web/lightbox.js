"use strict";

/*
 * 对话里的图片点开后的放大预览。
 *
 * 以前那三个按钮（送进预览工作区 / 新窗口打开 / 下载）直接铺在图片下面一行，
 * 每张图都占一行、还抢注意力。现在图片本身是入口：点一下,背景压暗、图放大到
 * 视口里,按钮跟着浮层走。
 *
 * 单独成文件而不是塞进 app.js —— 那边已经将近一万行,不该再长。
 * 依赖只有一个 `makeIconSlot`,由 app.js 注入(图标的实际绘制在那边)。
 */
window.MiyuLightbox = (() => {
  let makeIconSlot = null;
  let root = null;
  let image = null;
  let caption = null;
  let workspaceButton = null;
  let externalLink = null;
  let downloadLink = null;
  let lastFocused = null;
  let current = null;

  function iconButton(icon, label, { tag = "button" } = {}) {
    const node = document.createElement(tag);
    if (tag === "button") node.type = "button";
    node.className = "icon-button lightbox-action";
    node.title = label;
    node.setAttribute("aria-label", label);
    node.appendChild(makeIconSlot(icon));
    return node;
  }

  function build() {
    root = document.createElement("div");
    root.className = "lightbox";
    root.hidden = true;
    root.setAttribute("role", "dialog");
    root.setAttribute("aria-modal", "true");
    root.setAttribute("aria-label", t("图片预览"));

    const scrim = document.createElement("button");
    scrim.type = "button";
    scrim.className = "lightbox-scrim";
    scrim.tabIndex = -1;
    scrim.setAttribute("aria-label", t("关闭预览"));
    scrim.addEventListener("click", close);

    const frame = document.createElement("div");
    frame.className = "lightbox-frame";

    image = document.createElement("img");
    image.className = "lightbox-image";
    image.decoding = "async";
    image.alt = "";

    const bar = document.createElement("div");
    bar.className = "lightbox-bar";

    caption = document.createElement("span");
    caption.className = "lightbox-caption";

    workspaceButton = iconButton("panel-right", t("在预览工作区打开"));
    workspaceButton.addEventListener("click", () => {
      const target = current;
      close();
      target?.onOpenInWorkspace?.();
    });

    externalLink = iconButton("external-link", t("在新窗口打开图片"), { tag: "a" });
    externalLink.target = "_blank";
    externalLink.rel = "noreferrer noopener";

    downloadLink = iconButton("download", t("下载图片"), { tag: "a" });
    downloadLink.setAttribute("download", "");

    const closeButton = iconButton("x", t("关闭预览"));
    closeButton.classList.add("lightbox-close");
    closeButton.addEventListener("click", close);

    bar.append(caption, workspaceButton, externalLink, downloadLink, closeButton);
    frame.append(image, bar);
    root.append(scrim, frame);
    document.body.appendChild(root);
  }

  function onKeydown(event) {
    if (event.key === "Escape") {
      event.stopPropagation();
      close();
    }
  }

  function open(options) {
    const url = options?.url;
    if (!url) return;
    if (!makeIconSlot) return;
    if (!root) build();
    current = options;
    image.src = url;
    image.alt = options.name || "";
    caption.textContent = options.name || "";
    caption.title = options.name || "";
    externalLink.href = url;
    downloadLink.href = url;
    // 送进工作区这条只在调用方给了回调时才有意义。
    workspaceButton.hidden = typeof options.onOpenInWorkspace !== "function";
    lastFocused = document.activeElement;
    root.hidden = false;
    document.body.classList.add("has-lightbox");
    document.addEventListener("keydown", onKeydown, true);
    requestAnimationFrame(() => root.classList.add("is-open"));
  }

  function close() {
    if (!root || root.hidden) return;
    root.classList.remove("is-open");
    root.hidden = true;
    // 松开 src,免得一张大图一直占着内存。
    image.removeAttribute("src");
    current = null;
    document.body.classList.remove("has-lightbox");
    document.removeEventListener("keydown", onKeydown, true);
    if (lastFocused && document.contains(lastFocused)) lastFocused.focus();
    lastFocused = null;
  }

  return {
    init(deps) {
      makeIconSlot = deps?.makeIconSlot || null;
    },
    open,
    close,
    isOpen: () => Boolean(root) && !root.hidden,
  };
})();
