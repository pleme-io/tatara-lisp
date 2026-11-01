use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tatara_lisp::{Sexp, Span};
use thiserror::Error;

use super::{Budget, CompiledClosure, Frame, HandlerRecord, Vm};
use crate::env::Env;
use crate::eval::Interpreter;
use crate::value::{ErrorObj, MapKey, NativeFn, Value};
use crate::vm::chunk::{Chunk, CompiledFn, Fingerprint, FnRef};

/// A VM stopped between two steps, as plain data.
///
/// [`Vm::parked`] takes one from a VM that [`Vm::step`] or [`Vm::resume`] left
/// [`Blocked`](super::Progress::Blocked) or [`Yielded`](super::Progress::Yielded);
/// [`Vm::from_parked`] builds a VM from one, in this process or another,
/// against a chunk compiled from the same source. The new VM continues at the
/// same instruction with the same stack, frames, handlers and captures.
///
/// Nothing in it points into the VM that produced it. A value is data, a
/// function is the index of its entry in the chunk ([`FnRef`]), a native
/// function is the name it is registered under, and every capture cell is
/// numbered once in [`cells`](Self::cells), so closures that shared a cell
/// share one cell again after the move. A state with anything else in it (a
/// tree-walker closure, a promise, a host handle, a function from another
/// chunk) is refused by [`Vm::parked`] with a [`ParkError`] that names it.
///
/// The interpreter is not part of it: globals the program defined live there,
/// and the embedder establishes them on the receiving side.
///
/// # Example
///
/// A process parks at a host call on one interpreter and finishes on another:
///
/// ```
/// use std::collections::VecDeque;
/// use std::sync::Arc;
/// use tatara_lisp_eval::vm::{compile_program, Budget, Chunk, Parked, Progress, Vm};
/// use tatara_lisp_eval::{ffi::Arity, install_primitives, read_spanned, Interpreter, Value};
///
/// #[derive(Default)]
/// struct Mail(VecDeque<i64>);
///
/// fn node() -> Interpreter<Mail> {
///     let mut interp = Interpreter::new();
///     install_primitives(&mut interp);
///     interp.register_awaitable_fn(
///         "take",
///         Arity::Exact(0),
///         |_args: &[Value], mail: &Mail| !mail.0.is_empty(),
///         |_args: &[Value], mail: &mut Mail, _span| Ok(Value::Int(mail.0.pop_front().unwrap())),
///     );
///     interp
/// }
///
/// let source = "(let ((x 40)) (+ x (take)))";
/// let compile = || Arc::new(compile_program(&read_spanned(source).unwrap()).unwrap());
///
/// let (chunk, mut here, mut empty) = (compile(), node(), Mail::default());
/// let mut vm = Vm::new();
/// assert!(matches!(vm.step(Arc::clone(&chunk), &mut here, &mut empty), Ok(Progress::Blocked)));
/// let wire = serde_json::to_string(&vm.parked(&chunk).unwrap()).unwrap();
///
/// let parked: Parked = serde_json::from_str(&wire).unwrap();
/// let (chunk, mut there, mut mail) = (compile(), node(), Mail(VecDeque::from([2])));
/// let mut vm = Vm::from_parked(&parked, &chunk, &there, Budget::default()).unwrap();
/// let done = vm.resume(chunk, &mut there, &mut mail).unwrap();
/// assert!(matches!(done, Progress::Done(Value::Int(42))));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parked {
    /// The fingerprint of the chunk the frames index into.
    pub chunk: Fingerprint,
    /// The operand stack, bottom first. A local an inner closure captured is
    /// `Nil` here: the VM reads and writes it through its cell, which carries
    /// it.
    pub stack: Vec<ParkedValue>,
    /// The call frames, outermost first; the first is the program's top level.
    pub frames: Vec<ParkedFrame>,
    /// The contents of every capture cell the state reaches. A cell's number
    /// is its position here.
    pub cells: Vec<ParkedValue>,
}

/// One call frame of a [`Parked`] VM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedFrame {
    /// The function the frame runs.
    pub func: FnRef,
    /// The next instruction to run, an index into that function's opcodes.
    pub ip: usize,
    /// The stack slot of the frame's first local.
    pub locals_base: usize,
    /// The stack slot above the frame's locals.
    pub stack_base: usize,
    /// The cells the frame's closure captured, by number, in capture order.
    pub captures: Vec<usize>,
    /// The frame's locals that inner closures captured: `(local slot, cell)`,
    /// in slot order.
    pub local_cells: Vec<(usize, usize)>,
    /// The frame's active `try` handlers, innermost last.
    pub handlers: Vec<ParkedHandler>,
}

