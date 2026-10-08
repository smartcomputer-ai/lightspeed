//! Public host integration under concurrent native interpreters and microtasks.

mod support;

use codemode::{Cancellation, ExecutionErrorKind, ExecutionEvent, HostCompletion, start};
use serde_json::json;
use support::{bounded, event, input};

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual native interpreter integration with asynchronous filesystem I/O"]
async fn concurrent_runtimes_isolate_request_identities_and_cancellation() {
    bounded(async {
        let directory = tempfile::tempdir().expect("create host directory");
        let cancelled_path = directory.path().join("cancelled.json");
        let healthy_path = directory.path().join("healthy.json");
        tokio::try_join!(
            tokio::fs::write(&cancelled_path, br#"{"runtime":"cancelled"}"#),
            tokio::fs::write(&healthy_path, br#"{"runtime":"healthy"}"#),
        )
        .expect("prepare distinct host files");

        let cancellation = Cancellation::default();
        let mut cancelled = start(
            input(
                "globalThis.marker = 'cancelled'; return await tools.read({});",
                &[("read", "cancelled-file")],
            ),
            cancellation.clone(),
        )
        .expect("start first interpreter");
        let mut healthy = start(
            input(
                "return {marker: typeof marker, value: await tools.read({})};",
                &[("read", "healthy-file")],
            ),
            Cancellation::default(),
        )
        .expect("start second interpreter");
        let healthy_completions = healthy.completion_sender();

        // Both native interpreters must have admitted their first request before
        // either receives a response or cancellation.
        let (cancelled_event, healthy_event) =
            tokio::join!(event(&mut cancelled), event(&mut healthy));
        let ExecutionEvent::Request(cancelled_request) = cancelled_event else {
            panic!("first interpreter did not request its bound file")
        };
        let ExecutionEvent::Request(healthy_request) = healthy_event else {
            panic!("second interpreter did not request its bound file")
        };
        assert_eq!(cancelled_request.request_id, "call-1");
        assert_eq!(healthy_request.request_id, cancelled_request.request_id);
        assert_eq!(cancelled_request.binding_id, "cancelled-file");
        assert_eq!(healthy_request.binding_id, "healthy-file");

        let (cancelled_bytes, healthy_bytes) = tokio::try_join!(
            tokio::fs::read(&cancelled_path),
            tokio::fs::read(&healthy_path),
        )
        .expect("read independently bound host files");
        assert_ne!(cancelled_bytes, healthy_bytes);
        cancellation.cancel();
        let ExecutionEvent::Finished(cancelled_report) = event(&mut cancelled).await else {
            panic!("cancelled interpreter did not terminate")
        };
        assert_eq!(
            cancelled_report.error.expect("cancellation outcome").kind,
            ExecutionErrorKind::Cancelled,
        );
        assert_eq!(
            cancelled_report.pending_request_ids,
            [cancelled_request.request_id],
        );

        // The same local request identity remains valid in the other runtime
        // after its sibling has stopped.
        healthy_completions
            .complete(HostCompletion {
                request_id: healthy_request.request_id,
                outcome: Ok(serde_json::from_slice(&healthy_bytes).expect("host file JSON")),
            })
            .expect("complete surviving interpreter request");
        let ExecutionEvent::Finished(healthy_report) = event(&mut healthy).await else {
            panic!("surviving interpreter did not finish")
        };
        assert_eq!(healthy_report.error, None);
        assert_eq!(
            healthy_report.return_value,
            Some(json!({"marker": "undefined", "value": {"runtime": "healthy"}})),
        );
        assert!(healthy_report.pending_request_ids.is_empty());
        assert_eq!(healthy_report.metrics.tool_calls, 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual native interpreter integration with asynchronous filesystem I/O"]
async fn host_completions_progress_during_continuous_microtasks() {
    bounded(async {
        let directory = tempfile::tempdir().expect("create host directory");
        let path = directory.path().join("result.json");
        let mut execution = start(
            input(
                r#"
                let settled = false;
                let iterations = 0;
                let checkpoint;
                const result = tools.read({}).then(value => {
                    settled = true;
                    return value;
                });
                while (!settled) {
                    if (++iterations === 128) {
                        checkpoint = tools.churning({iterations});
                    }
                    await Promise.resolve();
                }
                await checkpoint;
                return {value: await result, iterations};
                "#,
                &[("read", "host-file"), ("churning", "microtask-checkpoint")],
            ),
            Cancellation::default(),
        )
        .expect("start interpreter");
        let completions = execution.completion_sender();
        let ExecutionEvent::Request(read_request) = event(&mut execution).await else {
            panic!("interpreter did not request the bound file")
        };
        let ExecutionEvent::Request(checkpoint_request) = event(&mut execution).await else {
            panic!("interpreter did not reach its microtask checkpoint")
        };
        assert_eq!(read_request.binding_id, "host-file");
        assert_eq!(checkpoint_request.binding_id, "microtask-checkpoint");
        assert_eq!(checkpoint_request.arguments, json!({"iterations": 128}));

        // The checkpoint proves that the self-scheduling microtask chain is
        // active before asynchronous host I/O and either completion begin.
        tokio::fs::write(&path, br#"{"from":"host filesystem"}"#)
            .await
            .expect("write host result");
        let bytes = tokio::fs::read(&path).await.expect("read host result");
        completions
            .complete(HostCompletion {
                request_id: checkpoint_request.request_id,
                outcome: Ok(json!(null)),
            })
            .expect("complete microtask checkpoint");
        completions
            .complete(HostCompletion {
                request_id: read_request.request_id,
                outcome: Ok(serde_json::from_slice(&bytes).expect("host result JSON")),
            })
            .expect("complete file request during microtask churn");

        let ExecutionEvent::Finished(report) = event(&mut execution).await else {
            panic!("interpreter emitted an unexpected additional request")
        };
        assert_eq!(report.error, None);
        let value = report.return_value.expect("JavaScript result");
        assert_eq!(value["value"], json!({"from": "host filesystem"}));
        assert!(value["iterations"].as_u64().expect("iteration count") >= 128);
        assert!(report.pending_request_ids.is_empty());
        assert_eq!(report.metrics.tool_calls, 2);
    })
    .await;
}
