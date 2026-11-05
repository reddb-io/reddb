import type { Tone } from "../tone";
/** Resolve the canonical tone before translating it into announcement urgency. */
export function feedbackTone(tone?: Tone, legacy?: Tone): Tone { return tone ?? legacy ?? "neutral"; }
/** Errors interrupt; other feedback is a polite status update. */
export function feedbackAnnouncement(tone: Tone): "alert" | "status" { return tone === "danger" ? "alert" : "status"; }