/// One active `try` handler of a [`ParkedFrame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkedHandler {
    /// The instruction the handler starts at.
    pub catch_ip: usize,
    /// The local slot the caught error is stored in.
    pub error_local: usize,
    /// The stack length when the handler was installed.
    pub stack_at_push: usize,
}

/// A [`Value`] as data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParkedValue {
    /// `nil`.
    Nil,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float, as the bits of [`f64::to_bits`], so every float survives
    /// every wire format.
    Float(u64),
    /// A string.
    Str(String),
    /// A symbol.
    Symbol(String),
    /// A keyword, without its `:`.
    Keyword(String),
    /// A list.
    List(Vec<ParkedValue>),
    /// A map, as its `(key, value)` entries.
    Map(Vec<(ParkedValue, ParkedValue)>),
    /// An error value.
    Error {
        /// Its tag.
        tag: String,
        /// Its message.
        message: String,
        /// Its data, as `(key, value)` pairs in order.
        data: Vec<(ParkedValue, ParkedValue)>,
    },
    /// A quoted source form with the span it came from.
    Sexp {
        /// The form.
        form: Sexp,
        /// The span's first byte.
        start: usize,
        /// The byte after the span.
        end: usize,
    },
    /// A native function, by the name it is registered under.
    Native(String),
    /// A VM closure: the `fn_table` entry of its body and its capture cells,
    /// by number.
    Closure {
        /// The index of its body in `fn_table`.
        func: usize,
        /// Its capture cells, by number, in capture order.
        captures: Vec<usize>,
    },
}

/// Why a VM state could not be turned into a [`Parked`], or a [`Parked`] into
/// a VM.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ParkError {
    /// The VM holds no frames: it has not started, or it returned from its top
    /// level.
    #[error("nothing to park: the VM holds no frames")]
    NoFrames,
    /// A value with no data form: a tree-walker closure, a promise or a host
    /// handle.
    #[error("cannot park {what} in {at}: it has no data form")]
    NoDataForm {
        /// What the value is.
        what: &'static str,
        /// Where the state holds it.
        at: String,
    },
    /// A frame or closure compiled into a chunk other than the one parked.
    #[error("cannot park {what} in {at}: it was compiled into another chunk")]
    OtherChunk {
        /// What it is.
        what: &'static str,
        /// Where the state holds it.
        at: String,
    },
    /// The chunk given to [`Vm::from_parked`] was not compiled from the source
    /// the state was parked in.
    #[error("the parked state belongs to chunk {parked}, not to this chunk {chunk}")]
    ChunkMismatch {
        /// The fingerprint the state records.
        parked: Fingerprint,
        /// The fingerprint of the chunk given.
        chunk: Fingerprint,
    },
    /// A native function the receiving interpreter does not register.
    #[error("native function `{name}` is not registered in this interpreter")]
    UnknownNative {
        /// The function's name.
        name: String,
    },
    /// Parked data no VM running this chunk could have produced.
    #[error("malformed parked state: {0}")]
    Malformed(String),
}

impl Vm {
    /// This VM's state as data, its functions indexed into `chunk`.
    ///
    /// `chunk` is the chunk the VM was started with, as passed to
    /// [`Vm::step`]. Call it between steps, after [`Vm::step`] or
    /// [`Vm::resume`] returned [`Blocked`](super::Progress::Blocked) or
    /// [`Yielded`](super::Progress::Yielded). The VM is not changed. See
    /// [`Parked`] for what it holds and an example.
    ///
    /// # Errors
    ///
    /// [`ParkError::NoFrames`] for a VM with nothing to carry,
    /// [`ParkError::NoDataForm`] for a tree-walker closure, a promise or a host
    /// handle anywhere in the state, and [`ParkError::OtherChunk`] for a frame
    /// or closure compiled into a chunk with another fingerprint. Each names
    /// where the state holds it.
    pub fn parked(&self, chunk: &Chunk) -> Result<Parked, ParkError> {
        if !self.started || self.frames.is_empty() {
            return Err(ParkError::NoFrames);
        }
        let mut encoder = Encoder {
            chunk,
            fingerprint: chunk.fingerprint(),
            verdicts: Vec::new(),
            numbered: HashMap::new(),
            cells: Vec::new(),
        };
        let celled: std::collections::HashSet<usize> = self
            .frames
            .iter()
            .flat_map(|frame| {
                frame
                    .local_cells
                    .keys()
                    .map(|slot| frame.locals_base + slot)
            })
            .collect();
        let mut stack = Vec::with_capacity(self.stack.len());
        for (slot, value) in self.stack.iter().enumerate() {
            stack.push(if celled.contains(&slot) {
                ParkedValue::Nil
            } else {
                encoder.value(value, &|| format!("stack slot {slot}"))?
            });
        }
        let mut frames = Vec::with_capacity(self.frames.len());
        for (index, frame) in self.frames.iter().enumerate() {
            frames.push(encoder.frame(index, frame)?);
        }
        Ok(Parked {
            chunk: encoder.fingerprint,
            stack,
            frames,
            cells: encoder.cells,
        })
    }

