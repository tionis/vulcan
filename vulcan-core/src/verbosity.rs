//! Process-wide diagnostic verbosity shared by every Vulcan layer.
//!
//! The CLI derives one [`Verbosity`] from its global `-q`/`-v` flags and
//! threads it through application workflows, the daemon, and any Vulcan child
//! process it launches, so "how chatty" is decided once per invocation.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Environment variable naming a default verbosity level for one invocation.
pub const VERBOSITY_ENV: &str = "VULCAN_VERBOSITY";

/// Ordered diagnostic level: each level includes everything below it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    /// Suppress progress, warnings, and other non-essential stderr output.
    Quiet,
    /// Default operator-facing output.
    #[default]
    Normal,
    /// Extra operational detail (`-v`).
    Verbose,
    /// Developer diagnostics (`-vv`).
    Debug,
    /// Exhaustive tracing (`-vvv` and above).
    Trace,
}

impl Verbosity {
    /// Every level, quietest first.
    pub const ALL: [Self; 5] = [
        Self::Quiet,
        Self::Normal,
        Self::Verbose,
        Self::Debug,
        Self::Trace,
    ];

    /// Combines the counted `-v` flag with `-q`. Explicit flags win over
    /// `fallback` (normally the environment default); `-q` wins over `-v`.
    #[must_use]
    pub fn from_flags(verbose_count: u8, quiet: bool, fallback: Self) -> Self {
        if quiet {
            return Self::Quiet;
        }
        match verbose_count {
            0 => fallback,
            1 => Self::Verbose,
            2 => Self::Debug,
            _ => Self::Trace,
        }
    }

    #[must_use]
    pub fn is_quiet(self) -> bool {
        self == Self::Quiet
    }

    /// True at `-v` and above.
    #[must_use]
    pub fn is_verbose(self) -> bool {
        self >= Self::Verbose
    }

    /// True at `-vv` and above.
    #[must_use]
    pub fn is_debug(self) -> bool {
        self >= Self::Debug
    }

    /// True at `-vvv` and above.
    #[must_use]
    pub fn is_trace(self) -> bool {
        self >= Self::Trace
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Normal => "normal",
            Self::Verbose => "verbose",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// Global CLI arguments that reproduce this level in a child `vulcan`
    /// process. They must precede the subcommand.
    #[must_use]
    pub fn cli_args(self) -> &'static [&'static str] {
        match self {
            Self::Quiet => &["--quiet"],
            Self::Normal => &[],
            Self::Verbose => &["-v"],
            Self::Debug => &["-vv"],
            Self::Trace => &["-vvv"],
        }
    }
}

impl fmt::Display for Verbosity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVerbosityError(String);

impl fmt::Display for ParseVerbosityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown verbosity `{}` (expected quiet, normal, verbose, debug, or trace)",
            self.0
        )
    }
}

impl std::error::Error for ParseVerbosityError {}

impl FromStr for Verbosity {
    type Err = ParseVerbosityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        Self::ALL
            .into_iter()
            .find(|level| level.as_str().eq_ignore_ascii_case(trimmed))
            .ok_or_else(|| ParseVerbosityError(trimmed.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered_from_quiet_to_trace() {
        assert!(Verbosity::Quiet < Verbosity::Normal);
        assert!(Verbosity::Normal < Verbosity::Verbose);
        assert!(Verbosity::Verbose < Verbosity::Debug);
        assert!(Verbosity::Debug < Verbosity::Trace);
        assert_eq!(Verbosity::default(), Verbosity::Normal);
    }

    #[test]
    fn flags_map_counts_and_quiet_wins() {
        let fallback = Verbosity::Normal;
        assert_eq!(Verbosity::from_flags(0, false, fallback), Verbosity::Normal);
        assert_eq!(
            Verbosity::from_flags(1, false, fallback),
            Verbosity::Verbose
        );
        assert_eq!(Verbosity::from_flags(2, false, fallback), Verbosity::Debug);
        assert_eq!(Verbosity::from_flags(3, false, fallback), Verbosity::Trace);
        assert_eq!(Verbosity::from_flags(9, false, fallback), Verbosity::Trace);
        assert_eq!(Verbosity::from_flags(2, true, fallback), Verbosity::Quiet);
    }

    #[test]
    fn explicit_flags_override_the_fallback() {
        assert_eq!(
            Verbosity::from_flags(0, false, Verbosity::Debug),
            Verbosity::Debug
        );
        assert_eq!(
            Verbosity::from_flags(1, false, Verbosity::Trace),
            Verbosity::Verbose
        );
        assert_eq!(
            Verbosity::from_flags(0, true, Verbosity::Trace),
            Verbosity::Quiet
        );
    }

    #[test]
    fn predicates_are_cumulative() {
        assert!(Verbosity::Quiet.is_quiet());
        assert!(!Verbosity::Normal.is_verbose());
        assert!(Verbosity::Verbose.is_verbose() && !Verbosity::Verbose.is_debug());
        assert!(Verbosity::Debug.is_verbose() && Verbosity::Debug.is_debug());
        assert!(Verbosity::Trace.is_debug() && Verbosity::Trace.is_trace());
    }

    #[test]
    fn names_round_trip_and_cli_args_reproduce_each_level() {
        for level in Verbosity::ALL {
            assert_eq!(level.as_str().parse::<Verbosity>(), Ok(level));
            let args = level.cli_args();
            let count = args
                .iter()
                .filter_map(|arg| arg.strip_prefix('-'))
                .filter(|rest| rest.chars().all(|c| c == 'v'))
                .map(|rest| u8::try_from(rest.len()).expect("short count"))
                .sum();
            let quiet = args.contains(&"--quiet");
            assert_eq!(
                Verbosity::from_flags(count, quiet, Verbosity::Normal),
                level
            );
        }
        assert_eq!(" DEBUG ".parse::<Verbosity>(), Ok(Verbosity::Debug));
        assert!("loud".parse::<Verbosity>().is_err());
    }
}
