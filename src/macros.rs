/// Helper macro to log an event within a span that is not currently entered.
macro_rules! span_scoped   {
    ($span:expr, $level:ident!($($arg:tt)*)) => {
        $span.in_scope(|| {
            $level!($($arg)*);
        })
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
