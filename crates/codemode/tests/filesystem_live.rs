//! Real filesystem effects behind the public interpreter boundary. The guest
//! receives only named tool wrappers; all paths and I/O remain in this host.

mod support;

use std::sync::Arc;

use codemode::{
    Cancellation, CompletionError, ExecutionErrorKind, ExecutionEvent, HostCompletion, HostError,
    HostRequest,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{fs, sync::Barrier, task::JoinSet};

use support::{bounded, event, input};

async fn filesystem_call(
    root: Arc<TempDir>,
    reads: Arc<Barrier>,
    request: HostRequest,
) -> HostCompletion {
    let value = match request.binding_id.as_str() {
        "input-reader" => {
            let id = request.arguments["id"].as_u64().expect("input id");
            assert!(id < 6);
            // No read can complete until the whole Promise.all group has
            // reached the host. This catches a blocking interpreter callback.
            reads.wait().await;
            let bytes = fs::read(root.path().join(format!("input-{id}.json")))
                .await
                .expect("read real input");
            serde_json::from_slice(&bytes).expect("input JSON")
        }
        "report-writer" => {
            let page = request.arguments["page"].as_u64().expect("report page");
            assert!(page < 2);
            fs::write(
                root.path().join(format!("report-{page}.json")),
                serde_json::to_vec(&request.arguments["summary"]).expect("summary JSON"),
            )
            .await
            .expect("write real report");
            json!({"page": page})
        }
        "report-reader" => {
            let page = request.arguments["page"].as_u64().expect("report page");
            assert!(page < 2);
            let bytes = fs::read(root.path().join(format!("report-{page}.json")))
                .await
                .expect("read previously committed report");
            serde_json::from_slice(&bytes).expect("report JSON")
        }
        binding => panic!("ungranted host binding: {binding}"),
    };
    HostCompletion {
        request_id: request.request_id,
        outcome: Ok(value),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "live native QuickJS and asynchronous filesystem effects"]
async fn parallel_reads_and_dependent_writes_repeat_across_a_javascript_loop() {
    bounded(async {
        let root = Arc::new(tempfile::tempdir().expect("isolated host directory"));
        for id in 0..6 {
            fs::write(
                root.path().join(format!("input-{id}.json")),
                serde_json::to_vec(&json!({"id":id,"enabled":id % 2 == 0,"points":id + 1}))
                    .unwrap(),
            )
            .await
            .expect("seed input");
        }
        let script = input(
            r#"
                let total = 0;
                for (let page = 0; page < 2; page++) {
                    const records = await Promise.all([0, 1, 2].map(offset =>
                        tools.read_record({id: page * 3 + offset})));
                    const selected = records.filter(record => record.enabled);
                    const summary = {
                        ids: selected.map(record => record.id),
                        total: selected.reduce((sum, record) => sum + record.points, 0)
                    };
                    const receipt = await tools.write_report({page, summary});
                    const persisted = await tools.read_report({page: receipt.page});
                    text(persisted);
                    total += persisted.total;
                }
                return {total};
            "#,
            &[
                ("read_record", "input-reader"),
                ("write_report", "report-writer"),
                ("read_report", "report-reader"),
            ],
        );
        let mut execution = codemode::start(script, Cancellation::default()).unwrap();
        let completions = execution.completion_sender();
        let reads = Arc::new(Barrier::new(3));
        let mut calls = JoinSet::new();
        let mut admitted = Vec::new();
        let report = loop {
            tokio::select! {
                event = event(&mut execution) => match event {
                    ExecutionEvent::Request(request) => {
                        admitted.push(request.clone());
                        calls.spawn(filesystem_call(root.clone(), reads.clone(), request));
                    }
                    ExecutionEvent::Finished(report) => break report,
                },
                Some(completion) = calls.join_next(), if !calls.is_empty() => {
                    completions.complete(completion.expect("host task must succeed")).unwrap();
                }
            }
        };
        assert_eq!(report.error, None, "{report:?}");
        assert!(calls.is_empty(), "every effect was awaited");
        assert!(report.pending_request_ids.is_empty());
        assert_eq!(report.metrics.tool_calls, 10);
        assert_eq!(report.return_value, Some(json!({"total": 9})));
        assert_eq!(
            report.output,
            [json!({"ids":[0,2],"total":4}), json!({"ids":[4],"total":5})]
        );
        assert_eq!(
            admitted
                .into_iter()
                .map(|call| call.request_id)
                .collect::<Vec<_>>(),
            (1..=10).map(|id| format!("call-{id}")).collect::<Vec<_>>()
        );
        for (page, expected) in report.output.iter().enumerate() {
            let bytes = fs::read(root.path().join(format!("report-{page}.json")))
                .await
                .expect("script report exists on disk");
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), *expected);
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "live filesystem failure recovery through the asynchronous host boundary"]
async fn missing_file_rejection_releases_capacity_for_recovery_and_readback() {
    bounded(async {
        let root = tempfile::tempdir().expect("isolated host directory");
        let path = root.path().join("created-by-recovery.json");
        let mut script = input(
            r#"
                try {
                    await tools.read_optional({});
                    throw new Error("the initial read must fail");
                } catch (error) {
                    if (error.kind !== "not_found") throw error;
                    text({kind: error.kind, value: error.value});
                    await tools.create({content: {recovered: true, value: 42}});
                }
                return await tools.read_optional({});
            "#,
            &[
                ("read_optional", "optional-reader"),
                ("create", "recovery-writer"),
            ],
        );
        // Recovery depends on returning the rejected request's capacity before
        // the guest can issue its next effect, just as successful calls do.
        script.limits.max_outstanding_tool_calls = 1;
        let mut execution = codemode::start(script, Cancellation::default()).unwrap();
        let sender = execution.completion_sender();
        let mut admitted = Vec::new();
        let mut missing_reads = 0;
        let report = loop {
            let request = match event(&mut execution).await {
                ExecutionEvent::Request(request) => request,
                ExecutionEvent::Finished(report) => break report,
            };
            admitted.push(request.binding_id.clone());
            let outcome = match request.binding_id.as_str() {
                "optional-reader" => match fs::read(&path).await {
                    Ok(bytes) => Ok(serde_json::from_slice(&bytes).expect("persisted JSON")),
                    Err(error) => {
                        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
                        missing_reads += 1;
                        Err(HostError {
                            kind: "not_found".into(),
                            message: "Optional document does not exist".into(),
                            value: Some(
                                json!({"resource": "optional-document", "recoverable": true}),
                            ),
                        })
                    }
                },
                "recovery-writer" => {
                    fs::write(
                        &path,
                        serde_json::to_vec(&request.arguments["content"]).unwrap(),
                    )
                    .await
                    .expect("persist guest-directed recovery");
                    Ok(Value::Null)
                }
                binding => panic!("ungranted host binding: {binding}"),
            };
            sender
                .complete(HostCompletion {
                    request_id: request.request_id,
                    outcome,
                })
                .expect("deliver filesystem outcome");
        };
        assert_eq!(missing_reads, 1);
        assert_eq!(
            admitted,
            ["optional-reader", "recovery-writer", "optional-reader"]
        );
        assert_eq!(report.error, None, "{report:?}");
        assert_eq!(report.metrics.tool_calls, 3);
        assert!(report.pending_request_ids.is_empty());
        assert_eq!(
            report.return_value,
            Some(json!({"recovered": true, "value": 42}))
        );
        assert_eq!(
            report.output,
            [json!({
                "kind": "not_found",
                "value": {"resource": "optional-document", "recoverable": true}
            })]
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).await.unwrap()).unwrap(),
            report.return_value.unwrap(),
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "live filesystem effects admitted before outstanding-call exhaustion"]
async fn fanout_limit_bounds_host_writes_even_when_guest_catches_rejections() {
    bounded(async {
        let root = Arc::new(tempfile::tempdir().expect("isolated host directory"));
        let mut script = input(
            r#"
                for (let id = 0; id < 6; id++) {
                    tools.write({id}).catch(() => null);
                }
                return "quota errors must not be hidden by catch";
            "#,
            &[("write", "bounded-writer")],
        );
        script.limits.max_outstanding_tool_calls = 2;
        let mut execution = codemode::start(script, Cancellation::default()).unwrap();
        let sender = execution.completion_sender();
        let mut admitted = Vec::new();
        let mut release = Vec::new();
        let mut writes = JoinSet::new();
        let report = loop {
            match event(&mut execution).await {
                ExecutionEvent::Request(request) => {
                    assert_eq!(request.binding_id, "bounded-writer");
                    let id = request.arguments["id"].as_u64().expect("file id");
                    admitted.push(request.request_id.clone());
                    let host_root = root.clone();
                    let (ready, wait) = tokio::sync::oneshot::channel();
                    release.push(ready);
                    writes.spawn(async move {
                        wait.await.expect("host releases admitted write");
                        fs::write(host_root.path().join(format!("effect-{id}.json")), b"true")
                            .await
                            .expect("commit admitted write after guest stops");
                        HostCompletion {
                            request_id: request.request_id,
                            outcome: Ok(json!(id)),
                        }
                    });
                }
                ExecutionEvent::Finished(report) => break report,
            }
        };
        assert_eq!(
            report.error.as_ref().unwrap().kind,
            ExecutionErrorKind::LimitExceeded
        );
        assert_eq!(report.metrics.tool_calls, 2);
        assert_eq!(admitted, ["call-1", "call-2"]);
        assert_eq!(report.pending_request_ids, admitted);
        assert_eq!(report.return_value, None);
        assert!(report.output.is_empty());

        for ready in release {
            ready.send(()).expect("admitted host task remains alive");
        }
        while let Some(completion) = writes.join_next().await {
            assert_eq!(
                sender.complete(completion.expect("host write succeeds")),
                Err(CompletionError::Stopped)
            );
        }
        for id in 0..6 {
            let result = fs::read(root.path().join(format!("effect-{id}.json"))).await;
            if id < 2 {
                assert_eq!(result.unwrap(), b"true");
            } else {
                assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
            }
        }
    })
    .await;
}

#[derive(Clone, Copy)]
enum Stop {
    ScriptError,
    Cancellation,
    Deadline,
}

async fn partial_effects(stop: Stop) {
    let root = Arc::new(tempfile::tempdir().expect("isolated host directory"));
    let ending = match stop {
        Stop::ScriptError => {
            "await tools.admitted({}); throw new Error('failure after a committed effect');"
        }
        Stop::Cancellation | Stop::Deadline => "await late; await tools.must_not_run({});",
    };
    let script = input(
        &format!("text(await tools.commit({{}})); const late = tools.commit_later({{}}); {ending}"),
        &[
            ("commit", "first-file"),
            ("commit_later", "second-file"),
            ("admitted", "admission-barrier"),
            ("must_not_run", "forbidden-after-stop"),
        ],
    );
    let mut execution = codemode::start(script, Cancellation::default()).unwrap();
    let sender = execution.completion_sender();
    let ExecutionEvent::Request(first) = event(&mut execution).await else {
        panic!("first effect must be requested")
    };
    assert_eq!(first.binding_id, "first-file");
    fs::write(root.path().join("first.json"), b"{\"committed\":\"first\"}")
        .await
        .expect("commit first effect");
    sender
        .complete(HostCompletion {
            request_id: first.request_id,
            outcome: Ok(json!({"committed":"first"})),
        })
        .unwrap();

    let ExecutionEvent::Request(second) = event(&mut execution).await else {
        panic!("second effect must be requested")
    };
    assert_eq!(second.binding_id, "second-file");
    let pending_id = second.request_id;
    let (release, admitted) = tokio::sync::oneshot::channel::<()>();
    let host_root = root.clone();
    let late_effect = tokio::spawn(async move {
        admitted
            .await
            .expect("host explicitly releases late effect");
        fs::write(
            host_root.path().join("second.json"),
            b"{\"committed\":\"second\"}",
        )
        .await
        .expect("already-admitted host effect can finish after JS stops");
    });

    let expected = match stop {
        Stop::ScriptError => {
            // Acknowledge only after the independent host effect was admitted.
            let ExecutionEvent::Request(barrier) = event(&mut execution).await else {
                panic!("admission barrier")
            };
            assert_eq!(barrier.binding_id, "admission-barrier");
            sender
                .complete(HostCompletion {
                    request_id: barrier.request_id,
                    outcome: Ok(Value::Null),
                })
                .unwrap();
            ExecutionErrorKind::Javascript
        }
        Stop::Cancellation => {
            execution.cancel();
            ExecutionErrorKind::Cancelled
        }
        Stop::Deadline => ExecutionErrorKind::TimedOut,
    };
    let ExecutionEvent::Finished(report) = event(&mut execution).await else {
        panic!("execution must stop without issuing another effect")
    };
    assert_eq!(report.error.as_ref().unwrap().kind, expected, "{report:?}");
    assert_eq!(report.output, [json!({"committed":"first"})]);
    assert_eq!(report.return_value, None);
    assert_eq!(
        report.pending_request_ids.as_slice(),
        std::slice::from_ref(&pending_id)
    );
    assert_eq!(
        fs::read(root.path().join("first.json")).await.unwrap(),
        b"{\"committed\":\"first\"}",
        "stopping JavaScript does not roll back an earlier effect"
    );
    assert_eq!(
        fs::metadata(root.path().join("second.json"))
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );

    release.send(()).expect("late host task is still alive");
    late_effect.await.expect("late effect finishes");
    assert_eq!(
        sender.complete(HostCompletion {
            request_id: pending_id,
            outcome: Ok(json!({"committed":"second"})),
        }),
        Err(CompletionError::Stopped),
        "a late result must not revive a terminated interpreter"
    );
    assert_eq!(
        fs::read(root.path().join("second.json")).await.unwrap(),
        b"{\"committed\":\"second\"}",
        "host effects have their own lifecycle; only the host knows their final outcome"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "live filesystem effects surviving a JavaScript exception"]
async fn script_failure_preserves_committed_and_unfinished_host_effects() {
    bounded(partial_effects(Stop::ScriptError)).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "live filesystem effects surviving interpreter cancellation"]
async fn cancellation_preserves_committed_and_unfinished_host_effects() {
    bounded(partial_effects(Stop::Cancellation)).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "live filesystem effects surviving the interpreter deadline"]
async fn deadline_preserves_committed_and_unfinished_host_effects() {
    bounded(partial_effects(Stop::Deadline)).await;
}
