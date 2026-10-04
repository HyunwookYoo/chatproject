//! Commands and events the Flutter UI uses (design 12.1). Keep this file thin:
//! logic lives in chat_core, this module only converts types.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::convert::FrbSink;
use crate::frb_generated::StreamSink;
use crate::global::{BUS, CORE};

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

pub struct CoreInfo {
    pub version: String,
    pub instance_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidConfig,
    AlreadyStartedDifferentConfig,
}

#[derive(Debug)]
pub struct CoreError {
    pub code: ErrorCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    ConnectionState {
        direct_peers: u32,
        mailbox_ok: bool,
        queue_ok: Option<bool>,
    },
    Error {
        code: ErrorCode,
        detail: String,
    },
}

#[flutter_rust_bridge::frb(sync)]
pub fn core_version() -> String {
    chat_core::CORE_VERSION.to_string()
}

/// Starts the core, or returns the running one (see `chat_core::CoreSlot::start`).
pub fn core_start(data_dir: String) -> Result<CoreInfo, CoreError> {
    let config = chat_core::CoreConfig { data_dir: PathBuf::from(data_dir) };
    CORE.start(config, &BUS).map(CoreInfo::from).map_err(CoreError::from)
}

/// The single event stream of the UI (design 12.2 rule 1).
pub fn events(sink: StreamSink<ChatEvent>) -> Result<(), CoreError> {
    BUS.subscribe(Arc::new(FrbSink(Mutex::new(sink))));
    Ok(())
}
