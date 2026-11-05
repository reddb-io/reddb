import type { TerminalEngineFactory } from "../../terminal-session";
/** Portable TerminalSession properties shared by native adapters. */
export interface TerminalSessionContract {
    /** Visible and accessible session name, also passed to the engine's input. */
    label: string;
    /** Application-owned lazy engine factory. No engine, CSS, transport or service is imported by this component. */
    engine: TerminalEngineFactory;
    /** Plain-text snapshot served in HTML and retained as a transcript after mounting. Never executed or parsed as markup. */
    transcript?: string;
    /** Density control-height multiples reserved for the terminal; defaults to 10. */
    rows?: number;
    /** Defers adapter creation until near the viewport; eager mounts immediately. */
    loading?: "lazy" | "eager";
    /** Suppresses input forwarding and makes the engine read-only. */
    readOnly?: boolean;
    /** Status text while the optional engine loads. */
    loadingLabel?: string;
    /** Status text after the optional engine mounts. */
    readyLabel?: string;
    /** Failure message; the transcript remains readable and retry is available. */
    errorLabel?: string;
    /** Label for retrying a failed engine load. */
    retryLabel?: string;
    /** Label of the retained plain-text snapshot disclosure. */
    transcriptLabel?: string;
}
