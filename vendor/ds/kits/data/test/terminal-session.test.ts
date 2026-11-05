import { flushSync, mount, tick, unmount } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import Consumer from "./fixtures/terminal-session-consumer.svelte";
import type { TerminalEngine, TerminalEngineFactory, TerminalEngineOptions } from "../src/terminal-session";

const mounted: Array<{ instance: ReturnType<typeof mount<typeof Consumer>>; target: HTMLElement }> = [];
afterEach(async () => { for (const { instance, target } of mounted.splice(0)) { await unmount(instance); target.remove(); } vi.unstubAllGlobals(); });
function setup(factory: TerminalEngineFactory) {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const target = document.createElement("div"); document.body.append(target);
  const session = vi.fn(), interaction = vi.fn();
  const instance = mount(Consumer, { target, props: { factory, session, interaction } });
  mounted.push({ instance, target }); flushSync();
  return { target, instance, session, interaction };
}
const engine = (): TerminalEngine => ({ write: vi.fn(), clear: vi.fn(), focus: vi.fn(), fit: vi.fn(), update: vi.fn(), dispose: vi.fn() });
async function settle() { await tick(); await tick(); flushSync(); }

describe("terminal adapter ownership and lifecycle", () => {
  it("forwards transport events without executing or echoing input, then suppresses them in read-only mode", async () => {
    const live = engine(); let options!: TerminalEngineOptions;
    const { instance, target, interaction, session } = setup((_host, next) => { options = next; return live; });
    await settle();
    expect(session).toHaveBeenCalledWith(live);
    options.input("status\r"); options.resize(80, 24); options.resize(0, NaN);
    expect(interaction.mock.calls).toEqual([[{ type: "data", data: "status\r" }], [{ type: "resize", columns: 80, rows: 24 }]]);
    expect(live.write).not.toHaveBeenCalled(); expect(live.focus).not.toHaveBeenCalled();
    instance.lock(); await settle(); options.input("blocked");
    expect(interaction).toHaveBeenCalledTimes(2);
    expect(live.update).toHaveBeenLastCalledWith(expect.objectContaining({ readOnly: true }));
    expect(target.querySelector("details pre")?.textContent).toBe("<script>caller text</script>");
    expect(target.querySelector("script")).toBeNull();
  });

  it("aborts replacement work, disposes late results and ignores stale input", async () => {
    let resolve!: (value: TerminalEngine) => void; let stale!: TerminalEngineOptions;
    const pending = new Promise<TerminalEngine>(done => resolve = done);
    const old = engine(), next = engine();
    const { instance, session, interaction } = setup((_host, options) => { stale = options; return pending; });
    await settle(); instance.replace(() => next); await settle();
    expect(stale.signal.aborted).toBe(true); resolve(old); await settle();
    expect(old.dispose).toHaveBeenCalledTimes(1); expect(session.mock.calls).toEqual([[next]]);
    stale.input("stale"); expect(interaction).not.toHaveBeenCalled();
    instance.replace(() => engine()); await settle();
    expect(next.dispose).toHaveBeenCalledTimes(1); expect(session.mock.calls[1]).toEqual([null]);
  });

  it("keeps the transcript after failure and restores explicit retry focus", async () => {
    const live = engine(); const factory = vi.fn<TerminalEngineFactory>().mockRejectedValueOnce(new Error("Unavailable")).mockResolvedValue(live);
    const { target } = setup(factory); await settle();
    expect(target.querySelector("[data-terminal-fallback]")?.textContent).toBe("<script>caller text</script>");
    const retry = target.querySelector("button")!; retry.focus(); retry.click(); await settle();
    expect(factory).toHaveBeenCalledTimes(2); expect(target.querySelector("[data-terminal-session]")?.getAttribute("data-phase")).toBe("ready");
    await vi.waitFor(() => expect(live.focus).toHaveBeenCalledTimes(1));
  });
});
