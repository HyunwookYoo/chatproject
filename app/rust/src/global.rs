//! Process-wide core state. Every entry point in this process (Dart through `api`, Kotlin
//! through `native`) goes through these two statics.

use std::sync::LazyLock;

use chat_core::{CoreSlot, EventBus};

pub(crate) static BUS: LazyLock<EventBus> = LazyLock::new(EventBus::new);
pub(crate) static CORE: LazyLock<CoreSlot> = LazyLock::new(CoreSlot::new);
