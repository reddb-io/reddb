import type { Tone } from "@reddb-io/design-system/base";
/** A categorical entity. State is caller-owned; color never replaces its text. */
export interface StateCell {
  id: string;
  label: string;
  state: string;
  description?: string;
  tone?: Tone;
  disabled?: boolean;
}
/** A retained event is immutable and ordered by the caller. */
export interface LogEvent {
  id: string;
  datetime: string;
  time: string;
  message: string;
  source?: string;
  tone?: Tone;
}
export function uniqueEntities<T extends { id: string }>(
  items: readonly T[],
  name: string,
): readonly T[] {
  const ids = new Set<string>();
  for (const item of items) {
    if (!item.id || ids.has(item.id))
      throw new Error(`${name}: ids must be non-empty and unique`);
    ids.add(item.id);
  }
  return items;
}
export function retainedEvents(
  events: readonly LogEvent[],
  limit: number,
  query: string,
) {
  uniqueEntities(events, "EventLog");
  const bound = Math.min(
    10000,
    Math.max(1, Math.floor(Number.isFinite(limit) ? limit : 200)),
  );
  const retained = events.slice(-bound),
    search = query.toLocaleLowerCase();
  return {
    discarded: Math.max(0, events.length - bound),
    events: search
      ? retained.filter((e) =>
          `${e.source ?? ""} ${e.message}`.toLocaleLowerCase().includes(search),
        )
      : retained,
  };
}
/** Native follow-tail; user navigation immediately suspends automatic scrolling. */
export function followEvents(element: HTMLElement, onPause: () => void) {
  let following = false;
  const userScroll = () => {
    if (
      following &&
      element.scrollHeight - element.clientHeight - element.scrollTop > 4
    ) {
      following = false;
      onPause();
    }
  };
  const stopOnNavigation = (event: Event) => {
    if (
      following &&
      (event instanceof KeyboardEvent
        ? ["ArrowUp", "PageUp", "Home"].includes(event.key)
        : true)
    ) {
      following = false;
      onPause();
    }
  };
  element.addEventListener("scroll", userScroll);
  element.addEventListener("wheel", stopOnNavigation, { passive: true });
  element.addEventListener("touchstart", stopOnNavigation, { passive: true });
  element.addEventListener("keydown", stopOnNavigation);
  return {
    update(enabled: boolean) {
      following = enabled;
      if (enabled) element.scrollTop = element.scrollHeight;
    },
    destroy() {
      element.removeEventListener("scroll", userScroll);
      element.removeEventListener("wheel", stopOnNavigation);
      element.removeEventListener("touchstart", stopOnNavigation);
      element.removeEventListener("keydown", stopOnNavigation);
    },
  };
}

export function stateCellWidth(value: number) {
  return Math.max(8, Math.min(40, Number.isFinite(value) ? value : 12));
}

/** Semantic ink for an explicit state; neutral inherits the surrounding role. */
export function operationalToneInk(tone?: Tone): string | undefined {
  return tone && tone !== "neutral"
    ? `var(--reddb-color-feedback-${tone}-foreground)`
    : undefined;
}
