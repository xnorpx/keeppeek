use std::io::IsTerminal;

/// Selects the dashboard for a foreground server process.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DashboardMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy)]
struct TerminalFacts {
    stdin: bool,
    stdout: bool,
    stderr: bool,
    supports_screen: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Presentation {
    Dashboard,
    Plain,
    Unavailable,
}

impl DashboardMode {
    pub(super) fn presentation(self) -> Presentation {
        self.select(TerminalFacts {
            stdin: std::io::stdin().is_terminal(),
            stdout: std::io::stdout().is_terminal(),
            stderr: std::io::stderr().is_terminal(),
            supports_screen: std::env::var_os("TERM")
                .map_or(cfg!(windows), |term| !term.is_empty() && term != "dumb"),
        })
    }

    const fn select(self, terminal: TerminalFacts) -> Presentation {
        if matches!(self, Self::Never) {
            return Presentation::Plain;
        }
        if terminal.stdin && terminal.stdout && terminal.stderr && terminal.supports_screen {
            return Presentation::Dashboard;
        }
        match self {
            Self::Always => Presentation::Unavailable,
            Self::Auto | Self::Never => Presentation::Plain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_terminal_facts_and_startup_modes() {
        for bits in 0..16 {
            let facts = TerminalFacts {
                stdin: bits & 1 != 0,
                stdout: bits & 2 != 0,
                stderr: bits & 4 != 0,
                supports_screen: bits & 8 != 0,
            };
            let auto = if bits == 15 {
                Presentation::Dashboard
            } else {
                Presentation::Plain
            };
            let always = if bits == 15 {
                Presentation::Dashboard
            } else {
                Presentation::Unavailable
            };
            assert_eq!(DashboardMode::Auto.select(facts), auto);
            assert_eq!(DashboardMode::Always.select(facts), always);
            assert_eq!(DashboardMode::Never.select(facts), Presentation::Plain);
        }
    }
}
