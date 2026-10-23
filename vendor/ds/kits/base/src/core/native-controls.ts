/** A locked selection reports no change when it is already pressed. */
export function nextPressed(pressed: boolean, allowUnpress: boolean): boolean {
  return pressed && !allowUnpress ? pressed : !pressed;
}

/** Keep a native radio group usable for fractional and non-finite stop counts. */
export function ratingStops(max: number): number[] {
  const count = Number.isFinite(max) ? Math.max(1, Math.floor(max)) : 5;
  return Array.from({ length: count }, (_, index) => index + 1);
}

/** File feedback never exposes a local filesystem path. */
export function fileNames(files: FileList | null | undefined): string[] {
  return Array.from(files ?? [], file => file.name);
}

/**
 * A browser may flush microtasks between reset listeners and the default action.
 * Reconcile in the next task, after native controls and outputs have reset.
 */
export function afterFormReset(form: HTMLFormElement | null | undefined, reconcile: () => void): () => void {
  const pending = new Set<ReturnType<typeof setTimeout>>();
  const reset = (event: Event) => {
    const timer = setTimeout(() => {
      pending.delete(timer);
      if (!event.defaultPrevented) reconcile();
    }, 0);
    pending.add(timer);
  };
  form?.addEventListener("reset", reset);
  return () => {
    form?.removeEventListener("reset", reset);
    pending.forEach(timer => clearTimeout(timer));
  };
}

/** Resolve explicit form ownership before a child snippet has patched its native attribute. */
export function associatedForm(control: HTMLInputElement | HTMLFieldSetElement | null | undefined, formId?: string | null): HTMLFormElement | null {
  if (!control) return null;
  if (formId == null) return control.form;
  const owner = control.ownerDocument.getElementById(formId);
  return owner?.tagName === "FORM" ? owner as HTMLFormElement : null;
}
