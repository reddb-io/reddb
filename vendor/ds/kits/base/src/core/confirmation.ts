/** A confirmation belongs to one overlay session; stale work cannot close a reopened session. */
export function createConfirmationSession(changed: (pending: boolean) => void) {
  let generation = 0, pending = false;
  return {
    reset() { generation++; pending = false; changed(false); },
    confirm(action: () => unknown | Promise<unknown>, commit: () => void): void | Promise<void> {
      if (pending) return;
      const ticket = ++generation;
      pending = true; changed(true);
      const finish = (accepted: boolean) => {
        if (ticket !== generation) return;
        pending = false; changed(false);
        if (accepted) commit();
      };
      try {
        const result = action();
        if (result && typeof (result as PromiseLike<unknown>).then === "function") {
          return Promise.resolve(result).then(() => finish(true), () => finish(false));
        }
        finish(true);
      } catch { finish(false); }
    },
  };
}
