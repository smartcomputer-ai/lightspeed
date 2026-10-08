use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc as sync_mpsc,
    },
    time::{Duration, Instant},
};

use rquickjs::{Context, Ctx, Exception, Function, Object, Promise, Runtime, Value as JsValue};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::*;

const PRELUDE: &str = include_str!("prelude.js");
const IDLE_POLL: Duration = Duration::from_millis(5);

#[derive(Default)]
struct State {
    output: Vec<Value>,
    selections: Vec<OutputSelection>,
    output_bytes: u64,
    pending: BTreeSet<String>,
    helper_calls: BTreeMap<String, HelperCall>,
    failure: Option<ExecutionError>,
    calls: u32,
    jobs: u64,
    startup_micros: u64,
}

#[derive(Clone, Copy)]
enum HelperKind {
    Media,
    File,
}

struct HelperCall {
    kind: HelperKind,
    /// Present only after the actual host completion succeeded. The guest
    /// cannot lower this count by replacing or editing its returned descriptor.
    result_bytes: Option<u64>,
}

pub(super) fn start(
    input: ExecutionInput,
    cancellation: Cancellation,
) -> Result<Execution, StartError> {
    validate(&input)?;
    let capacity = input.limits.max_outstanding_tool_calls as usize;
    let stack_size = (input.limits.max_stack_bytes as usize)
        .checked_add(512 * 1024)
        .ok_or_else(|| StartError::InvalidInput("stack budget overflows native size".into()))?
        .max(2 * 1024 * 1024);
    // One extra event slot ensures terminal reporting never blocks behind all
    // outstanding requests when the host is not currently polling.
    let (event_tx, event_rx) = mpsc::channel(capacity + 1);
    let (completion_tx, completion_rx) = sync_mpsc::sync_channel(capacity);
    let completions = CompletionSender {
        sender: completion_tx,
        max_result_bytes: input.limits.max_result_bytes,
    };
    let thread_cancel = cancellation.clone();
    std::thread::Builder::new()
        .name("codemode".into())
        .stack_size(stack_size)
        .spawn(move || {
            let report = run(input, thread_cancel, &event_tx, completion_rx);
            let _ = event_tx.try_send(ExecutionEvent::Finished(report));
        })?;
    Ok(Execution {
        events: event_rx,
        completions,
        cancellation,
    })
}

fn validate(input: &ExecutionInput) -> Result<(), StartError> {
    let limits = &input.limits;
    for (name, value) in [
        ("timeout_ms", limits.timeout_ms),
        ("max_memory_bytes", limits.max_memory_bytes),
        ("max_stack_bytes", limits.max_stack_bytes),
        ("max_source_bytes", limits.max_source_bytes),
        ("max_catalog_bytes", limits.max_catalog_bytes),
        ("max_request_bytes", limits.max_request_bytes),
        ("max_result_bytes", limits.max_result_bytes),
        ("max_output_bytes", limits.max_output_bytes),
        ("max_tool_calls", u64::from(limits.max_tool_calls)),
        (
            "max_outstanding_tool_calls",
            u64::from(limits.max_outstanding_tool_calls),
        ),
    ] {
        if value == 0 || usize::try_from(value).is_err() {
            return Err(StartError::InvalidInput(format!(
                "{name} must be positive and fit native size"
            )));
        }
    }
    if limits.max_outstanding_tool_calls > limits.max_tool_calls {
        return Err(StartError::InvalidInput(
            "outstanding budget exceeds total call budget".into(),
        ));
    }
    if input.source.len() as u64 > limits.max_source_bytes {
        return Err(StartError::InvalidInput(
            "source exceeds source byte limit".into(),
        ));
    }
    if serialized_len(&input.bindings) > limits.max_catalog_bytes {
        return Err(StartError::InvalidInput(
            "bindings exceed catalog byte limit".into(),
        ));
    }
    let mut names = HashSet::new();
    let mut ids = HashSet::new();
    for binding in &input.bindings {
        if binding.name.is_empty()
            || binding.binding_id.is_empty()
            || !names.insert(&binding.name)
            || !ids.insert(&binding.binding_id)
        {
            return Err(StartError::InvalidInput(
                "bindings need unique nonempty names and identities".into(),
            ));
        }
    }
    if Instant::now()
        .checked_add(Duration::from_millis(limits.timeout_ms))
        .is_none()
    {
        return Err(StartError::InvalidInput(
            "timeout exceeds native duration".into(),
        ));
    }
    Ok(())
}

