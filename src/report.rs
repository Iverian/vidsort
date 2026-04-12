use std::fmt;

use tracing_error::SpanTrace;

pub trait SpanTraceExt {
    fn span_trace(&self) -> &SpanTrace;
}

struct Handler {
    span_trace: SpanTrace,
}

impl eyre::EyreHandler for Handler {
    fn debug(
        &self,
        error: &(dyn std::error::Error + 'static),
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "{error}")?;

        let mut source = error.source();
        while let Some(cause) = source {
            write!(f, "\n\nCaused by:\n    {cause}")?;
            source = cause.source();
        }

        Ok(())
    }
}

pub type AnyResult<T> = eyre::Result<T>;

pub fn install_error_hook() {
    eyre::set_hook(Box::new(|_| {
        Box::new(Handler {
            span_trace: SpanTrace::capture(),
        })
    }))
    .expect("eyre hook already installed");
}

impl SpanTraceExt for eyre::Report {
    fn span_trace(&self) -> &SpanTrace {
        let handler = self
            .handler()
            .downcast_ref::<Handler>()
            .expect("incorrect handler type");
        &handler.span_trace
    }
}

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(panic_hook));
}

fn panic_hook(info: &std::panic::PanicHookInfo<'_>) {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown");

    if let Some(loc) = info.location() {
        tracing::error!(
            "panic.message" = message,
            "panic.file" = loc.file(),
            "panic.line" = loc.line(),
            "panic.column" = loc.column(),
            "panic occurred",
        );
    } else {
        tracing::error!("panic.message" = message, "panic occurred");
    }
}