    /// A VM that continues `parked` against `chunk`, under `budget`.
    ///
    /// `chunk` is compiled from the source the state was parked in; this
    /// process's own copy or one recompiled on another node both work. Native
    /// functions resolve by name in `interp`, and closures the state holds see
    /// `interp`'s globals. The new VM has spent nothing of `budget`. Drive it
    /// with [`Vm::resume`], passing the same `chunk`. See [`Parked`] for an
    /// example.
    ///
    /// # Errors
    ///
    /// [`ParkError::ChunkMismatch`] when `chunk` was compiled from other source,
    /// [`ParkError::UnknownNative`] for a native function `interp` does not
    /// register, and [`ParkError::Malformed`] for an index the chunk or the
    /// state does not have. Nothing is read past a refusal.
    pub fn from_parked<H: 'static>(
        parked: &Parked,
        chunk: &Arc<Chunk>,
        interp: &Interpreter<H>,
        budget: Budget,
    ) -> Result<Self, ParkError> {
        let found = chunk.fingerprint();
        if found != parked.chunk {
            return Err(ParkError::ChunkMismatch {
                parked: parked.chunk,
                chunk: found,
            });
        }
        if parked.frames.first().map(|frame| frame.func) != Some(FnRef::Top) {
            return Err(ParkError::Malformed(
                "the first frame is not the top level".to_string(),
            ));
        }
        let mut decoder = Decoder {
            chunk,
            interp,
            globals: interp.globals_snapshot().clone(),
            cells: (0..parked.cells.len())
                .map(|_| Arc::new(Mutex::new(Value::Nil)))
                .collect(),
            funcs: HashMap::new(),
        };
        for (number, held) in parked.cells.iter().enumerate() {
            let value = decoder.value(held)?;
            *decoder.cells[number].lock().unwrap() = value;
        }
        let stack = parked
            .stack
            .iter()
            .map(|value| decoder.value(value))
            .collect::<Result<Vec<_>, _>>()?;
        let mut frames = Vec::with_capacity(parked.frames.len());
        for (index, frame) in parked.frames.iter().enumerate() {
            frames.push(decoder.frame(index, frame, stack.len())?);
        }
        Ok(Self {
            stack,
            frames,
            budget,
            burned: 0,
            slice: 0,
            started: true,
            blocked: false,
        })
    }
}

fn no_data_form(what: &'static str, at: &dyn Fn() -> String) -> ParkError {
    ParkError::NoDataForm { what, at: at() }
}

struct Encoder<'a> {
    chunk: &'a Chunk,
    fingerprint: Fingerprint,
    verdicts: Vec<(*const Chunk, bool)>,
    numbered: HashMap<*const Mutex<Value>, usize>,
    cells: Vec<ParkedValue>,
}

