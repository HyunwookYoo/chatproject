pub mod api;
#[cfg(target_os = "android")]
mod android_jni;
mod convert;
mod frb_generated;
mod global;
mod native;

uniffi::setup_scaffolding!();
