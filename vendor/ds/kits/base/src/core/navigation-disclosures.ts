/** A shell coordinates independent navigation disclosures without choosing any layout. */
export function createNavigationDisclosures() {
  const closers = new Set<() => void>();
  return {
    register(close: () => void) {
      closers.add(close);
      return { open() { for (const other of closers) if (other !== close) other(); }, destroy() { closers.delete(close); } };
    },
  };
}
