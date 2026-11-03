//! Which terminal is on the other end, read from the environment. It decides what
//! `COLORTERM` and `TERM` can't say: tmux, SSH and Windows Terminal drop
//! `COLORTERM` yet still show 24-bit color, only some terminals turn links into
//! something to click, and the old Windows console has no font fallback, so
//! glyphs its font lacks show as boxes.

use std::env;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Emulator {
    AppleTerminal,
    Iterm2,
    Ghostty,
    Kitty,
    WezTerm,
    Alacritty,
    Rio,
    Foot,
    /// GNOME Terminal and the others built on VTE, with its version.
    Vte(u32),
    /// VS Code and the editors built on it, whose terminal is xterm.js.
    VsCode,
    Zed,
    Warp,
    WindowsTerminal,
    JetBrains,
    Unknown,
}

/// Blocks the old Windows console font draws, for the fallbacks to stay within.
#[cfg(test)]
pub const CONSOLAS_BLOCKS: &str = " ░▒▓█▌▐▄▀";

/// VTE gained links in 0.50.
const VTE_LINKS: u32 = 5000;

static TERMINAL: LazyLock<Emulator> = LazyLock::new(|| detect(&|name| env::var(name).ok()));
static FULL_GLYPHS: LazyLock<bool> =
    LazyLock::new(|| match env::var("GIGSIFT_LEGACY_CONSOLE").as_deref() {
        Ok("1" | "true") => false,
        Ok("0" | "false") => true,
        _ => !cfg!(windows) || current().bundles_fonts(),
    });

pub fn current() -> Emulator {
    *TERMINAL
}

/// False in the old Windows console, which draws with Consolas and nothing else.
/// Set `GIGSIFT_LEGACY_CONSOLE` to 1 or 0 where the guess is wrong.
pub fn full_glyphs() -> bool {
    *FULL_GLYPHS
}

impl Emulator {
    pub fn truecolor(self) -> bool {
        matches!(
            self,
            Self::Iterm2
                | Self::Ghostty
                | Self::Kitty
                | Self::WezTerm
                | Self::Alacritty
                | Self::Rio
                | Self::Foot
                | Self::Vte(_)
                | Self::VsCode
                | Self::Zed
                | Self::Warp
                | Self::WindowsTerminal
        )
    }

    /// Terminal.app garbles the escape code, and Warp and JetBrains print it.
    pub fn links(self) -> bool {
        match self {
            Self::Vte(version) => version >= VTE_LINKS,
            Self::Iterm2
            | Self::Ghostty
            | Self::Kitty
            | Self::WezTerm
            | Self::Alacritty
            | Self::Rio
            | Self::Foot
            | Self::VsCode
            | Self::Zed
            | Self::WindowsTerminal => true,
            _ => false,
        }
    }

    /// Terminals that bring their own font or fall back to another one.
    fn bundles_fonts(self) -> bool {
        matches!(
            self,
            Self::WindowsTerminal
                | Self::VsCode
                | Self::Zed
                | Self::WezTerm
                | Self::Kitty
                | Self::Alacritty
                | Self::Ghostty
                | Self::Rio
        )
    }
}

/// Markers that survive tmux come before `TERM_PROGRAM`, which tmux replaces, and
/// `LC_TERMINAL` survives SSH.
fn detect(var: &dyn Fn(&str) -> Option<String>) -> Emulator {
    let var = |name: &str| var(name).filter(|value| !value.is_empty());
    let term = var("TERM").unwrap_or_default();
    if var("VSCODE_GIT_ASKPASS_MAIN").is_some() || var("CURSOR_TRACE_ID").is_some() {
        return Emulator::VsCode;
    }
    if let Some(program) = var("TERM_PROGRAM").and_then(|program| from_program(&program)) {
        return program;
    }
    if var("TERMINAL_EMULATOR").is_some_and(|name| name.to_ascii_lowercase().contains("jetbrains"))
    {
        return Emulator::JetBrains;
    }
    if var("WEZTERM_VERSION").is_some() {
        return Emulator::WezTerm;
    }
    if var("ITERM_SESSION_ID").is_some()
        || var("LC_TERMINAL").is_some_and(|name| name.eq_ignore_ascii_case("iterm2"))
    {
        return Emulator::Iterm2;
    }
    // iTerm2 sets this too, so it has to come after.
    if var("TERM_SESSION_ID").is_some() {
        return Emulator::AppleTerminal;
    }
    if var("KITTY_WINDOW_ID").is_some() || term.contains("kitty") {
        return Emulator::Kitty;
    }
    if var("GHOSTTY_RESOURCES_DIR").is_some() || term.contains("ghostty") {
        return Emulator::Ghostty;
    }
    if var("ALACRITTY_WINDOW_ID").is_some() || term == "alacritty" {
        return Emulator::Alacritty;
    }
    if term == "rio" {
        return Emulator::Rio;
    }
    if term.starts_with("foot") {
        return Emulator::Foot;
    }
    if let Some(version) = var("VTE_VERSION").and_then(|version| version.parse().ok()) {
        return Emulator::Vte(version);
    }
    if var("WT_SESSION").is_some() {
        return Emulator::WindowsTerminal;
    }
    Emulator::Unknown
}

