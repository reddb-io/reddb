

/** Portable ApplicationShell properties shared by native adapters. */
export interface ApplicationShellContract {
/** The id shared by the keyboard bypass and the main landmark. */
    mainId?: string;
/** Let layouts consume the viewport edge. Header content owns its vertical rhythm and shares the navigation plane, so a Navbar is flush with the shell. */
    fullWidth?: boolean;
/**
     * The shell is embedded in a host document that already owns the page's
     * `<main>` (a preview, a documentation frame). Its main region stays the
     * focusable skip-link target but is not a second `main` landmark. Defaults
     * to `true` inside another ApplicationShell, which owns `<main>`, and to
     * `false` everywhere else.
     */
    embedded?: boolean;
}
