//! Channels — bounded mpsc-style FIFOs as first-class Values.
//!
//! Tatara-lisp's runtime is single-threaded; channels here are the
//! coordination primitive between cooperative tasks (lazy seqs,
//! generators, future fibers in the bytecode VM). They're intentionally
//! NON-blocking — `>!` returns `#f` on a full channel, `<!` returns
//! `nil` on empty. The blocking-with-yield variants land alongside
//! fiber support.
//!
//! Surface (registered by `install_channels`):
//!
//! ```text
//!   (chan)              → unbounded channel
//!   (chan capacity)     → bounded channel
//!   (chan? v)           → bool
//!   (chan-closed? ch)   → bool
//!   (chan-len ch)       → int (current depth)
//!   (chan-capacity ch)  → int or :unbounded
//!   (>! ch value)       → #t on success, #f if full or closed
//!   (<! ch)             → next value, or :empty if empty, or :closed if drained+closed
//!   (close! ch)         → seals the channel; further `>!` → #f, `<!` drains then returns :closed
//!   (drain! ch)         → list of every remaining value (closes the channel)
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tatara_lisp::Span;

use crate::error::{EvalError, Result};
use crate::eval::Interpreter;
use crate::ffi::Arity;
use crate::value::Value;

/// State of a channel. Capacity `None` means unbounded.
#[derive(Debug)]
pub struct ChannelState {
    pub queue: VecDeque<Value>,
    pub capacity: Option<usize>,
    pub closed: bool,
}

impl ChannelState {
    pub fn new(capacity: Option<usize>) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
            closed: false,
        }
    }
}

/// A handle to a channel. Stored as `Value::Foreign(Arc<Channel>)` so
/// the channel survives `clone` of the wrapping Value (every clone
/// references the same inner queue).
pub struct Channel {
    pub inner: Mutex<ChannelState>,
}

impl Channel {
    pub fn new(capacity: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(ChannelState::new(capacity)),
        })
    }

    /// Try to enqueue. Returns true on success, false if full or closed.
    pub fn try_send(&self, value: Value) -> bool {
        let mut g = self.inner.lock().unwrap();
        if g.closed {
            return false;
        }
        if let Some(cap) = g.capacity {
            if g.queue.len() >= cap {
                return false;
            }
        }
        g.queue.push_back(value);
        true
    }

    /// Try to dequeue. Returns Some(value) if available; None if
    /// empty (whether closed or not — caller distinguishes via
    /// `is_closed`).
    pub fn try_recv(&self) -> Option<Value> {
        let mut g = self.inner.lock().unwrap();
        g.queue.pop_front()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().queue.is_empty()
    }

    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    pub fn capacity(&self) -> Option<usize> {
        self.inner.lock().unwrap().capacity
    }

    pub fn drain(&self) -> Vec<Value> {
        let mut g = self.inner.lock().unwrap();
        g.closed = true;
        g.queue.drain(..).collect()
    }
}

/// Helper: pull a `Value::Foreign(Arc<Channel>)` out of a Value, or
/// raise a TypeMismatch.
fn expect_channel(v: &Value, sp: Span) -> Result<Arc<Channel>> {
    match v {
        Value::Foreign(any) => any
            .clone()
            .downcast::<Channel>()
            .map_err(|_| EvalError::type_mismatch("channel", v.type_name(), sp)),
        other => Err(EvalError::type_mismatch("channel", other.type_name(), sp)),
    }
}

/// Names registered. Sorted for the self-test.
pub const CHANNEL_NAMES: &[&str] = &[
    "<!",
    ">!",
    "chan",
    "chan-capacity",
    "chan-closed?",
    "chan-len",
    "chan?",
    "close!",
    "drain!",
];