fn from_program(program: &str) -> Option<Emulator> {
    let name: String = program
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|letter| letter.to_ascii_lowercase())
        .collect();
    Some(match name.as_str() {
        "appleterminal" => Emulator::AppleTerminal,
        "itermapp" | "iterm2" => Emulator::Iterm2,
        "ghostty" => Emulator::Ghostty,
        "wezterm" => Emulator::WezTerm,
        "kitty" => Emulator::Kitty,
        "alacritty" => Emulator::Alacritty,
        "rio" => Emulator::Rio,
        "vscode" => Emulator::VsCode,
        "zed" => Emulator::Zed,
        "warpterminal" => Emulator::Warp,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detected(vars: &[(&str, &str)]) -> Emulator {
        detect(&|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn term_program_names_the_terminal() {
        assert_eq!(detected(&[("TERM_PROGRAM", "iTerm.app")]), Emulator::Iterm2);
        assert_eq!(detected(&[("TERM_PROGRAM", "ghostty")]), Emulator::Ghostty);
        assert_eq!(
            detected(&[("TERM_PROGRAM", "Apple_Terminal")]),
            Emulator::AppleTerminal
        );
        assert_eq!(
            detected(&[("TERM_PROGRAM", "WarpTerminal")]),
            Emulator::Warp
        );
    }

    #[test]
    fn inside_tmux_the_outer_terminal_still_shows() {
        let iterm = [
            ("TERM_PROGRAM", "tmux"),
            ("TERM", "tmux-256color"),
            ("TERM_SESSION_ID", "w0t0p0"),
            ("ITERM_SESSION_ID", "w0t0p0:1"),
        ];
        assert_eq!(detected(&iterm), Emulator::Iterm2);
        let apple = [("TERM_PROGRAM", "tmux"), ("TERM_SESSION_ID", "1234")];
        assert_eq!(detected(&apple), Emulator::AppleTerminal);
    }

    #[test]
    fn editors_win_over_the_terminal_they_report() {
        let cursor = [("TERM_PROGRAM", "vscode"), ("CURSOR_TRACE_ID", "abc")];
        assert_eq!(detected(&cursor), Emulator::VsCode);
    }

    #[test]
    fn term_names_terminals_that_set_nothing_else() {
        assert_eq!(detected(&[("TERM", "xterm-kitty")]), Emulator::Kitty);
        assert_eq!(detected(&[("TERM", "foot-extra")]), Emulator::Foot);
        assert_eq!(detected(&[("TERM", "xterm-256color")]), Emulator::Unknown);
    }

    #[test]
    fn old_vte_gets_no_links() {
        assert!(!detected(&[("VTE_VERSION", "4803")]).links());
        assert!(detected(&[("VTE_VERSION", "7600")]).links());
    }

    #[test]
    fn windows_terminal_shows_color_and_links() {
        let terminal = detected(&[("WT_SESSION", "a-guid")]);
        assert!(terminal.truecolor() && terminal.links() && terminal.bundles_fonts());
    }

    #[test]
    fn an_empty_value_counts_as_unset() {
        assert_eq!(
            detected(&[("TERM_PROGRAM", ""), ("WT_SESSION", "")]),
            Emulator::Unknown
        );
    }

    #[test]
    fn unknown_and_apple_terminals_get_no_links() {
        assert!(!Emulator::Unknown.links());
        assert!(!Emulator::AppleTerminal.links());
        assert!(!Emulator::Warp.links());
    }
}
