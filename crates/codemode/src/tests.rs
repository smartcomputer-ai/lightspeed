use super::*;

use serde_json::json;

fn input(source: &str) -> ExecutionInput {
    ExecutionInput {
        source: source.into(),
        bindings: vec![ToolBinding {
            name: "echo".into(),
            binding_id: "opaque-echo".into(),
        }],
        limits: ExecutionLimits {
            timeout_ms: 2_000,
            max_memory_bytes: 16 * 1024 * 1024,
            max_stack_bytes: 256 * 1024,
            max_source_bytes: 64 * 1024,
            max_catalog_bytes: 64 * 1024,
            max_request_bytes: 16 * 1024,
            max_result_bytes: 16 * 1024,
            max_output_bytes: 16 * 1024,
            max_tool_calls: 32,
            max_outstanding_tool_calls: 8,
        },
    }
}

fn content_input(source: &str) -> ExecutionInput {
    let mut script = input(source);
    script.bindings = ["blob_put", "blob_info", "blob_read"]
        .into_iter()
        .map(|name| ToolBinding {
            name: name.into(),
            binding_id: format!("opaque-{name}"),
        })
        .collect();
    script
}

fn content_descriptor() -> Value {
    json!({"content_ref":format!("sha256:{}", "a".repeat(64)),"byte_len":4,"media_type":"image/png","name":"plot.png"})
}

async fn run_content(
    input: ExecutionInput,
    mut complete: impl FnMut(&HostRequest) -> Result<Value, HostError>,
) -> (Vec<HostRequest>, ExecutionReport) {
    let mut execution = start(input, Cancellation::default()).unwrap();
    let sender = execution.completion_sender();
    let mut requests = Vec::new();
    while let Some(event) = execution.next_event().await {
        match event {
            ExecutionEvent::Request(request) => {
                let result = sender.complete(HostCompletion {
                    request_id: request.request_id.clone(),
                    outcome: complete(&request),
                });
                assert!(result.is_ok() || result == Err(CompletionError::Stopped));
                requests.push(request);
            }
            ExecutionEvent::Finished(report) => return (requests, report),
        }
    }
    panic!("engine stopped without terminal report");
}

