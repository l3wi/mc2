//! Thin interactive prompt wrappers over `dialoguer`.
//!
//! Kept behind one module so the wizard trees read clearly and the underlying
//! prompt library is swappable. All helpers return `Result` so failures (e.g.
//! non-tty stdin) surface as friendly errors.

use anyhow::{bail, Context, Result};

/// True when stdin is an interactive terminal (prompts require one).
pub fn is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Guard used before entering any interactive tree.
pub fn ensure_tty() -> Result<()> {
    if is_terminal() {
        Ok(())
    } else {
        bail!("mc2 setup is interactive — run it in a terminal (stdin is not a TTY)")
    }
}

/// Arrow-key single selection. Returns the chosen item index.
pub fn select(title: &str, items: &[&str]) -> Result<usize> {
    dialoguer::Select::new()
        .with_prompt(title)
        .items(items)
        .default(0)
        .interact()
        .context("interactive selection failed (no TTY?)")
}

/// Free-text input with a default; `Enter` accepts the default.
pub fn input(prompt: &str, default: &str, allow_empty: bool) -> Result<String> {
    dialoguer::Input::<String>::new()
        .with_prompt(prompt)
        .default(default.to_string())
        .allow_empty(allow_empty)
        .interact_text()
        .context("text input failed (no TTY?)")
}

/// Yes/no confirmation with a default.
pub fn confirm(prompt: &str, default: bool) -> Result<bool> {
    dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(default)
        .interact()
        .context("confirmation failed (no TTY?)")
}

/// Masked secret input (e.g. the API token).
pub fn secret(prompt: &str) -> Result<String> {
    dialoguer::Password::new()
        .with_prompt(prompt)
        .allow_empty_password(false)
        .interact()
        .context("secret input failed (no TTY?)")
}
