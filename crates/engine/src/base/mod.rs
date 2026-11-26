//! The bottom layer at the path the root gives it: `wsp-base`'s command line,
//! git, labels and seat names, re-exported, and the build stamp, which the
//! binary hands in because the root's `build.rs` writes the variables it reads.

pub use wsp_base::base::{args, git, glyph, labels, procs, seat, term};
pub mod stamp;

/// Whether this process is a test's, so engine records or skips what would spawn, end or tell (wsp-544).
///
/// `cfg(test)` is this crate's own tests only. A dependent's tests build it with `test-support`, and so does
/// every `wsp` binary a workspace test run builds (the root's dev-dependency enables the feature), so the
/// feature alone must not decide: only a cargo test harness, which runs from `target/*/deps`, is one.
pub fn under_test() -> bool {
    #[cfg(test)]
    return true;
    #[cfg(all(not(test), feature = "test-support"))]
    {
        static HARNESS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *HARNESS.get_or_init(|| std::env::current_exe().is_ok_and(|exe| is_harness(&exe)))
    }
    #[cfg(not(any(test, feature = "test-support")))]
    false
}

#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(test, allow(dead_code))]
fn is_harness(exe: &std::path::Path) -> bool {
    exe.parent().and_then(|d| d.file_name()).is_some_and(|d| d == "deps")
}

#[cfg(test)]
mod under_test_tests {
    #[test]
    fn only_a_binary_cargo_left_in_deps_reads_as_a_test_harness() {
        assert!(super::is_harness(&std::env::current_exe().unwrap()));
        assert!(!super::is_harness(std::path::Path::new("/w/target/debug/wsp")));
    }
}
