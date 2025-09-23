/// Helper macro to log an event within a span that is not currently entered.
macro_rules! span_scoped   {
    ($span:expr, $level:ident!($($arg:tt)*)) => {
        $span.in_scope(|| {
            ::tracing::$level!($($arg)*);
        })
    };
}

/// Helper macro to log a debug event within a span that is not currently
/// entered.
macro_rules! span_debug {
    ($span:expr, $($arg:tt)*) => {
        span_scoped!($span, debug!($($arg)*))
    };
}

/// Helper macro to log an info event within a span that is not currently
/// entered.
macro_rules! span_info {
    ($span:expr, $($arg:tt)*) => {
        span_scoped!($span, info!($($arg)*))
    };

}

/// Helper macro to log a warning event within a span that is not currently
/// entered.
macro_rules! span_error {
    ($span:expr, $($arg:tt)*) => {
        span_scoped!($span, error!($($arg)*))
    };
}

/// Helper macro to unwrap a result or continue the loop with a tracing event.
macro_rules! res_unwrap_or_continue {
    ($result:expr, $span:expr, $level:ident!($($arg:tt)*)) => {
        match $result {
            Ok(value) => value,
            Err(err) => {
                span_scoped!($span, $level!(%err, $($arg)*));
                continue;
            }
        }
    };
}

/// Helper macro to unwrap an option or continue the loop with a tracing event.
macro_rules! opt_unwrap_or_continue {
    ($option:expr, $span:expr, $level:ident!($($arg:tt)*)) => {
        match $option {
            Some(value) => value,
            None => {
                span_scoped!($span, $level!($($arg)*));
                continue;
            }
        }
    };
}
