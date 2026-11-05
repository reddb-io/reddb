import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import ChatMessage from "../src/composites/ChatMessage.svelte";
import { chatMessage } from "../src/composites/chat-message.variants";
import SocialSurfacesConsumer from "./fixtures/SocialSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const body = createRawSnippet(() => ({
  render: () => '<p>Deployment is ready. <a href="/deployments/42">Open it</a></p>',
}));
const actions = createRawSnippet(() => ({
  render: () => '<button type="button">Reply</button>',
}));

describe("ChatMessage", () => {
  it("composes canonical media and avatar around caller-owned message content", () => {
    const message = rendered(render(ChatMessage, {
      author: "Ada Lovelace",
      avatarFallback: "AL",
      datetime: "2026-08-08T19:00:00Z",
      time: "19:00",
      children: body,
      actions,
    }));

    expect(message.tagName).toBe("ARTICLE");
    expect(message.getAttribute("aria-label")).toBe("Message from Ada Lovelace");
    expect(message.querySelector("[data-media-object]")).not.toBeNull();
    expect(message.querySelector("[data-avatar]")?.getAttribute("aria-label")).toBe("Ada Lovelace");
    expect(message.querySelector("time")?.dateTime).toBe("2026-08-08T19:00:00Z");
    expect(message.querySelector("[data-chat-message-body]")?.textContent).toContain("Deployment is ready.");
  });

  it("leaves message destinations and actions in native keyboard order", () => {
    const message = rendered(render(ChatMessage, {
      author: "Ada Lovelace",
      avatarFallback: "AL",
      datetime: "2026-08-08T19:00:00Z",
      time: "19:00",
      children: body,
      actions,
    }));
    const controls = [...message.querySelectorAll<HTMLElement>("a, button")];

    expect(controls.map((control) => control.textContent)).toEqual(["Open it", "Reply"]);
    for (const control of controls) {
      control.focus();
      expect(document.activeElement).toBe(control);
    }
  });

  it("keeps Density tokenized and inherits every nested appearance axis", () => {
    const scope = rendered(render(SocialSurfacesConsumer));
    const message = scope.querySelector<HTMLElement>("[data-chat-message]")!;
    const styles = chatMessage();

    expect(classes(message)).toEqual(expect.objectContaining(classesOf(styles.root())));
    expect(classes(message.querySelector("[data-chat-message-header]")!))
      .toContain("gap-[var(--reddb-spatial-gap-sm)]");
    expect(message.hasAttribute("data-theme")).toBe(false);
    expect(message.hasAttribute("data-color-scheme")).toBe(false);
    expect(message.hasAttribute("data-density")).toBe(false);
    expect(message.hasAttribute("data-motion")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("chat-message", "ChatMessage");
  });
});
