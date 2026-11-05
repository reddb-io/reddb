// The DS deprecation window, as code (ADR 0026).
//
// A public rename keeps the old name as a working alias for exactly one CalVer
// release, and that release's notes carry a **BREAKING:** line. While the
// alias lives, using it warns once per session in a development build (the
// consumer's bundler sets `import.meta.env.DEV`); production keeps the alias
// silently. The next release removes the alias.

const warned = new Set<string>();

/**
 * Warn, once per session and only in development, that `component` received a
 * deprecated name. `used` is what the caller wrote (`intent="danger"`),
 * `instead` the canonical spelling (`tone="danger"`).
 */
export function warnDeprecated(component: string, used: string, instead: string): void {
  if (!(import.meta as ImportMeta & { readonly env?: { readonly DEV?: boolean } }).env?.DEV) return;
  const key = `${component}\u0000${used}`;
  if (warned.has(key)) return;
  warned.add(key);
  console.warn(
    `[reddb ${component}] ${used} is deprecated and is removed in the next release. Use ${instead}.`,
  );
}