impl Encoder<'_> {
    fn owns(&mut self, chunk: &Arc<Chunk>) -> bool {
        let ptr = Arc::as_ptr(chunk);
        if std::ptr::eq(ptr, self.chunk) {
            return true;
        }
        if let Some(&(_, same)) = self.verdicts.iter().find(|(seen, _)| *seen == ptr) {
            return same;
        }
        let same = chunk.fingerprint() == self.fingerprint;
        self.verdicts.push((ptr, same));
        same
    }

    fn cell(
        &mut self,
        cell: &Arc<Mutex<Value>>,
        at: &dyn Fn() -> String,
    ) -> Result<usize, ParkError> {
        let ptr = Arc::as_ptr(cell);
        if let Some(&number) = self.numbered.get(&ptr) {
            return Ok(number);
        }
        let number = self.cells.len();
        self.numbered.insert(ptr, number);
        self.cells.push(ParkedValue::Nil);
        let held = cell.lock().unwrap().clone();
        self.cells[number] = self.value(&held, at)?;
        Ok(number)
    }

    fn pairs<'v>(
        &mut self,
        pairs: impl Iterator<Item = (Value, &'v Value)>,
        at: &dyn Fn() -> String,
    ) -> Result<Vec<(ParkedValue, ParkedValue)>, ParkError> {
        pairs
            .map(|(key, value)| Ok((self.value(&key, at)?, self.value(value, at)?)))
            .collect()
    }

    fn value(&mut self, value: &Value, at: &dyn Fn() -> String) -> Result<ParkedValue, ParkError> {
        Ok(match value {
            Value::Nil => ParkedValue::Nil,
            Value::Bool(b) => ParkedValue::Bool(*b),
            Value::Int(n) => ParkedValue::Int(*n),
            Value::Float(x) => ParkedValue::Float(x.to_bits()),
            Value::Str(s) => ParkedValue::Str(s.to_string()),
            Value::Symbol(s) => ParkedValue::Symbol(s.to_string()),
            Value::Keyword(s) => ParkedValue::Keyword(s.to_string()),
            Value::List(items) => ParkedValue::List(
                items
                    .iter()
                    .map(|item| self.value(item, at))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Map(map) => ParkedValue::Map(
                self.pairs(map.iter().map(|(key, value)| (key.to_value(), value)), at)?,
            ),
            Value::Error(error) => ParkedValue::Error {
                tag: error.tag.to_string(),
                message: error.message.to_string(),
                data: self.pairs(
                    error.data.iter().map(|(key, value)| (key.clone(), value)),
                    at,
                )?,
            },
            Value::Sexp(form, span) => ParkedValue::Sexp {
                form: form.clone(),
                start: span.start,
                end: span.end,
            },
            Value::NativeFn(native) => ParkedValue::Native(native.name.to_string()),
            Value::Foreign(any) => match any.downcast_ref::<CompiledClosure>() {
                Some(closure) => self.closure(closure, at)?,
                None => return Err(no_data_form("a host handle", at)),
            },
            Value::Closure(_) => return Err(no_data_form("a tree-walker closure", at)),
            Value::Promise(_) => return Err(no_data_form("a promise", at)),
        })
    }

    fn closure(
        &mut self,
        closure: &CompiledClosure,
        at: &dyn Fn() -> String,
    ) -> Result<ParkedValue, ParkError> {
        if !self.owns(&closure.chunk) {
            return Err(ParkError::OtherChunk {
                what: "a closure",
                at: at(),
            });
        }
        let mut captures = Vec::with_capacity(closure.captures.len());
        for ((name, _), cell) in closure.body.captures.iter().zip(&closure.captures) {
            captures.push(self.cell(cell, &|| {
                format!("capture `{name}` of a closure in {}", at())
            })?);
        }
        Ok(ParkedValue::Closure {
            func: closure.fn_index,
            captures,
        })
    }

    fn frame(&mut self, index: usize, frame: &Frame) -> Result<ParkedFrame, ParkError> {
        if !self.owns(&frame.chunk) {
            return Err(ParkError::OtherChunk {
                what: "a frame",
                at: format!("frame {index}"),
            });
        }
        let mut captures = Vec::with_capacity(frame.captures.len());
        for ((name, _), cell) in frame.func.captures.iter().zip(&frame.captures) {
            captures.push(self.cell(cell, &|| format!("capture `{name}` of frame {index}"))?);
        }
        let mut slots: Vec<&usize> = frame.local_cells.keys().collect();
        slots.sort_unstable();
        let mut local_cells = Vec::with_capacity(slots.len());
        for slot in slots {
            let number = self.cell(&frame.local_cells[slot], &|| {
                format!("local {slot} of frame {index}")
            })?;
            local_cells.push((*slot, number));
        }
        Ok(ParkedFrame {
            func: frame.func_ref,
            ip: frame.ip,
            locals_base: frame.locals_base,
            stack_base: frame.stack_base,
            captures,
            local_cells,
            handlers: frame
                .handlers
                .iter()
                .map(|handler| ParkedHandler {
                    catch_ip: handler.catch_ip,
                    error_local: handler.error_local,
                    stack_at_push: handler.stack_at_push,
                })
                .collect(),
        })
    }
}

struct Decoder<'a, H> {
    chunk: &'a Arc<Chunk>,
    interp: &'a Interpreter<H>,
    globals: Env,
    cells: Vec<Arc<Mutex<Value>>>,
    funcs: HashMap<FnRef, Arc<CompiledFn>>,
}

impl<H: 'static> Decoder<'_, H> {
    fn func(&mut self, func: FnRef) -> Result<Arc<CompiledFn>, ParkError> {
        if let Some(body) = self.funcs.get(&func) {
            return Ok(Arc::clone(body));
        }
        let body = Arc::new(
            func.resolve(self.chunk)
                .ok_or_else(|| {
                    ParkError::Malformed(format!("this chunk has no function {func:?}"))
                })?
                .clone(),
        );
        self.funcs.insert(func, Arc::clone(&body));
        Ok(body)
    }

