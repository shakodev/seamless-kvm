pub mod config;
pub mod input;
pub mod keyboard;
pub mod session;

// This feature type-checks native code on Windows without linking frameworks;
// it never substitutes for a build and desktop validation on macOS.
#[cfg(any(target_os = "macos", feature = "native-check"))]
pub mod app;
#[cfg(any(target_os = "macos", feature = "native-check"))]
pub mod native;
