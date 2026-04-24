//! Time + duration helpers.
//!
//!   (now)                    → integer — unix seconds
//!   (now-ms)                 → integer — unix milliseconds
//!   (now-ns)                 → integer — unix nanoseconds (monotonic)
//!   (now-rfc3339)            → string — current time formatted RFC 3339 UTC
//!   (sleep SECONDS)          → nil — blocks the script
//!   (sleep-ms MILLIS)        → nil
//!   (elapsed-since START-NS) → integer — ns since START-NS (from (now-ns))

use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::sync::Arc;
use tatara_lisp_eval::{Arity, Interpreter, Value};

use crate::script_ctx::ScriptCtx;
use crate::stdlib::env::int_arg;

pub fn install(interp: &mut Interpreter<ScriptCtx>) {
    interp.register_fn(
        "now",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            Ok(Value::Int(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            ))
        },
    );

    interp.register_fn(
        "now-ms",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            Ok(Value::Int(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            ))
        },
    );

    interp.register_fn(
        "now-ns",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            Ok(Value::Int(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0),
            ))
        },
    );

    interp.register_fn(
        "now-rfc3339",
        Arity::Exact(0),
        |_args: &[Value], _ctx: &mut ScriptCtx, _sp| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Ok(Value::Str(Arc::from(format_rfc3339_utc(now))))
        },
    );

    interp.register_fn(
        "sleep",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let secs = int_arg(&args[0], "sleep", sp)?;
            if secs > 0 {
                thread::sleep(Duration::from_secs(secs as u64));
            }
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "sleep-ms",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let ms = int_arg(&args[0], "sleep-ms", sp)?;
            if ms > 0 {
                thread::sleep(Duration::from_millis(ms as u64));
            }
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "elapsed-since",
        Arity::Exact(1),
        |args: &[Value], _ctx: &mut ScriptCtx, sp| {
            let start_ns = int_arg(&args[0], "elapsed-since", sp)?;
            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            Ok(Value::Int(now_ns - start_ns))
        },
    );
}

/// Format a unix-seconds timestamp as RFC-3339 UTC (no external crate).
/// Good enough for log lines + filenames; NOT timezone-aware.
fn format_rfc3339_utc(unix_secs: i64) -> String {
    // Civil calendar math — days since 1970-01-01.
    let (mut y, mut m, mut d, h, mi, s) = seconds_to_datetime(unix_secs);
    if d == 0 {
        d = 1;
    }
    if m == 0 {
        m = 1;
    }
    if y == 0 {
        y = 1970;
    }
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn seconds_to_datetime(unix_secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix_secs / 86_400;
    let rem = unix_secs.rem_euclid(86_400);
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;
    let (y, m, d) = days_to_ymd(days);
    (y, m, d, h, mi, s)
}

/// Days since 1970-01-01 → (year, month, day).
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant "date algorithm" civil-from-days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // 0..=146_096
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_epoch() {
        assert_eq!(format_rfc3339_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_y2k() {
        // 946684800 = 2000-01-01T00:00:00Z
        assert_eq!(format_rfc3339_utc(946_684_800), "2000-01-01T00:00:00Z");
    }
}