#[tokio::test(flavor = "current_thread")]
async fn content_helpers_use_only_admitted_tools_and_return_admission_descriptors() {
    let (requests, report) = run_content(content_input(r#"
        text("before");
        const created = await file({json:{answer:42}}, {name:"answer.json",media_type:"application/json"});
        await media({blobRef:created.content_ref,mimeType:"image/jpeg"}, {media_type:"image/png",name:"shown.png"});
        await file("file:aaaaaaaaaaaaaaaaaaaaaaaa", {name:"again.png"});
        return created;
    "#), |_| Ok(content_descriptor())).await;
    assert_eq!(report.error, None);
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].binding_id, "opaque-blob_put");
    assert_eq!(
        requests[0].arguments,
        json!({"json":{"answer":42},"name":"answer.json","media_type":"application/json"})
    );
    assert_eq!(requests[1].binding_id, "opaque-blob_info");
    assert_eq!(requests[1].arguments["presentation"], "file");
    assert_eq!(requests[1].arguments["name"], "answer.json");
    assert_eq!(requests[2].binding_id, "opaque-blob_read");
    assert_eq!(requests[2].arguments["format"], "media");
    assert_eq!(requests[2].arguments["ref"]["media_type"], "image/png");
    assert_eq!(requests[2].arguments["ref"]["name"], "shown.png");
    assert!(requests[2].arguments["ref"].get("mimeType").is_none());
    assert_eq!(
        requests[3].arguments["ref"],
        json!({"content_ref":"file:aaaaaaaaaaaaaaaaaaaaaaaa","name":"again.png"})
    );
    assert_eq!(report.return_value, Some(content_descriptor()));
    assert_eq!(report.output, vec![json!("before")]);
    assert_eq!(
        report.selections,
        vec![
            OutputSelection::Text { index: 0 },
            OutputSelection::File {
                request_id: "call-2".into()
            },
            OutputSelection::Media {
                request_id: "call-3".into()
            },
            OutputSelection::File {
                request_id: "call-4".into()
            }
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn existing_file_and_media_local_names_can_shadow_new_helpers() {
    let (requests, report) = run_content(
        content_input(
            r#"
        const file = await tools.blob_info({ref:"sha256:existing"});
        const media = {label:"local media value"};
        return {file,media};
    "#,
        ),
        |_| Ok(content_descriptor()),
    )
    .await;
    assert_eq!(report.error, None);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        report.return_value,
        Some(json!({"file":content_descriptor(),"media":{"label":"local media value"}}))
    );
    assert!(report.selections.is_empty());

    let (requests, report) = run_content(
        content_input(
            r#"
        async function show() { return await file("sha256:existing"); }
        await show();
        { const file = "local file"; const media = "local media"; text({file,media}); }
        await media("sha256:existing");
        return "done";
    "#,
        ),
        |_| Ok(content_descriptor()),
    )
    .await;
    assert_eq!(report.error, None);
    assert_eq!(requests.len(), 2);
    assert_eq!(report.return_value, Some(json!("done")));
    assert_eq!(
        report.selections,
        vec![
            OutputSelection::File {
                request_id: "call-1".into()
            },
            OutputSelection::Text { index: 0 },
            OutputSelection::Media {
                request_id: "call-2".into()
            }
        ]
    );

    let (requests, report) = run_content(
        content_input(
            r#"
        await file("sha256:existing");
        const file = ;
    "#,
        ),
        |_| panic!("invalid syntax must be rejected before effects"),
    )
    .await;
    assert!(requests.is_empty());
    assert_eq!(report.error.unwrap().kind, ExecutionErrorKind::Javascript);
}

#[tokio::test(flavor = "current_thread")]
async fn ranged_read_descriptors_select_the_original_identity_without_storing_the_slice() {
    let (requests, report) = run_content(content_input(r#"
        const range = {content_ref:"sha256:original",byte_len:1000,format:"bytes",offset:20,bytes_read:3,bytes:[1,2,3],truncated:true,next_offset:23};
        await file(range);
        await media({...range,format:"text",text:"preview"});
    "#), |_| Ok(content_descriptor())).await;
    assert_eq!(report.error, None);
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].binding_id, "opaque-blob_info");
    assert_eq!(requests[1].binding_id, "opaque-blob_read");
    for request in requests {
        assert_eq!(request.arguments["ref"]["content_ref"], "sha256:original");
        assert_eq!(request.arguments["ref"]["byte_len"], 1000);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn parallel_helpers_preserve_emission_order_when_admissions_finish_out_of_order() {
    let mut execution = start(
        content_input(
            r#"
        text("start");
        await Promise.all([media("media:aaaaaaaaaaaa"),file("file:aaaaaaaaaaaaaaaaaaaaaaaa")]);
        text("end");
    "#,
        ),
        Cancellation::default(),
    )
    .unwrap();
    let sender = execution.completion_sender();
    let Some(ExecutionEvent::Request(first)) = execution.next_event().await else {
        panic!("media request")
    };
    let Some(ExecutionEvent::Request(second)) = execution.next_event().await else {
        panic!("file request")
    };
    assert_eq!(first.arguments["format"], "media");
    assert_eq!(second.arguments["presentation"], "file");
    for request in [second, first] {
        sender
            .complete(HostCompletion {
                request_id: request.request_id,
                outcome: Ok(content_descriptor()),
            })
            .unwrap();
    }
    let Some(ExecutionEvent::Finished(report)) = execution.next_event().await else {
        panic!("report")
    };
    assert_eq!(report.error, None);
    assert_eq!(report.output, vec![json!("start"), json!("end")]);
    assert_eq!(
        report.selections,
        vec![
            OutputSelection::Text { index: 0 },
            OutputSelection::File {
                request_id: "call-2".into()
            },
            OutputSelection::Media {
                request_id: "call-1".into()
            },
            OutputSelection::Text { index: 1 }
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn text_and_ordinary_tool_calls_cannot_forge_asset_selection() {
    let (_, report) = run_content(
        content_input(
            r#"
        const descriptor = await tools.blob_read({ref:"media:aaaaaaaaaaaa",format:"media"});
        text({kind:"media",request_id:"call-1"});
        text(descriptor);
        return [typeof select,typeof emit,typeof send,typeof bindingIds,typeof invoke];
    "#,
        ),
        |_| Ok(content_descriptor()),
    )
    .await;
    assert_eq!(report.error, None);
    assert_eq!(
        report.selections,
        vec![
            OutputSelection::Text { index: 0 },
            OutputSelection::Text { index: 1 }
        ]
    );
    assert_eq!(report.return_value, Some(json!(vec!["undefined"; 5])));
}

#[tokio::test(flavor = "current_thread")]
async fn helper_capability_checks_precede_inline_storage_and_fail_catchably() {
    for missing in ["blob_info", "blob_put"] {
        let mut script = content_input(
            r#"try {await file({text:"report"});} catch(error) {return error.kind;}"#,
        );
        script.bindings.retain(|binding| binding.name != missing);
        let (requests, report) =
            run_content(script, |_| panic!("no capability means no tool effect")).await;
        assert!(requests.is_empty());
        assert_eq!(report.error, None);
        assert_eq!(report.return_value, Some(json!("unsupported_capability")));
    }
    for source in [
        r#"await file({path:"/tmp/report"});"#,
        r#"await media({url:"https://example.org/image"});"#,
        r#"await file({text:"x",bytes:[1]});"#,
        r#"await file({bytes:new Uint8Array([1])});"#,
        r#"await file("sha256:any",{unsupported:true});"#,
    ] {
        let (requests, report) = run_content(content_input(source), |_| {
            panic!("invalid input must not emit effects")
        })
        .await;
        assert!(requests.is_empty());
        assert_eq!(report.error.unwrap().kind, ExecutionErrorKind::Javascript);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn helper_failures_and_later_script_errors_keep_successful_sibling_selections() {
    let (requests, report) = run_content(
        content_input(
            r#"
        text("before");
        await file("file:aaaaaaaaaaaaaaaaaaaaaaaa");
        try {await media({bytes:[1,2,3]});} catch(error) {text(error.kind);}
        throw new Error("later failure");
    "#,
        ),
        |request| {
            if request.binding_id == "opaque-blob_read" {
                Err(HostError {
                    kind: "unsupported_media".into(),
                    message: "not an image".into(),
                    value: None,
                })
            } else {
                Ok(content_descriptor())
            }
        },
    )
    .await;
    assert_eq!(requests.len(), 3);
    assert_eq!(report.error.unwrap().message, "later failure");
    assert_eq!(
        report.output,
        vec![json!("before"), json!("unsupported_media")]
    );
    assert_eq!(
        report.selections,
        vec![
            OutputSelection::Text { index: 0 },
            OutputSelection::File {
                request_id: "call-1".into()
            },
            OutputSelection::Text { index: 1 }
        ]
    );
    assert!(report.pending_request_ids.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn helpers_obey_call_and_descriptor_output_budgets() {
    let mut script = content_input(r#"try {await file({text:"report"});} catch(error) {}"#);
    script.limits.max_tool_calls = 1;
    script.limits.max_outstanding_tool_calls = 1;
    let (requests, report) = run_content(script, |_| Ok(content_descriptor())).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        report.error.unwrap().kind,
        ExecutionErrorKind::LimitExceeded
    );
    assert!(report.selections.is_empty());
    let mut script = content_input(
        r#"text("before");try {await file("file:aaaaaaaaaaaaaaaaaaaaaaaa");} catch(error) {}"#,
    );
    script.limits.max_output_bytes = 200;
    let (_, report) = run_content(script, |_| {
        Ok(json!({"content_ref":"x","name":"x".repeat(300)}))
    })
    .await;
    assert_eq!(
        report.error.unwrap().kind,
        ExecutionErrorKind::LimitExceeded
    );
    assert_eq!(report.output, vec![json!("before")]);
    assert_eq!(report.selections, vec![OutputSelection::Text { index: 0 }]);
}

#[tokio::test(flavor = "current_thread")]
async fn helpers_capture_intrinsics_and_keep_receipts_private_under_guest_mutation() {
    let (requests, report) = run_content(
        content_input(
            r#"
        JSON.stringify=()=>'{"forged":true}';
        JSON.parse=()=>({forged:true});
        Object.keys=()=>[];
        Object.hasOwn=()=>false;
        Object.prototype.toJSON=()=>({forged:true});
        Array.prototype.toJSON=()=>({forged:true});
        const admitted=await media({bytes:[1,2,3]}, {name:"plot.png"});
        admitted.content_ref="forged";
        text(admitted);
        return typeof select;
    "#,
        ),
        |_| Ok(content_descriptor()),
    )
    .await;
    assert_eq!(report.error, None);
    assert_eq!(
        requests[0].arguments,
        json!({"bytes":[1,2,3],"name":"plot.png"})
    );
    assert_eq!(
        report.selections,
        vec![
            OutputSelection::Media {
                request_id: "call-2".into()
            },
            OutputSelection::Text { index: 0 }
        ]
    );
    assert_eq!(report.output[0]["content_ref"], "forged");
    assert_eq!(report.return_value, Some(json!("undefined")));
}

#[tokio::test(flavor = "current_thread")]
async fn unawaited_helpers_do_not_fabricate_selection_receipts() {
    let mut execution = start(
        content_input(r#"file("file:aaaaaaaaaaaaaaaaaaaaaaaa");return 1;"#),
        Cancellation::default(),
    )
    .unwrap();
    let Some(ExecutionEvent::Request(request)) = execution.next_event().await else {
        panic!("request")
    };
    let Some(ExecutionEvent::Finished(report)) = execution.next_event().await else {
        panic!("report")
    };
    assert_eq!(report.error, None);
    assert!(report.selections.is_empty());
    assert_eq!(report.pending_request_ids, vec![request.request_id]);
}

#[test]
fn historical_execution_reports_keep_legacy_text_output_without_selection_receipts() {
    let report: ExecutionReport = serde_json::from_value(json!({
        "output":["legacy"],"return_value":null,"error":null,"pending_request_ids":[],
        "metrics":{"startup_micros":0,"elapsed_micros":0,"tool_calls":0,"pending_jobs":0}
    }))
    .unwrap();
    assert!(report.selections.is_empty());
    assert_eq!(report.output, vec![json!("legacy")]);
}

async fn run(input: ExecutionInput) -> (Vec<HostRequest>, ExecutionReport) {
    let mut execution = start(input, Cancellation::default()).expect("start engine");
    let completions = execution.completion_sender();
    let mut requests = Vec::new();
    while let Some(event) = execution.next_event().await {
        match event {
            ExecutionEvent::Request(request) => {
                let result = completions.complete(HostCompletion {
                    request_id: request.request_id.clone(),
                    outcome: Ok(request.arguments.clone()),
                });
                assert!(result.is_ok() || result == Err(CompletionError::Stopped));
                requests.push(request);
            }
            ExecutionEvent::Finished(report) => return (requests, report),
        }
    }
    panic!("engine stopped without terminal report");
}

#[tokio::test(flavor = "current_thread")]
async fn executes_loops_dependencies_and_selected_output() {
    let (requests, report) = run(input(
        r#"
        let total = 0;
        for (let i = 1; i <= 3; i++) total += (await tools.echo({value: i})).value;
        text({total});
        return await tools.echo({value: total * 2});
    "#,
    ))
    .await;
    assert_eq!(report.error, None);
    assert_eq!(report.output, vec![json!({"total":6})]);
    assert_eq!(report.return_value, Some(json!({"value":12})));
    assert_eq!(
        requests
            .iter()
            .map(|request| &*request.request_id)
            .collect::<Vec<_>>(),
        ["call-1", "call-2", "call-3", "call-4"]
    );
    assert!(report.pending_request_ids.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn parallel_requests_are_issued_before_any_completion_and_settle_out_of_order() {
    let mut execution = start(
        input(
            r#"
        const values = await Promise.all([tools.echo({n:1}), tools.echo({n:2}), tools.echo({n:3})]);
        return values.map(value => value.n);
    "#,
        ),
        Cancellation::default(),
    )
    .unwrap();
    let completions = execution.completion_sender();
    let mut requests = Vec::new();
    for _ in 0..3 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), execution.next_event())
            .await
            .unwrap()
            .unwrap();
        let ExecutionEvent::Request(request) = event else {
            panic!("expected request before result")
        };
        requests.push(request);
    }
    for request in requests.into_iter().rev() {
        completions
            .complete(HostCompletion {
                request_id: request.request_id,
                outcome: Ok(request.arguments),
            })
            .unwrap();
    }
    let Some(ExecutionEvent::Finished(report)) = execution.next_event().await else {
        panic!("terminal report")
    };
    assert_eq!(report.error, None);
    assert_eq!(report.return_value, Some(json!([1, 2, 3])));
}

#[tokio::test(flavor = "current_thread")]
async fn host_errors_are_catchable_and_preserve_structured_error_data() {
    let mut execution = start(
        input(
            r#"
        const results = await Promise.allSettled([tools.echo({n:1}), tools.echo({n:2})]);
        return results.map(result => result.status === "fulfilled"
            ? result.value : {kind: result.reason.kind, value: result.reason.value});
    "#,
        ),
        Cancellation::default(),
    )
    .unwrap();
    let completions = execution.completion_sender();
    while let Some(event) = execution.next_event().await {
        match event {
            ExecutionEvent::Request(request) => {
                let outcome = if request.arguments["n"] == 1 {
                    Ok(json!({"ok":true}))
                } else {
                    Err(HostError {
                        kind: "denied".into(),
                        message: "Denied by session".into(),
                        value: Some(json!({"code":42})),
                    })
                };
                completions
                    .complete(HostCompletion {
                        request_id: request.request_id,
                        outcome,
                    })
                    .unwrap();
            }
            ExecutionEvent::Finished(report) => {
                assert_eq!(report.error, None);
                assert_eq!(
                    report.return_value,
                    Some(json!([{"ok":true},{"kind":"denied","value":{"code":42}}]))
                );
                return;
            }
        }
    }
    panic!("missing report");
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_json_values_fail_before_effects_and_can_be_caught() {
    for value in [
        "undefined",
        "{n:undefined}",
        "NaN",
        "Infinity",
        "1n",
        "(()=>1)",
        "Symbol('x')",
        "[1,,3]",
        "new Date()",
        "new Map()",
        "new Uint8Array(1)",
        "({get value(){return 1}})",
        "(()=>{const a={};a.a=a;return a})()",
        "({[Symbol('x')]:1})",
    ] {
        let source =
            format!("try {{ await tools.echo({value}); }} catch (error) {{ return 'caught'; }}");
        let (requests, report) = run(input(&source)).await;
        assert!(requests.is_empty(), "{value} crossed boundary");
        assert_eq!(report.error, None, "{value}");
        assert_eq!(report.return_value, Some(json!("caught")), "{value}");
    }
    let (_, report) = run(input("return {value: undefined};")).await;
    assert_eq!(
        report.error.unwrap().kind,
        ExecutionErrorKind::UnsupportedValue
    );
    let (_, report) = run(input("return;")).await;
    assert_eq!(report.error, None);
    assert_eq!(report.return_value, None);
}

#[tokio::test(flavor = "current_thread")]
async fn compilation_rejects_javascript_and_typescript_syntax_before_effects() {
    for source in [
        "await tools.echo({}); let broken = ;",
        "await tools.echo({}); const n: number = 3;",
    ] {
        let (requests, report) = run(input(source)).await;
        assert!(requests.is_empty());
        assert_eq!(report.error.unwrap().kind, ExecutionErrorKind::Javascript);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fresh_runtime_has_no_ambient_io_or_bridge_controls() {
    let (_, first) = run(input("globalThis.saved=42; return typeof saved;")).await;
    assert_eq!(first.return_value, Some(json!("number")));
    let (_, report) = run(input(
        r#"
        return [typeof saved, typeof process, typeof require, typeof fetch, typeof setTimeout,
            typeof std, typeof os, typeof send, typeof emit, typeof deliver, typeof pending];
    "#,
    ))
    .await;
    assert_eq!(report.error, None);
    assert_eq!(report.return_value, Some(json!(vec!["undefined"; 11])));
    let (_, report) = run(input("return await import('file:///etc/passwd');")).await;
    assert_eq!(report.error.unwrap().kind, ExecutionErrorKind::Javascript);
}

#[tokio::test(flavor = "current_thread")]
async fn arbitrary_tool_names_and_mutated_intrinsics_do_not_expose_bridge_controls() {
    let mut script = input(
        r#"
        JSON.stringify = () => '{"forged":true}';
        JSON.parse = () => ({forged:true});
        Object.keys = () => [];
        Object.prototype.toJSON = () => ({forged:true});
        Array.prototype.toJSON = () => ({forged:true});
        const result = await tools["__proto__"]({n:1});
        text([result]);
        return await tools['tool " with / punctuation']({n:2});
    "#,
    );
    script.bindings = vec![
        ToolBinding {
            name: "__proto__".into(),
            binding_id: "first".into(),
        },
        ToolBinding {
            name: "tool \" with / punctuation".into(),
            binding_id: "second".into(),
        },
    ];
    let (requests, report) = run(script).await;
    assert_eq!(report.error, None);
    assert_eq!(report.output, vec![json!([{"n":1}])]);
    assert_eq!(report.return_value, Some(json!({"n":2})));
    assert_eq!(requests[0].binding_id, "first");
    assert_eq!(requests[1].binding_id, "second");
}

#[tokio::test(flavor = "current_thread")]
async fn returning_with_unawaited_calls_preserves_outstanding_request_identities() {
    let mut execution = start(
        input("tools.echo({n:1}); tools.echo({n:2}); return 3;"),
        Cancellation::default(),
    )
    .unwrap();
    let mut requests = Vec::new();
    while let Some(event) = execution.next_event().await {
        match event {
            ExecutionEvent::Request(request) => requests.push(request.request_id),
            ExecutionEvent::Finished(report) => {
                assert_eq!(report.return_value, Some(json!(3)));
                assert_eq!(report.error, None);
                assert_eq!(report.pending_request_ids, requests);
                return;
            }
        }
    }
    panic!("missing report");
}

#[tokio::test(flavor = "current_thread")]
async fn deadline_interrupts_synchronous_loops_and_microtask_loops() {
    for (source, expected) in [
        ("for (;;) {}", ExecutionErrorKind::TimedOut),
        (
            "try { for (;;) {} } catch (e) { return 'caught'; }",
            ExecutionErrorKind::TimedOut,
        ),
        (
            "for (;;) await Promise.resolve();",
            ExecutionErrorKind::TimedOut,
        ),
        (
            "await new Promise(() => {});",
            ExecutionErrorKind::Javascript,
        ),
    ] {
        let mut script = input(source);
        script.limits.timeout_ms = 30;
        let (_, report) = run(script).await;
        assert_eq!(report.error.as_ref().unwrap().kind, expected, "{report:?}");
        assert!(report.metrics.elapsed_micros < 1_000_000);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_interrupts_running_and_host_waiting_scripts() {
    for source in [
        "for (;;) {}",
        "for (;;) await Promise.resolve();",
        "await tools.echo({});",
    ] {
        let cancellation = Cancellation::default();
        let mut execution = start(input(source), cancellation.clone()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        cancellation.cancel();
        loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), execution.next_event())
                    .await
                    .unwrap()
                    .unwrap();
            if let ExecutionEvent::Finished(report) = event {
                assert_eq!(report.error.unwrap().kind, ExecutionErrorKind::Cancelled);
                break;
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn enforces_call_request_and_output_budgets_even_when_guest_catches_errors() {
    let mut total = input("for(let n=0;n<3;n++){ try { await tools.echo({n}); }catch(e){} }");
    total.limits.max_tool_calls = 2;
    total.limits.max_outstanding_tool_calls = 2;
    let (requests, report) = run(total).await;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        report.error.unwrap().kind,
        ExecutionErrorKind::LimitExceeded
    );
    let mut outstanding = input("tools.echo({}); tools.echo({}); tools.echo({});");
    outstanding.limits.max_outstanding_tool_calls = 2;
    let (requests, report) = run(outstanding).await;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        report.error.unwrap().kind,
        ExecutionErrorKind::LimitExceeded
    );
    for (source, request_limit, output_limit) in [
        ("await tools.echo({large:'x'.repeat(100)});", 32, 1024),
        ("try{text('x'.repeat(100));}catch(e){}", 1024, 32),
        ("text('a'.repeat(20));return 'b'.repeat(20);", 1024, 32),
    ] {
        let mut script = input(source);
        script.limits.max_request_bytes = request_limit;
        script.limits.max_output_bytes = output_limit;
        let (requests, report) = run(script).await;
        assert!(requests.is_empty());
        assert_eq!(
            report.error.unwrap().kind,
            ExecutionErrorKind::LimitExceeded
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn memory_and_native_stack_limits_terminate_the_attempt() {
    let mut memory = input("const values=[]; for(;;) values.push(new Array(1024).fill('x'));");
    memory.limits.max_memory_bytes = 1024 * 1024;
    let (_, report) = run(memory).await;
    // An exhausted QuickJS heap can reject with null because allocating the
    // exception object itself fails. Do not invent a more precise diagnosis.
    assert!(
        matches!(
            report.error.as_ref().unwrap().kind,
            ExecutionErrorKind::LimitExceeded | ExecutionErrorKind::Javascript
        ),
        "{report:?}"
    );
    assert!(report.metrics.elapsed_micros < 1_000_000);
    let (_, report) = run(input(
        "function recurse(){return 1 + recurse()} return recurse();",
    ))
    .await;
    assert_eq!(
        report.error.as_ref().unwrap().kind,
        ExecutionErrorKind::LimitExceeded,
        "{report:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_completions_are_rejected_before_entering_the_engine() {
    let mut script = input("return await tools.echo({});");
    script.limits.max_result_bytes = 128;
    let mut execution = start(script, Cancellation::default()).unwrap();
    let Some(ExecutionEvent::Request(request)) = execution.next_event().await else {
        panic!("request")
    };
    let sender = execution.completion_sender();
    assert_eq!(
        sender.complete(HostCompletion {
            request_id: request.request_id.clone(),
            outcome: Ok(json!("x".repeat(200)))
        }),
        Err(CompletionError::TooLarge)
    );
    sender
        .complete(HostCompletion {
            request_id: request.request_id,
            outcome: Ok(json!(1)),
        })
        .unwrap();
    let Some(ExecutionEvent::Finished(report)) = execution.next_event().await else {
        panic!("report")
    };
    assert_eq!(report.error, None);
    assert_eq!(report.return_value, Some(json!(1)));
}

#[test]
fn input_validation_happens_before_thread_creation() {
    let mut script = input("return 1;");
    script.limits.max_source_bytes = 1;
    assert!(matches!(
        start(script, Cancellation::default()),
        Err(StartError::InvalidInput(_))
    ));
    let mut script = input("");
    script.limits.max_catalog_bytes = 1;
    assert!(matches!(
        start(script, Cancellation::default()),
        Err(StartError::InvalidInput(_))
    ));
    let mut script = input("");
    script.bindings.push(script.bindings[0].clone());
    assert!(matches!(
        start(script, Cancellation::default()),
        Err(StartError::InvalidInput(_))
    ));
    let mut script = input("");
    script.limits.max_memory_bytes = 0;
    assert!(matches!(
        start(script, Cancellation::default()),
        Err(StartError::InvalidInput(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_execution_cancels_the_native_thread() {
    let execution = start(input("for (;;) {}"), Cancellation::default()).unwrap();
    let completion = execution.completion_sender();
    drop(execution);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let result = completion.complete(HostCompletion {
                request_id: "unused".into(),
                outcome: Ok(Value::Null),
            });
            if result == Err(CompletionError::Stopped) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("dropped execution must release its completion receiver");
}

#[tokio::test(flavor = "current_thread")]
async fn javascript_exceptions_preserve_their_message() {
    let (_, report) = run(input("throw new Error('failure from script');")).await;
    assert_eq!(
        report.error.unwrap(),
        ExecutionError {
            kind: ExecutionErrorKind::Javascript,
            message: "failure from script".into()
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_and_duplicate_completions_fail_without_losing_outstanding_calls() {
    for duplicate in [false, true] {
        let mut execution = start(
            input("await tools.echo({n:1}); await tools.echo({n:2});"),
            Cancellation::default(),
        )
        .unwrap();
        let sender = execution.completion_sender();
        let Some(ExecutionEvent::Request(first)) = execution.next_event().await else {
            panic!("first request")
        };
        let pending = if duplicate {
            sender
                .complete(HostCompletion {
                    request_id: first.request_id.clone(),
                    outcome: Ok(Value::Null),
                })
                .unwrap();
            let Some(ExecutionEvent::Request(second)) = execution.next_event().await else {
                panic!("second request")
            };
            second.request_id
        } else {
            first.request_id.clone()
        };
        sender
            .complete(HostCompletion {
                request_id: if duplicate {
                    first.request_id
                } else {
                    "unknown".into()
                },
                outcome: Ok(Value::Null),
            })
            .unwrap();
        let Some(ExecutionEvent::Finished(report)) = execution.next_event().await else {
            panic!("terminal report")
        };
        assert_eq!(report.error.unwrap().kind, ExecutionErrorKind::Internal);
        assert_eq!(report.pending_request_ids, vec![pending]);
        assert_eq!(
            sender.complete(HostCompletion {
                request_id: "unused".into(),
                outcome: Ok(Value::Null)
            }),
            Err(CompletionError::Stopped)
        );
    }
}
