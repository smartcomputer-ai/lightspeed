//! Native helper calls with real host persistence and durable selection receipts.

mod support;

use codemode::{
    Cancellation, ExecutionEvent, ExecutionReport, HostCompletion, HostError, OutputSelection,
};
use serde_json::json;

use support::{bounded, event, input};

#[tokio::test(flavor = "current_thread")]
#[ignore = "live native helper admission with asynchronous filesystem persistence"]
async fn selected_file_receipts_survive_a_later_media_rejection_and_script_failure() {
    bounded(async {
        let directory = tempfile::tempdir().expect("isolated host directory");
        let payload_path = directory.path().join("payload.txt");
        let receipt_path = directory.path().join("admission.json");
        let report_path = directory.path().join("execution.json");
        // SHA-256 of the exact fixture bytes "hello\n". The standalone engine
        // treats this host descriptor as JSON and does not perform storage I/O.
        let content_ref = "sha256:5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";
        let mut execution = codemode::start(
            input(
                r#"
                const created = await file({text:"hello\n"}, {name:"report.txt"});
                try { await media(created); }
                catch (error) { text({kind:error.kind}); }
                throw new Error("failure after selected output");
                "#,
                &[("blob_put","host-put"),("blob_info","host-info"),("blob_read","host-read")],
            ),
            Cancellation::default(),
        ).unwrap();
        let sender = execution.completion_sender();
        let report = loop {
            let request = match event(&mut execution).await {
                ExecutionEvent::Request(request) => request,
                ExecutionEvent::Finished(report) => break report,
            };
            let outcome = match request.binding_id.as_str() {
                "host-put" => {
                    assert_eq!(request.arguments["text"], "hello\n");
                    tokio::fs::write(&payload_path, request.arguments["text"].as_str().unwrap().as_bytes()).await.unwrap();
                    Ok(json!({"content_ref":content_ref,"byte_len":6,"media_type":"text/plain","name":"report.txt"}))
                }
                "host-info" => {
                    assert_eq!(request.arguments["presentation"], "file");
                    assert_eq!(request.arguments["ref"]["content_ref"], content_ref);
                    let bytes = tokio::fs::read(&payload_path).await.unwrap();
                    assert_eq!(bytes, b"hello\n");
                    let descriptor = json!({"content_ref":content_ref,"byte_len":bytes.len(),"media_type":"text/plain","name":"report.txt","handle":"file:5891b5b522d5df086d0ff0b110"});
                    tokio::fs::write(&receipt_path, serde_json::to_vec(&json!({"request_id":request.request_id,"descriptor":descriptor})).unwrap()).await.unwrap();
                    Ok(descriptor)
                }
                "host-read" => {
                    assert_eq!(request.arguments["format"],"media");
                    assert_eq!(request.arguments["ref"]["content_ref"], content_ref);
                    let bytes = tokio::fs::read(&payload_path).await.unwrap();
                    assert_eq!(bytes,b"hello\n");
                    Err(HostError {kind:"unsupported_media".into(),message:"Text is not native media".into(),value:None})
                }
                binding => panic!("ungranted binding: {binding}"),
            };
            sender.complete(HostCompletion {request_id:request.request_id,outcome}).unwrap();
        };
        assert_eq!(report.error.as_ref().unwrap().message,"failure after selected output");
        assert_eq!(report.output,vec![json!({"kind":"unsupported_media"})]);
        assert_eq!(report.selections,vec![OutputSelection::File {request_id:"call-2".into()},OutputSelection::Text {index:0}]);
        assert!(report.pending_request_ids.is_empty());
        assert_eq!(report.metrics.tool_calls,3);
        tokio::fs::write(&report_path,serde_json::to_vec(&report).unwrap()).await.unwrap();
        drop(execution);
        let restored:ExecutionReport = serde_json::from_slice(&tokio::fs::read(&report_path).await.unwrap()).unwrap();
        let admission:serde_json::Value = serde_json::from_slice(&tokio::fs::read(&receipt_path).await.unwrap()).unwrap();
        assert_eq!(restored,report);
        assert_eq!(admission["request_id"],"call-2");
        assert_eq!(admission["descriptor"]["content_ref"],content_ref);
        assert_eq!(tokio::fs::read(&payload_path).await.unwrap(),b"hello\n");
    }).await;
}
