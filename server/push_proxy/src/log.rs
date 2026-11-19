//! The only log the proxy writes (spec 9.4): time, path, result codes, item
//! count, elapsed ms. One line per request and one per upstream send. Never a
//! token, handle, tag, payload or client IP: nothing here takes one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use push_wire::PushResult;

use crate::upstream::UpstreamResult;

/// Where log lines go. One call per line, without the newline.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

pub fn stderr_sink() -> LogSink {
    Arc::new(|line| eprintln!("{line}"))
}

pub(crate) struct Logger {
    pub(crate) clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub(crate) sink: LogSink,
}

/// Result codes of one `/push`, attached to its response for the request line.
#[derive(Clone, Copy, Default)]
pub(crate) struct PushCounts {
    items: usize,
    ok: usize,
    gone: usize,
    limited: usize,
    bad: usize,
}

impl PushCounts {
    pub(crate) fn refused(items: usize) -> Self {
        PushCounts {
            items,
            ..Self::default()
        }
    }

    pub(crate) fn of(results: &[PushResult]) -> Self {
        let mut c = Self::refused(results.len());
        for r in results {
            match r {
                PushResult::Ok => c.ok += 1,
                PushResult::Gone => c.gone += 1,
                PushResult::Limited => c.limited += 1,
                PushResult::Bad => c.bad += 1,
            }
        }
        c
    }
}

/// Only the routes the proxy serves are named; any other path could carry
/// whatever a client put in it.
fn path_label(path: &str) -> &'static str {
    match path {
        "/handle" => "/handle",
        "/push" => "/push",
        _ => "other",
    }
}

impl Logger {
    fn write(&self, line: &str) {
        (self.sink)(line);
    }

    pub(crate) fn sent(&self, result: UpstreamResult, elapsed: Duration) {
        let code = match result {
            UpstreamResult::Sent => "sent",
            UpstreamResult::TokenGone => "token_gone",
            UpstreamResult::Failed => "failed",
        };
        self.write(&format!(
            "t={} path=upstream result={code} items=1 ms={}",
            (self.clock)(),
            elapsed.as_millis()
        ));
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
    if let Some(c) = response.extensions().get::<PushCounts>() {
        line.push_str(&format!(
            " items={} ok={} gone={} limited={} bad={}",
            c.items, c.ok, c.gone, c.limited, c.bad
        ));
    }
    line.push_str(&format!(" ms={}", start.elapsed().as_millis()));
    logger.write(&line);
    response
}