pub fn install_channels<H: 'static>(interp: &mut Interpreter<H>) {
    interp.register_fn(
        "chan",
        Arity::Range(0, 1),
        |args: &[Value], _h: &mut H, sp: Span| {
            let capacity = match args.first() {
                None => None,
                Some(Value::Int(n)) if *n >= 0 => Some(*n as usize),
                Some(Value::Int(_)) => {
                    return Err(EvalError::native_fn(
                        Arc::<str>::from("chan"),
                        "negative capacity",
                        sp,
                    ));
                }
                Some(other) => {
                    return Err(EvalError::type_mismatch(
                        "non-negative int",
                        other.type_name(),
                        sp,
                    ));
                }
            };
            Ok(Value::Foreign(Channel::new(capacity)))
        },
    );

    interp.register_fn(
        "chan?",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, _sp| {
            let is = match &args[0] {
                Value::Foreign(any) => any.clone().downcast::<Channel>().is_ok(),
                _ => false,
            };
            Ok(Value::Bool(is))
        },
    );

    interp.register_fn(
        "chan-closed?",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let ch = expect_channel(&args[0], sp)?;
            Ok(Value::Bool(ch.is_closed()))
        },
    );

    interp.register_fn(
        "chan-len",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let ch = expect_channel(&args[0], sp)?;
            Ok(Value::Int(ch.len() as i64))
        },
    );

    interp.register_fn(
        "chan-capacity",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let ch = expect_channel(&args[0], sp)?;
            Ok(match ch.capacity() {
                None => Value::Keyword(Arc::from("unbounded")),
                Some(n) => Value::Int(n as i64),
            })
        },
    );

    interp.register_fn(
        ">!",
        Arity::Exact(2),
        |args: &[Value], _h: &mut H, sp| {
            let ch = expect_channel(&args[0], sp)?;
            Ok(Value::Bool(ch.try_send(args[1].clone())))
        },
    );

    interp.register_fn(
        "<!",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let ch = expect_channel(&args[0], sp)?;
            match ch.try_recv() {
                Some(v) => Ok(v),
                None => {
                    if ch.is_closed() {
                        Ok(Value::Keyword(Arc::from("closed")))
                    } else {
                        Ok(Value::Keyword(Arc::from("empty")))
                    }
                }
            }
        },
    );

    interp.register_fn(
        "close!",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let ch = expect_channel(&args[0], sp)?;
            ch.close();
            Ok(Value::Nil)
        },
    );

    interp.register_fn(
        "drain!",
        Arity::Exact(1),
        |args: &[Value], _h: &mut H, sp| {
            let ch = expect_channel(&args[0], sp)?;
            Ok(Value::list(ch.drain()))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Interpreter;
    use crate::install_full_stdlib_with;
    use tatara_lisp::read_spanned;

    struct NoHost;

    fn run(src: &str) -> Value {
        let mut i: Interpreter<NoHost> = Interpreter::new();
        install_full_stdlib_with(&mut i, &mut NoHost);
        install_channels(&mut i);
        let forms = read_spanned(src).unwrap();
        i.eval_program(&forms, &mut NoHost).unwrap()
    }

    #[test]
    fn unbounded_chan_send_recv_round_trip() {
        let v = run(
            "(define ch (chan))
             (>! ch 1)
             (>! ch 2)
             (>! ch 3)
             (list (<! ch) (<! ch) (<! ch))",
        );
        assert_eq!(format!("{v}"), "(1 2 3)");
    }

    #[test]
    fn fifo_order_preserved() {
        let v = run(
            "(define ch (chan))
             (>! ch :a)
             (>! ch :b)
             (>! ch :c)
             (list (<! ch) (<! ch) (<! ch))",
        );
        assert_eq!(format!("{v}"), "(:a :b :c)");
    }

    #[test]
    fn empty_recv_returns_empty_keyword() {
        let v = run("(<! (chan))");
        assert!(matches!(v, Value::Keyword(s) if &*s == "empty"));
    }

    #[test]
    fn bounded_chan_rejects_overflow() {
        let v = run(
            "(define ch (chan 2))
             (list (>! ch 1) (>! ch 2) (>! ch 3))",
        );
        assert_eq!(format!("{v}"), "(#t #t #f)");
    }

    #[test]
    fn close_blocks_further_sends() {
        let v = run(
            "(define ch (chan))
             (close! ch)
             (>! ch :v)",
        );
        assert!(matches!(v, Value::Bool(false)));
    }

    #[test]
    fn closed_drained_recv_returns_closed_keyword() {
        let v = run(
            "(define ch (chan))
             (>! ch :one)
             (close! ch)
             (list (<! ch) (<! ch))",
        );
        // First recv: :one; second recv: :closed (drained + closed).
        assert_eq!(format!("{v}"), "(:one :closed)");
    }

    #[test]
    fn drain_empties_and_closes() {
        let v = run(
            "(define ch (chan))
             (>! ch 1)
             (>! ch 2)
             (define drained (drain! ch))
             (list drained (chan-closed? ch) (<! ch))",
        );
        assert_eq!(format!("{v}"), "((1 2) #t :closed)");
    }

    #[test]
    fn chan_capacity_introspection() {
        let v = run(
            "(list (chan-capacity (chan)) (chan-capacity (chan 5)))",
        );
        assert_eq!(format!("{v}"), "(:unbounded 5)");
    }

    #[test]
    fn chan_len_tracks_depth() {
        let v = run(
            "(define ch (chan))
             (>! ch 1)
             (>! ch 2)
             (>! ch 3)
             (define before (chan-len ch))
             (<! ch)
             (define after (chan-len ch))
             (list before after)",
        );
        assert_eq!(format!("{v}"), "(3 2)");
    }

    #[test]
    fn chan_predicate_distinguishes() {
        assert!(matches!(run("(chan? (chan))"), Value::Bool(true)));
        assert!(matches!(run("(chan? 42)"), Value::Bool(false)));
        assert!(matches!(run("(chan? (list))"), Value::Bool(false)));
    }
}
