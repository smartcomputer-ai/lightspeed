//! The wire form of the shared `deliver_emission` signal.
//!
//! The SDK decodes a signal's typed input before the handler runs and turns a
//! decode error into a failed workflow task. The signal stays in history, so
//! every retry fails the same way and the receiver never moves again. A
//! receiver must survive whatever a sender put on the wire, so every handler
//! takes the raw payloads and decodes them here. A malformed emission is
//! logged and dropped, never fatal.

use harness::EmissionEnvelope;
use temporalio_common::data_converters::{PayloadConverter, RawValue};
use temporalio_sdk::SyncWorkflowContext;

/// Longest decode error a receiver reports. Serde echoes the rejected value,
/// which here is whatever a sender chose to put on the wire.
const MAX_DECODE_ERROR_CHARS: usize = 512;

/// The signal input for one emission.
pub fn emission_signal_input(envelope: &EmissionEnvelope) -> RawValue {
    RawValue::from_value(envelope, &PayloadConverter::default())
}

/// Decodes the signal's first payload, which a well-formed sender always
/// provides. `RawValue::to_value` is not an option here: it panics on the
/// inputs this exists to survive, and a panic fails the workflow task too.
fn decode_emission(raw: &RawValue) -> Result<EmissionEnvelope, String> {
    let payload = raw
        .payloads
        .first()
        .ok_or_else(|| "emission signal has no payload".to_owned())?;
    serde_json::from_slice(&payload.data).map_err(|error| {
        format!("emission signal payload does not decode: {error}")
            .chars()
            .take(MAX_DECODE_ERROR_CHARS)
            .collect()
    })
}

/// Decodes one received `deliver_emission` signal. A failure is already logged
/// when it returns: the handler's whole duty is to drop the signal.
pub(crate) fn receive_emission<W>(
    ctx: &SyncWorkflowContext<W>,
    raw: &RawValue,
) -> Result<EmissionEnvelope, String> {
    decode_emission(raw).inspect_err(|error| {
        if !ctx.is_replaying() {
            tracing::error!(
                target: "temporal_workflow",
                event = "emission_signal_dropped",
                workflow_id = ctx.workflow_id(),
                error = error.as_str(),
                "dropping a malformed deliver_emission signal"
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{BlobRef, PromiseId, PromiseResolution};
    use serde_json::{Value, json};
    use temporalio_common::data_converters::{
        GenericPayloadConverter, SerializationContext, SerializationContextData,
    };
    use temporalio_common::protos::temporal::api::common::v1::Payload;

    fn envelope() -> EmissionEnvelope {
        EmissionEnvelope::source_resolution(
            uuid::Uuid::from_u128(1),
            "universe/producer".to_owned(),
            "universe/holder",
            PromiseId::new("promise_1"),
            PromiseResolution::Resolved {
                payload_ref: Some(BlobRef::from_bytes(b"reply")),
            },
        )
    }

    fn raw_bytes(data: Vec<u8>) -> RawValue {
        RawValue::new(vec![Payload {
            data,
            ..Default::default()
        }])
    }

    fn raw_json(value: &Value) -> RawValue {
        raw_bytes(serde_json::to_vec(value).unwrap())
    }

    fn mutated(path: &[&str], replacement: Value) -> RawValue {
        let mut value = serde_json::to_value(envelope()).unwrap();
        let mut target = &mut value;
        for key in path {
            target = target.get_mut(*key).expect("fixture path");
        }
        *target = replacement;
        raw_json(&value)
    }

    #[test]
    fn a_sent_envelope_decodes_back() {
        let input = emission_signal_input(&envelope());
        assert_eq!(input.payloads.len(), 1);
        assert_eq!(decode_emission(&input), Ok(envelope()));
    }

    #[test]
    fn the_typed_wire_form_is_what_a_plugin_sends() {
        let input = emission_signal_input(&envelope());
        assert_eq!(
            serde_json::from_slice::<Value>(&input.payloads[0].data).unwrap(),
            serde_json::to_value(envelope()).unwrap()
        );
    }

    /// The step before any handler runs, as the workflow macro spells it: this is
    /// where a typed input failed the workflow task.
    #[test]
    fn the_sdk_hands_over_any_payloads_as_raw() {
        let converter = PayloadConverter::default();
        let context_data = SerializationContextData::None;
        let context = SerializationContext::new(&context_data, &converter);
        let garbage = Payload {
            data: vec![0xff, 0x00, 0x7b],
            ..Default::default()
        };
        for payloads in [
            Vec::new(),
            vec![garbage.clone()],
            vec![garbage.clone(), garbage],
        ] {
            let raw: RawValue = converter
                .from_payloads(&context, payloads.clone())
                .expect("a raw input never fails the signal");
            assert_eq!(raw.payloads, payloads);
        }
    }

    #[test]
    fn no_payload_is_a_decode_failure() {
        let error = decode_emission(&RawValue::new(Vec::new())).unwrap_err();
        assert!(error.contains("no payload"), "{error}");
    }

    #[test]
    fn whatever_a_sender_puts_on_the_wire_is_a_decode_failure() {
        for (name, raw) in [
            ("default payload", RawValue::empty()),
            ("not json", raw_bytes(vec![0xff, 0x00, 0x7b])),
            ("wrong json type", raw_json(&json!([1, 2, 3]))),
            ("missing fields", raw_json(&json!({ "emission_id": "x" }))),
            (
                "foreign emission id",
                mutated(&["emission_id"], json!("my-retry-token")),
            ),
            (
                "unknown body kind",
                mutated(&["body", "kind"], json!("source_resolved")),
            ),
            (
                "invalid promise id",
                mutated(&["body", "promise_id"], json!("reply")),
            ),
            (
                "malformed payload ref",
                mutated(
                    &["body", "resolution", "payload_ref"],
                    json!(format!("sha256:{}", "A".repeat(64))),
                ),
            ),
        ] {
            assert!(decode_emission(&raw).is_err(), "{name} decoded");
        }
    }

    #[test]
    fn a_rejected_value_cannot_blow_up_what_the_receiver_reports() {
        let raw = mutated(&["emission_id"], json!("x".repeat(100_000)));
        assert!(decode_emission(&raw).unwrap_err().chars().count() <= MAX_DECODE_ERROR_CHARS);
    }
}
