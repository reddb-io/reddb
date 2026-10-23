import type {
  SecretValueContract,
  SecretHideReason,
} from "./contracts/secretvalue";
export function secretIsVisible(options: SecretValueContract): boolean {
  return (
    !!options.revealable &&
    !!options.revealed &&
    !options.pending &&
    !options.error &&
    options.value != null
  );
}
/** Portable exposure lifetime. Refreshing props never extends the current deadline. */
export function createSecretExposure(
  read: () => SecretValueContract,
  publish: (visible: boolean) => void,
) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let visible = false,
    blocked = false,
    observed = false;
  let previousValue: string | null | undefined;
  const clear = () => {
    clearTimeout(timer);
    timer = undefined;
  };
  const hide = (reason: SecretHideReason) => {
    clear();
    blocked = true;
    const was = visible;
    visible = false;
    publish(false);
    if (was || reason === "control") read().onrevealedchange?.(false, reason);
  };
  return {
    sync() {
      const options = read();
      if (!options.revealed) blocked = false;
      if (visible && previousValue !== options.value) {
        hide("value-change");
        previousValue = options.value;
        return;
      }
      const next = secretIsVisible(options) && !blocked;
      if (visible && !next) {
        hide("unavailable");
        previousValue = options.value;
        observed = !!options.revealed;
        return;
      }
      if (options.revealed && !observed) blocked = false;
      observed = !!options.revealed;
      if (next && !visible) {
        clear();
        const lifetime = Number.isFinite(options.hideAfter ?? 30000)
          ? Math.max(0, options.hideAfter ?? 30000)
          : 30000;
        if (lifetime > 0) timer = setTimeout(() => hide("timeout"), lifetime);
      }
      visible = next;
      previousValue = options.value;
      publish(next);
    },
    matches(value: string | null | undefined) {
      return visible && value === previousValue;
    },
    request(next: boolean) {
      if (!next) hide("control");
      else if (read().revealable && !read().pending)
        read().onrevealedchange?.(true, "control");
    },
    hide,
    destroy() {
      clear();
    },
  };
}
