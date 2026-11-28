//! The only log the server writes (spec 9.5): time, path, result codes,
//! command count, elapsed ms. One line per request and one per sweep. Never a
//! queue id, key, signature, ciphertext, size or client IP: nothing here
//! takes one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use queue_wire::{CommandName, ItemResult};

use crate::app::Clock;
use crate::store::{StoreError, SweepCounts};

/// Where log lines go. One call per line, without the newline.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

pub fn stderr_sink() -> LogSink {
    Arc::new(|line| eprintln!("{line}"))
}

pub(crate) struct Logger {
    pub(crate) clock: Clock,
    pub(crate) sink: LogSink,
}

/// Result codes of one request, attached to its response for the request line.
#[derive(Clone, Copy, Default)]
pub(crate) struct ResultCounts {
    items: usize,
    ok: usize,
    exists: usize,
    gone: usize,
    full: usize,
    limited: usize,
    bad: usize,
    entries: usize,
}

impl ResultCounts {
    pub(crate) fn refused(items: usize) -> Self {
        ResultCounts {
            items,
            ..Self::default()
        }
    }

    pub(crate) fn of(results: &[ItemResult]) -> Self {
        let mut c = Self::refused(results.len());
        for r in results {
            match r {
                ItemResult::Ok => c.ok += 1,
                ItemResult::Exists => c.exists += 1,
                ItemResult::Gone => c.gone += 1,
                ItemResult::Full => c.full += 1,
                ItemResult::Limited => c.limited += 1,
                ItemResult::Bad => c.bad += 1,
                ItemResult::Entries { .. } => c.entries += 1,
            }
        }
        c
    }
}

/// Attached to a 500 response: which store failure, as a code.
#[derive(Clone, Copy)]
pub(crate) struct StoreFailed(pub(crate) StoreError);

/// Only the routes the server serves are named; any other path could carry
/// whatever a client put in it.
fn path_label(path: &str) -> &'static str {
    [
        CommandName::Create,
        CommandName::Send,
        CommandName::Recv,
        CommandName::Ack,
        CommandName::Delete,
    ]
    .into_iter()
    .map(CommandName::path)
    .find(|p| *p == path)
    .unwrap_or("other")
}

impl Logger {
    fn write(&self, line: &str) {
        (self.sink)(line);
    }

    pub(crate) fn swept(&self, result: Result<SweepCounts, StoreError>, elapsed: Duration) {
        let t = (self.clock)();
        let ms = elapsed.as_millis();
        self.write(&match result {
            Ok(c) => format!(
                "t={t} path=sweep result=ok entries={} acked={} queues={} ms={ms}",
                c.entries, c.acked, c.queues
            ),
            Err(e) => format!("t={t} path=sweep result=error error={e} ms={ms}"),
        });
    }
}

/// Middleware: one line per request, after the response is built.
pub(crate) async fn log_request(
    State(logger): State<Arc<Logger>>,
    request: Request,
    next: Next,
) -> Response {
    let start = Instant::now();
    let path = path_label(request.uri().path());
    let response = next.run(request).await;
    let mut line = format!(
        "t={} path={path} status={}",
        (logger.clock)(),
        response.status().as_u16()
    );
    if let Some(c) = response.extensions().get::<ResultCounts>() {
        line.push_str(&format!(
            " items={} ok={} exists={} gone={} full={} limited={} bad={} entries={}",
            c.items, c.ok, c.exists, c.gone, c.full, c.limited, c.bad, c.entries
        ));
    }
    if let Some(StoreFailed(e)) = response.extensions().get::<StoreFailed>() {
        line.push_str(&format!(" error={e}"));
    }
    line.push_str(&format!(" ms={}", start.elapsed().as_millis()));
    logger.write(&line);
    response
}
