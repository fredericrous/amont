//! What a person does with amont, one test per journey (decisions plan
//! 2026-10-17-amont-tests-follow-user-flows, Phase 5).
//!
//! Each flow builds its repository once and walks the journey, asserting
//! what each step leaves on the terminal, in the refs and on disk: amont's
//! public interface. Every assertion names its step, so a failure says
//! where the journey broke, not only that it did. Edge cases stay where they
//! are cheap: tables beside the rule, unit tests beside the mechanism.

/// `assert!` for a flow: the message starts with the step's name.
///
/// ```ignore
/// step!("push", run.passed(), "{}", run.output());
/// ```
macro_rules! step {
    ($step:literal, $cond:expr $(,)?) => {
        assert!($cond, "step `{}`: {}", $step, stringify!($cond))
    };
    ($step:literal, $cond:expr, $($arg:tt)+) => {
        assert!($cond, "step `{}`: {}", $step, format_args!($($arg)+))
    };
}

/// `assert_eq!` for a flow, named the same way.
macro_rules! step_eq {
    ($step:literal, $left:expr, $right:expr $(,)?) => {
        assert_eq!($left, $right, "step `{}`", $step)
    };
    ($step:literal, $left:expr, $right:expr, $($arg:tt)+) => {
        assert_eq!($left, $right, "step `{}`: {}", $step, format_args!($($arg)+))
    };
}