fn error(kind: ExecutionErrorKind, message: impl Into<String>) -> ExecutionError {
    ExecutionError {
        kind,
        message: message.into(),
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn stop_reason(cancellation: &Cancellation, deadline: Instant) -> Option<ExecutionError> {
    if cancellation.is_cancelled() {
        Some(error(
            ExecutionErrorKind::Cancelled,
            "JavaScript execution was cancelled",
        ))
    } else if Instant::now() >= deadline {
        Some(error(
            ExecutionErrorKind::TimedOut,
            "JavaScript execution exceeded its wall-clock limit",
        ))
    } else {
        None
    }
}

fn run(
    input: ExecutionInput,
    cancellation: Cancellation,
    events: &mpsc::Sender<ExecutionEvent>,
    completions: sync_mpsc::Receiver<HostCompletion>,
) -> ExecutionReport {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(input.limits.timeout_ms);
    let state = Rc::new(RefCell::new(State::default()));
    let fatal = Arc::new(AtomicBool::new(false));
    let result = (|| {
        if let Some(reason) = stop_reason(&cancellation, deadline) {
            return Err(reason);
        }
        let runtime = Runtime::new().map_err(native_error)?;
        runtime.set_memory_limit(input.limits.max_memory_bytes as usize);
        runtime.set_max_stack_size(input.limits.max_stack_bytes as usize);
        let interrupt_cancel = cancellation.clone();
        let interrupt_fatal = fatal.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            interrupt_cancel.is_cancelled()
                || Instant::now() >= deadline
                || interrupt_fatal.load(Ordering::Acquire)
        })));
        let context = Context::full(&runtime).map_err(native_error)?;
        context.with(|ctx| {
            run_context(
                ctx,
                &input,
                state.clone(),
                fatal,
                &cancellation,
                deadline,
                started,
                events,
                &completions,
            )
        })
    })();
    let mut state = state.borrow_mut();
    let (return_value, execution_error) = match result {
        Ok(value) => (value, None),
        Err(failure) => (None, Some(failure)),
    };
    // Resource limits and cancellation remain terminal even if guest code
    // catches the exception raised at an interrupt or host-callback boundary.
    let execution_error = stop_reason(&cancellation, deadline)
        .or_else(|| state.failure.take())
        .or(execution_error);
    ExecutionReport {
        output: std::mem::take(&mut state.output),
        selections: std::mem::take(&mut state.selections),
        return_value,
        error: execution_error,
        pending_request_ids: state.pending.iter().cloned().collect(),
        metrics: ExecutionMetrics {
            startup_micros: state.startup_micros,
            elapsed_micros: elapsed_micros(started),
            tool_calls: state.calls,
            pending_jobs: state.jobs,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn run_context<'js>(
    ctx: Ctx<'js>,
    input: &ExecutionInput,
    state: Rc<RefCell<State>>,
    fatal: Arc<AtomicBool>,
    cancellation: &Cancellation,
    deadline: Instant,
    started: Instant,
    events: &mpsc::Sender<ExecutionEvent>,
    completions: &sync_mpsc::Receiver<HostCompletion>,
) -> Result<Option<Value>, ExecutionError> {
    let send_state = state.clone();
    let send_fatal = fatal.clone();
    let send_events = events.clone();
    let send_limits = input.limits.clone();
    let send_cancel = cancellation.clone();
    let allowed: HashMap<_, _> = input
        .bindings
        .iter()
        .map(|binding| (binding.binding_id.clone(), binding.name.clone()))
        .collect();
    let send = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>,
              binding_id: String,
              json: String,
              selection: String|
              -> rquickjs::Result<String> {
            let mut state = send_state.borrow_mut();
            let failure = if let Some(reason) = stop_reason(&send_cancel, deadline) {
                Some(reason)
            } else if !allowed.contains_key(&binding_id) {
                Some(error(ExecutionErrorKind::Internal, "unrecognized binding"))
            } else if json.len() as u64 > send_limits.max_request_bytes {
                Some(error(
                    ExecutionErrorKind::LimitExceeded,
                    "tool arguments exceed request byte limit",
                ))
            } else if state.calls >= send_limits.max_tool_calls {
                Some(error(
                    ExecutionErrorKind::LimitExceeded,
                    "tool call budget exhausted",
                ))
            } else if state.pending.len() >= send_limits.max_outstanding_tool_calls as usize {
                Some(error(
                    ExecutionErrorKind::LimitExceeded,
                    "outstanding tool call budget exhausted",
                ))
            } else {
                None
            };
            if let Some(failure) = failure {
                return Err(fail_callback(&ctx, &mut state, &send_fatal, failure));
            }
            let arguments: Value = serde_json::from_str(&json).map_err(|_| {
                fail_callback(
                    &ctx,
                    &mut state,
                    &send_fatal,
                    error(ExecutionErrorKind::Internal, "invalid bridge JSON"),
                )
            })?;
            let helper_kind = match selection.as_str() {
                "" => None,
                "media"
                    if allowed.get(&binding_id).map(String::as_str) == Some("blob_read")
                        && arguments.get("format").and_then(Value::as_str) == Some("media") =>
                {
                    Some(HelperKind::Media)
                }
                "file"
                    if allowed.get(&binding_id).map(String::as_str) == Some("blob_info")
                        && arguments.get("presentation").and_then(Value::as_str)
                            == Some("file") =>
                {
                    Some(HelperKind::File)
                }
                _ => {
                    return Err(fail_callback(
                        &ctx,
                        &mut state,
                        &send_fatal,
                        error(
                            ExecutionErrorKind::Internal,
                            "invalid output admission request",
                        ),
                    ));
                }
            };
            let request_id = format!("call-{}", state.calls + 1);
            let request = HostRequest {
                request_id: request_id.clone(),
                binding_id,
                arguments,
            };
            if send_events
                .try_send(ExecutionEvent::Request(request))
                .is_err()
            {
                return Err(fail_callback(
                    &ctx,
                    &mut state,
                    &send_fatal,
                    error(
                        ExecutionErrorKind::HostDisconnected,
                        "host request receiver is unavailable",
                    ),
                ));
            }
            state.calls += 1;
            state.pending.insert(request_id.clone());
            if let Some(kind) = helper_kind {
                state.helper_calls.insert(
                    request_id.clone(),
                    HelperCall {
                        kind,
                        result_bytes: None,
                    },
                );
            }
            Ok(request_id)
        },
    )
    .map_err(|failure| js_error(&ctx, failure))?;
    let emit_state = state.clone();
    let emit_fatal = fatal.clone();
    let emit_cancel = cancellation.clone();
    let max_output = input.limits.max_output_bytes;
    let emit = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, json: String| -> rquickjs::Result<()> {
            let mut state = emit_state.borrow_mut();
            if let Some(reason) = stop_reason(&emit_cancel, deadline) {
                return Err(fail_callback(&ctx, &mut state, &emit_fatal, reason));
            }
            // Include an element separator in the retained-output budget, so a
            // million small values cannot avoid collection-overhead accounting.
            let selection = OutputSelection::Text {
                index: state.output.len(),
            };
            let bytes = (json.len() as u64)
                .saturating_add(serialized_len(&selection))
                .saturating_add(2);
            if state.output_bytes.saturating_add(bytes) > max_output {
                return Err(fail_callback(
                    &ctx,
                    &mut state,
                    &emit_fatal,
                    error(
                        ExecutionErrorKind::LimitExceeded,
                        "selected output exceeds byte limit",
                    ),
                ));
            }
            let value = serde_json::from_str(&json).map_err(|_| {
                fail_callback(
                    &ctx,
                    &mut state,
                    &emit_fatal,
                    error(ExecutionErrorKind::Internal, "invalid output JSON"),
                )
            })?;
            state.output_bytes += bytes;
            state.output.push(value);
            state.selections.push(selection);
            Ok(())
        },
    )
    .map_err(|failure| js_error(&ctx, failure))?;
    let select_state = state.clone();
    let select_fatal = fatal.clone();
    let select_cancel = cancellation.clone();
    let select = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, request_id: String| -> rquickjs::Result<()> {
            let mut state = select_state.borrow_mut();
            if let Some(reason) = stop_reason(&select_cancel, deadline) {
                return Err(fail_callback(&ctx, &mut state, &select_fatal, reason));
            }
            let Some(call) = state.helper_calls.get(&request_id) else {
                return Err(fail_callback(
                    &ctx,
                    &mut state,
                    &select_fatal,
                    error(
                        ExecutionErrorKind::Internal,
                        "unknown output admission receipt",
                    ),
                ));
            };
            let Some(result_bytes) = call.result_bytes else {
                return Err(fail_callback(
                    &ctx,
                    &mut state,
                    &select_fatal,
                    error(
                        ExecutionErrorKind::Internal,
                        "output admission has not completed successfully",
                    ),
                ));
            };
            let selection = match call.kind {
                HelperKind::Media => OutputSelection::Media {
                    request_id: request_id.clone(),
                },
                HelperKind::File => OutputSelection::File {
                    request_id: request_id.clone(),
                },
            };
            let bytes = result_bytes
                .saturating_add(serialized_len(&selection))
                .saturating_add(2);
            if state.output_bytes.saturating_add(bytes) > max_output {
                return Err(fail_callback(
                    &ctx,
                    &mut state,
                    &select_fatal,
                    error(
                        ExecutionErrorKind::LimitExceeded,
                        "selected output exceeds byte limit",
                    ),
                ));
            }
            state.output_bytes += bytes;
            state.selections.push(selection);
            state.helper_calls.remove(&request_id);
            Ok(())
        },
    )
    .map_err(|failure| js_error(&ctx, failure))?;
    let prelude: Function = ctx
        .eval(PRELUDE)
        .map_err(|failure| js_error(&ctx, failure))?;
    let catalog = serde_json::to_string(&input.bindings)
        .map_err(|failure| error(ExecutionErrorKind::Internal, failure.to_string()))?;
    let controls: Object = prelude
        .call((send, emit, select, catalog))
        .map_err(|failure| js_error(&ctx, failure))?;
    let compile: Function = controls
        .get("compile")
        .map_err(|failure| js_error(&ctx, failure))?;
    let deliver: Function = controls
        .get("deliver")
        .map_err(|failure| js_error(&ctx, failure))?;
    let serialize: Function = controls
        .get("serialize")
        .map_err(|failure| js_error(&ctx, failure))?;
    // Compile the complete function before any guest statement can emit effects.
    let program: Function = compile
        .call((&input.source,))
        .map_err(|failure| js_error(&ctx, failure))?;
    state.borrow_mut().startup_micros = elapsed_micros(started);
    let promise: Promise = program
        .call(())
        .map_err(|failure| js_error(&ctx, failure))?;
    loop {
        if let Some(reason) = stop_reason(cancellation, deadline) {
            return Err(reason);
        }
        if let Some(failure) = state.borrow().failure.clone() {
            return Err(failure);
        }
        if let Some(result) = promise.result::<JsValue>() {
            let value = result.map_err(|failure| js_error(&ctx, failure))?;
            if value.is_undefined() {
                return Ok(None);
            }
            let json: String = serialize.call((value,)).map_err(|failure| {
                let mut error = js_error(&ctx, failure);
                error.kind = ExecutionErrorKind::UnsupportedValue;
                error
            })?;
            if state
                .borrow()
                .output_bytes
                .saturating_add(json.len() as u64)
                > input.limits.max_output_bytes
            {
                return Err(error(
                    ExecutionErrorKind::LimitExceeded,
                    "return value exceeds output byte limit",
                ));
            }
            return serde_json::from_str(&json)
                .map(Some)
                .map_err(|failure| error(ExecutionErrorKind::Internal, failure.to_string()));
        }
        // Service completions between jobs so a self-scheduling microtask chain
        // cannot starve the bridge. Both jobs and waits check the same deadline.
        match completions.try_recv() {
            Ok(completion) => apply_completion(&ctx, &deliver, &state, completion)?,
            Err(sync_mpsc::TryRecvError::Disconnected) => {
                return Err(error(
                    ExecutionErrorKind::HostDisconnected,
                    "host completion sender disconnected",
                ));
            }
            Err(sync_mpsc::TryRecvError::Empty) => {}
        }
        if ctx.execute_pending_job() {
            state.borrow_mut().jobs += 1;
            continue;
        }
        if state.borrow().pending.is_empty() {
            return Err(error(
                ExecutionErrorKind::Javascript,
                "script is awaiting a promise with no runnable jobs or host calls",
            ));
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(IDLE_POLL);
        match completions.recv_timeout(wait) {
            Ok(completion) => apply_completion(&ctx, &deliver, &state, completion)?,
            Err(sync_mpsc::RecvTimeoutError::Disconnected) => {
                return Err(error(
                    ExecutionErrorKind::HostDisconnected,
                    "host completion sender disconnected",
                ));
            }
            Err(sync_mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn apply_completion<'js>(
    ctx: &Ctx<'js>,
    deliver: &Function<'js>,
    state: &Rc<RefCell<State>>,
    completion: HostCompletion,
) -> Result<(), ExecutionError> {
    // A duplicate host completion cannot settle a later request or inject a new
    // result. Treat a faulty host bridge as a terminal integration error.
    if !state.borrow().pending.contains(&completion.request_id) {
        return Err(error(
            ExecutionErrorKind::Internal,
            "host completed an unknown or already completed request",
        ));
    }
    let (success, json) = match completion.outcome {
        Ok(value) => (true, serde_json::to_string(&value)),
        Err(value) => (false, serde_json::to_string(&value)),
    };
    let json = json.map_err(|failure| error(ExecutionErrorKind::Internal, failure.to_string()))?;
    {
        let mut state = state.borrow_mut();
        if success {
            if let Some(call) = state.helper_calls.get_mut(&completion.request_id) {
                call.result_bytes = Some(json.len() as u64);
            }
        } else {
            state.helper_calls.remove(&completion.request_id);
        }
    }
    deliver
        .call::<_, ()>((&completion.request_id, success, json))
        .map_err(|failure| js_error(ctx, failure))?;
    state.borrow_mut().pending.remove(&completion.request_id);
    Ok(())
}

fn fail_callback(
    ctx: &Ctx<'_>,
    state: &mut State,
    fatal: &AtomicBool,
    failure: ExecutionError,
) -> rquickjs::Error {
    let exception = Exception::throw_type(ctx, &failure.message);
    if state.failure.is_none() {
        state.failure = Some(failure);
    }
    fatal.store(true, Ordering::Release);
    exception
}

fn native_error(failure: rquickjs::Error) -> ExecutionError {
    let kind = if matches!(failure, rquickjs::Error::Allocation) {
        ExecutionErrorKind::LimitExceeded
    } else {
        ExecutionErrorKind::Internal
    };
    error(kind, failure.to_string())
}

fn js_error(ctx: &Ctx<'_>, failure: rquickjs::Error) -> ExecutionError {
    if !matches!(failure, rquickjs::Error::Exception) {
        return native_error(failure);
    }
    let exception = ctx.catch();
    let message = if let Some(exception) = exception.as_exception() {
        exception
            .message()
            .unwrap_or_else(|| "JavaScript exception".into())
    } else if let Some(message) = exception.as_string() {
        message
            .to_string()
            .unwrap_or_else(|_| "JavaScript exception".into())
    } else {
        "JavaScript threw a non-error value".into()
    };
    // A guest can throw an arbitrarily large error string; report retention has
    // a small independent cap rather than bypassing selected-output limits.
    let mut message = message;
    if message.len() > 4096 {
        let mut end = 4096;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    let kind = if message.contains("out of memory")
        || message.contains("stack overflow")
        || message.contains("Maximum call stack size exceeded")
    {
        ExecutionErrorKind::LimitExceeded
    } else {
        ExecutionErrorKind::Javascript
    };
    error(kind, message)
}
