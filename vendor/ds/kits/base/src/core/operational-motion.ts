/** Native visibility/motion observation with adapter-owned cleanup. */
export function observeActivity(
  element: Element,
  change: (allowed: boolean) => void,
) {
  const doc = element.ownerDocument,
    win = doc.defaultView;
  if (!win) {
    change(false);
    return () => {};
  }
  const media = win.matchMedia?.("(prefers-reduced-motion: reduce)");
  const bounds = element.getBoundingClientRect();
  let inView =
    typeof win.IntersectionObserver !== "function" ||
    (bounds.width > 0 &&
      bounds.height > 0 &&
      bounds.bottom >= 0 &&
      bounds.right >= 0 &&
      bounds.top <= win.innerHeight &&
      bounds.left <= win.innerWidth);
  const update = () => change(inView && !doc.hidden && !media?.matches);
  const observer =
    typeof win.IntersectionObserver === "function"
      ? new win.IntersectionObserver((entries) => {
          inView = entries.some((e) => e.isIntersecting);
          update();
        })
      : undefined;
  observer?.observe(element);
  doc.addEventListener("visibilitychange", update);
  media?.addEventListener("change", update);
  update();
  return () => {
    observer?.disconnect();
    doc.removeEventListener("visibilitychange", update);
    media?.removeEventListener("change", update);
  };
}
/** Semantics update immediately; only paint receives an optional fade. */
export function valuePresentation(element: HTMLElement) {
  let key: unknown,
    initial = true,
    animation: Animation | undefined,
    allowed = false;
  const stop = observeActivity(element, (next) => {
    allowed = next;
    if (!next) animation?.cancel();
  });
  return {
    update(next: unknown, enabled: boolean) {
      const changed = !initial && key !== next;
      initial = false;
      key = next;
      animation?.cancel();
      if (changed && enabled && allowed)
        animation = element.animate?.([{ opacity: 0.45 }, { opacity: 1 }], {
          duration: 180,
          easing: "ease-out",
        });
    },
    destroy() {
      animation?.cancel();
      stop();
    },
  };
}
/** A decorative clone never owns committed state or focus. */
export function transferPresentation(
  host: HTMLElement,
  fromId: string,
  toId: string,
  duration: number,
) {
  const doc = host.ownerDocument,
    from = doc.getElementById(fromId),
    to = doc.getElementById(toId),
    win = doc.defaultView;
  if (
    !from ||
    !to ||
    from === to ||
    doc.hidden ||
    win?.matchMedia?.("(prefers-reduced-motion: reduce)").matches
  )
    return () => {};
  const a = from.getBoundingClientRect(),
    b = to.getBoundingClientRect();
  if (
    !a.width ||
    !a.height ||
    !b.width ||
    !b.height ||
    !win ||
    a.bottom < 0 ||
    a.top > win.innerHeight ||
    a.right < 0 ||
    a.left > win.innerWidth
  )
    return () => {};
  const clone = from.cloneNode(true) as HTMLElement;
  for (const node of [clone, ...clone.querySelectorAll<HTMLElement>("*")]) {
    node.removeAttribute("id");
    node.removeAttribute("name");
    node.removeAttribute("autofocus");
  }
  clone.inert = true;
  clone.setAttribute("aria-hidden", "true");
  clone.setAttribute("data-transfer-clone", "");
  Object.assign(clone.style, {
    position: "fixed",
    left: `${a.left}px`,
    top: `${a.top}px`,
    width: `${a.width}px`,
    height: `${a.height}px`,
    margin: "0",
    pointerEvents: "none",
    transformOrigin: "top left",
    zIndex: "2147483647",
  });
  const material = win.getComputedStyle(from);
  for (let i = 0; i < material.length; i++) {
    const property = material.item(i);
    if (property.startsWith("--reddb-"))
      clone.style.setProperty(property, material.getPropertyValue(property));
  }
  Object.assign(clone.style, {
    fontFamily: material.fontFamily,
    fontSize: material.fontSize,
    lineHeight: material.lineHeight,
    color: material.color,
  });
  doc.body.append(clone);
  let animation: Animation | undefined,
    disposed = false;
  let stop = () => {};
  const destroy = () => {
    if (disposed) return;
    disposed = true;
    stop();
    animation?.cancel();
    clone.remove();
  };
  stop = observeActivity(from, (allowed) => {
    if (!allowed) destroy();
  });
  if (disposed) {
    stop();
    return destroy;
  }
  animation = clone.animate?.(
    [
      { transform: "translate(0, 0) scale(1)", opacity: 0.9 },
      {
        transform: `translate(${b.left - a.left}px, ${b.top - a.top}px) scale(${b.width / a.width}, ${b.height / a.height})`,
        opacity: 0,
      },
    ],
    {
      duration: Math.max(
        0,
        Math.min(1500, Number.isFinite(duration) ? duration : 400),
      ),
      easing: "cubic-bezier(.16,1,.3,1)",
      fill: "forwards",
    },
  );
  if (animation) animation.finished.then(destroy, destroy);
  else destroy();
  return () => {
    stop();
    destroy();
  };
}