    fn cell(&self, number: usize) -> Result<Arc<Mutex<Value>>, ParkError> {
        self.cells
            .get(number)
            .cloned()
            .ok_or_else(|| ParkError::Malformed(format!("there is no cell {number}")))
    }

    fn cells_of(
        &self,
        numbers: &[usize],
        wanted: usize,
        what: &str,
    ) -> Result<Vec<Arc<Mutex<Value>>>, ParkError> {
        if numbers.len() != wanted {
            return Err(ParkError::Malformed(format!(
                "{what} has {} captures, its function {wanted}",
                numbers.len()
            )));
        }
        numbers.iter().map(|number| self.cell(*number)).collect()
    }

    fn pairs(
        &mut self,
        pairs: &[(ParkedValue, ParkedValue)],
    ) -> Result<Vec<(Value, Value)>, ParkError> {
        pairs
            .iter()
            .map(|(key, value)| Ok((self.value(key)?, self.value(value)?)))
            .collect()
    }

    fn value(&mut self, value: &ParkedValue) -> Result<Value, ParkError> {
        Ok(match value {
            ParkedValue::Nil => Value::Nil,
            ParkedValue::Bool(b) => Value::Bool(*b),
            ParkedValue::Int(n) => Value::Int(*n),
            ParkedValue::Float(bits) => Value::Float(f64::from_bits(*bits)),
            ParkedValue::Str(s) => Value::Str(Arc::from(s.as_str())),
            ParkedValue::Symbol(s) => Value::Symbol(crate::interner::intern(s)),
            ParkedValue::Keyword(s) => Value::Keyword(crate::interner::intern(s)),
            ParkedValue::List(items) => Value::list(
                items
                    .iter()
                    .map(|item| self.value(item))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ParkedValue::Map(entries) => {
                let mut map = Vec::with_capacity(entries.len());
                for (key, value) in self.pairs(entries)? {
                    let key = MapKey::from_value(&key).ok_or_else(|| {
                        ParkError::Malformed(format!(
                            "a map key of type {} cannot be hashed",
                            key.type_name()
                        ))
                    })?;
                    map.push((key, value));
                }
                Value::map(map)
            }
            ParkedValue::Error { tag, message, data } => Value::Error(Arc::new(ErrorObj {
                tag: Arc::from(tag.as_str()),
                message: Arc::from(message.as_str()),
                data: self.pairs(data)?,
            })),
            ParkedValue::Sexp { form, start, end } => {
                Value::Sexp(form.clone(), Span::new(*start, *end))
            }
            ParkedValue::Native(name) => {
                let entry = self
                    .interp
                    .registry
                    .lookup(name)
                    .ok_or_else(|| ParkError::UnknownNative { name: name.clone() })?;
                Value::NativeFn(Arc::new(NativeFn {
                    name: Arc::clone(&entry.name),
                    arity: entry.arity,
                }))
            }
            ParkedValue::Closure { func, captures } => {
                let body = self.func(FnRef::Table(*func))?;
                let captures = self.cells_of(captures, body.captures.len(), "a closure")?;
                Value::Foreign(Arc::new(CompiledClosure {
                    body,
                    captures,
                    chunk: Arc::clone(self.chunk),
                    globals: self.globals.clone(),
                    fn_index: *func,
                }))
            }
        })
    }

    fn frame(
        &mut self,
        index: usize,
        frame: &ParkedFrame,
        stack_len: usize,
    ) -> Result<Frame, ParkError> {
        let func = self.func(frame.func)?;
        if frame.ip > func.ops.len()
            || frame.locals_base > frame.stack_base
            || frame.stack_base > stack_len
        {
            return Err(ParkError::Malformed(format!(
                "frame {index} lies outside its function or the stack"
            )));
        }
        let captures = self.cells_of(
            &frame.captures,
            func.captures.len(),
            &format!("frame {index}"),
        )?;
        let mut local_cells = HashMap::with_capacity(frame.local_cells.len());
        for (slot, number) in &frame.local_cells {
            local_cells.insert(*slot, self.cell(*number)?);
        }
        Ok(Frame {
            func,
            func_ref: frame.func,
            chunk: Arc::clone(self.chunk),
            ip: frame.ip,
            locals_base: frame.locals_base,
            stack_base: frame.stack_base,
            captures,
            local_cells,
            handlers: frame
                .handlers
                .iter()
                .map(|handler| HandlerRecord {
                    catch_ip: handler.catch_ip,
                    error_local: handler.error_local,
                    stack_at_push: handler.stack_at_push,
                })
                .collect(),
        })
    }
}
