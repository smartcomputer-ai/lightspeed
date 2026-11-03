//! Which blobs a session log holds on to.
//!
//! A blob root stops the collector deleting a blob the log points at. Roots come
//! from values of type [`BlobRef`], found by walking the decoded event with a
//! serializer: a string is never a ref, whatever it looks like, so a model's reply
//! that happens to be a digest neither roots anything nor fails an append.
//!
//! The walk sees only what is typed. A ref held in a plain string retains nothing,
//! so [`check_typed_walk_sees_every_ref`] compares it with a scan by string shape on
//! every append to the in-memory store, which is the store tests drive writers
//! through.

use crate::{BlobRef, CoreAgentCodec, blob::BLOB_REF_NEWTYPE, session::StoredEvent};
use serde::{Serialize, ser};
use std::collections::BTreeSet;

use super::SessionStoreError;

/// Every [`BlobRef`] value reachable through `value`'s `Serialize` impl.
pub fn typed_blob_refs<T: Serialize + ?Sized>(value: &T) -> Result<BTreeSet<BlobRef>, String> {
    let mut collector = RefCollector::default();
    value
        .serialize(&mut collector)
        .map_err(|error| error.to_string())?;
    Ok(collector.refs)
}

/// The blobs an appended event must hold, for the store to root in the append's
/// own transaction. An event that does not decode could not be read back, so the
/// append refuses it now, as data no retry can fix.
pub fn event_blob_roots(event: &StoredEvent) -> Result<BTreeSet<BlobRef>, SessionStoreError> {
    let rejected = |error: String| SessionStoreError::DataRejected {
        message: format!("session event {}: {error}", event.kind),
    };
    let decoded = CoreAgentCodec
        .decode_event(event)
        .map_err(|error| rejected(error.to_string()))?;
    typed_blob_refs(&decoded).map_err(rejected)
}

/// String fields where a digest is someone's text and not a ref, named by the end
/// of their path through the event.
const FREE_TEXT_PATHS: &[&[&str]] = &[
    &["preview"],
    &["submission_id"],
    // Describes where an attachment came from; nothing resolves it.
    &["source", "id"],
];

/// Fails when a string shaped like a ref sits somewhere the typed walk does not
/// look: that string would retain nothing, and the blob it names would be collected
/// from under the log. Refs belong in [`BlobRef`] fields.
pub(super) fn check_typed_walk_sees_every_ref(
    event: &StoredEvent,
) -> Result<(), SessionStoreError> {
    let typed = event_blob_roots(event)?;
    let mut path = Vec::new();
    let mut missed = Vec::new();
    scan_ref_shapes(&event.payload, &mut path, &mut |path, blob_ref| {
        let free_text = FREE_TEXT_PATHS
            .iter()
            .any(|free_text| path.ends_with(free_text));
        if !free_text && !typed.contains(&blob_ref) {
            missed.push(format!("{} = {blob_ref}", path.join(".")));
        }
    });
    if missed.is_empty() {
        return Ok(());
    }
    Err(SessionStoreError::DataRejected {
        message: format!(
            "session event {} holds refs in plain strings, where they would retain nothing: {}",
            event.kind,
            missed.join(", ")
        ),
    })
}

fn scan_ref_shapes<'a>(
    value: &'a serde_json::Value,
    path: &mut Vec<&'a str>,
    found: &mut impl FnMut(&[&str], BlobRef),
) {
    match value {
        serde_json::Value::String(text) => {
            if let Ok(blob_ref) = BlobRef::parse(text.as_str()) {
                found(path, blob_ref);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                scan_ref_shapes(value, path, found);
            }
        }
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                path.push(key);
                scan_ref_shapes(value, path, found);
                path.pop();
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

#[derive(Default)]
struct RefCollector {
    refs: BTreeSet<BlobRef>,
    /// The next string is the inside of a [`BlobRef`].
    in_blob_ref: bool,
}

macro_rules! ignore_values {
    ($($method:ident: $value:ty),* $(,)?) => {
        $(fn $method(self, _: $value) -> Result<(), Self::Error> { Ok(()) })*
    };
}

macro_rules! walk_compound {
    ($($compound:ident: $method:ident),* $(,)?) => {$(
        impl ser::$compound for &mut RefCollector {
            type Ok = ();
            type Error = serde::de::value::Error;
            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
                value.serialize(&mut **self)
            }
            fn end(self) -> Result<(), Self::Error> {
                Ok(())
            }
        }
    )*};
}

macro_rules! walk_fields {
    ($($compound:ident),* $(,)?) => {$(
        impl ser::$compound for &mut RefCollector {
            type Ok = ();
            type Error = serde::de::value::Error;
            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                _: &'static str,
                value: &T,
            ) -> Result<(), Self::Error> {
                value.serialize(&mut **self)
            }
            fn end(self) -> Result<(), Self::Error> {
                Ok(())
            }
        }
    )*};
}

walk_compound!(
    SerializeSeq: serialize_element,
    SerializeTuple: serialize_element,
    SerializeTupleStruct: serialize_field,
    SerializeTupleVariant: serialize_field,
);
walk_fields!(SerializeStruct, SerializeStructVariant);

impl ser::SerializeMap for &mut RefCollector {
    type Ok = ();
    type Error = serde::de::value::Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        key.serialize(&mut **self)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl ser::Serializer for &mut RefCollector {
    type Ok = ();
    type Error = serde::de::value::Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    ignore_values!(
        serialize_bool: bool,
        serialize_i8: i8,
        serialize_i16: i16,
        serialize_i32: i32,
        serialize_i64: i64,
        serialize_u8: u8,
        serialize_u16: u16,
        serialize_u32: u32,
        serialize_u64: u64,
        serialize_f32: f32,
        serialize_f64: f64,
        serialize_char: char,
        serialize_bytes: &[u8],
        serialize_unit_struct: &'static str,
    );

    fn serialize_str(self, text: &str) -> Result<(), Self::Error> {
        if std::mem::take(&mut self.in_blob_ref) {
            self.refs
                .insert(BlobRef::parse(text).map_err(ser::Error::custom)?);
        }
        Ok(())
    }

    fn serialize_none(self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn serialize_unit(self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.in_blob_ref = name == BLOB_REF_NEWTYPE;
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Self, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple(self, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Self::Error> {
        Ok(self)
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Self, Self::Error> {
        Ok(self)
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Self::Error> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self, Self::Error> {
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContentRef, ContextEntryInput, ContextEntryKind, ContextMessageRole, CoreAgentEvent,
        RunEvent, RunId, ToolEffect, ToolEvent,
    };
    use serde::Deserialize;
    use std::collections::BTreeMap;

    fn digest(seed: &[u8]) -> BlobRef {
        BlobRef::from_bytes(seed)
    }

    fn encoded(event: &CoreAgentEvent) -> StoredEvent {
        CoreAgentCodec.encode_event(event).expect("encode event")
    }

    fn roots(event: &CoreAgentEvent) -> BTreeSet<BlobRef> {
        event_blob_roots(&encoded(event)).expect("roots")
    }

    fn message(content: BlobRef, preview: Option<String>) -> ContextEntryInput {
        ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::Assistant,
            },
            content: ContentRef::text(content),
            preview,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        }
    }

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Tagged {
        Inner { held: BlobRef },
        Wrapped(Struct),
        Nothing,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Untagged {
        Text(String),
        Held(BlobRef),
    }

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind", content = "data", rename_all = "snake_case")]
    enum Adjacent {
        Held(BlobRef),
    }

    #[derive(Serialize, Deserialize)]
    struct Flattened {
        beside: BlobRef,
    }

    #[derive(Serialize, Deserialize)]
    struct Struct {
        direct: BlobRef,
        optional: Option<BlobRef>,
        listed: Vec<BlobRef>,
        tuple: (u8, BlobRef),
        keyed: BTreeMap<BlobRef, u8>,
        valued: BTreeMap<String, BlobRef>,
        #[serde(flatten)]
        flattened: Flattened,
    }

    #[test]
    fn the_walk_reaches_a_ref_through_every_serde_shape() {
        let expected: Vec<_> = (0u8..10).map(|seed| digest(&[seed])).collect();
        let structure = Struct {
            direct: expected[0].clone(),
            optional: Some(expected[1].clone()),
            listed: vec![expected[2].clone()],
            tuple: (1, expected[3].clone()),
            keyed: BTreeMap::from([(expected[4].clone(), 1)]),
            valued: BTreeMap::from([("key".to_owned(), expected[5].clone())]),
            flattened: Flattened {
                beside: expected[6].clone(),
            },
        };
        let value = (
            Tagged::Inner {
                held: expected[7].clone(),
            },
            Tagged::Wrapped(structure),
            Tagged::Nothing,
            Untagged::Held(expected[8].clone()),
            Adjacent::Held(expected[9].clone()),
        );
        assert_eq!(
            typed_blob_refs(&value).unwrap(),
            expected.into_iter().collect()
        );
    }

    #[test]
    fn a_string_shaped_like_a_ref_is_not_a_ref() {
        let shaped = digest(b"shaped").to_string();
        let held = digest(b"held");
        let value = (
            shaped.clone(),
            Some(shaped.as_str()),
            vec![shaped.clone()],
            BTreeMap::from([(shaped.clone(), shaped.clone())]),
            serde_json::json!({ "opaque": shaped.clone() }),
            Untagged::Text(shaped.clone()),
            held.clone(),
        );
        assert_eq!(typed_blob_refs(&value).unwrap(), BTreeSet::from([held]));
    }

    #[test]
    fn a_string_after_a_ref_or_another_newtype_is_not_taken_for_one() {
        #[derive(Serialize)]
        struct Label(String);
        let shaped = digest(b"shaped").to_string();
        let held = digest(b"held");
        let value = (Label(shaped.clone()), held.clone(), shaped);
        assert_eq!(typed_blob_refs(&value).unwrap(), BTreeSet::from([held]));
    }

    #[test]
    fn a_reply_that_is_only_a_digest_roots_nothing_but_its_content() {
        let content = digest(b"stored reply");
        let event = CoreAgentEvent::Run(RunEvent::SteeringAccepted {
            run_id: RunId::new(1),
            steering_id: crate::SteeringId::new(1),
            input: vec![message(content.clone(), Some(digest(b"image").to_string()))],
            requested_by: None,
        });
        assert_eq!(roots(&event), BTreeSet::from([content]));
        assert!(check_typed_walk_sees_every_ref(&encoded(&event)).is_ok());
    }

    #[test]
    fn a_submission_id_that_is_a_digest_roots_nothing() {
        let event = CoreAgentEvent::Run(RunEvent::Accepted(crate::AcceptedRunEvent {
            run_id: RunId::new(1),
            submission_id: Some(crate::SubmissionId::new(digest(b"retry token").to_string())),
            source: crate::RunSource::Input { input: Vec::new() },
            run_config: Default::default(),
            config_revision: 0,
            notify_on_terminal: Vec::new(),
            requested_by: None,
        }));
        assert_eq!(roots(&event), BTreeSet::new());
        assert!(check_typed_walk_sees_every_ref(&encoded(&event)).is_ok());
    }

    fn call_completed(effect: ToolEffect) -> CoreAgentEvent {
        CoreAgentEvent::Tool(ToolEvent::CallCompleted {
            run_id: RunId::new(1),
            turn_id: crate::TurnId::new(1),
            batch_id: crate::ToolBatchId::new(1),
            result: crate::ToolCallResult {
                call_id: crate::ToolCallId::new("call"),
                status: crate::ToolCallStatus::Succeeded,
                output_ref: None,
                model_visible_context_entries: Vec::new(),
                error_ref: None,
                effects: vec![effect],
                attachments: Vec::new(),
                duration_ms: None,
                output_bytes: None,
                truncated: false,
            },
        })
    }

    #[test]
    fn a_ref_on_a_tool_effect_is_a_root() {
        let snapshot = digest(b"older workspace head");
        let event = call_completed(ToolEffect {
            kind: "vfs.workspace_commit".to_owned(),
            data: BTreeMap::from([("revision".to_owned(), "2".to_owned())]),
            refs: BTreeMap::from([("snapshot_ref".to_owned(), snapshot.clone())]),
        });
        assert!(roots(&event).contains(&snapshot));
        assert!(check_typed_walk_sees_every_ref(&encoded(&event)).is_ok());
    }

    #[test]
    fn the_cross_check_catches_a_ref_a_writer_left_in_a_plain_string() {
        let snapshot = digest(b"older workspace head");
        let event = call_completed(ToolEffect {
            kind: "vfs.workspace_commit".to_owned(),
            data: BTreeMap::from([("snapshot_ref".to_owned(), snapshot.to_string())]),
            refs: BTreeMap::new(),
        });
        assert!(!roots(&event).contains(&snapshot));
        let error = check_typed_walk_sees_every_ref(&encoded(&event)).unwrap_err();
        assert!(error.is_data_rejection());
        assert!(error.to_string().contains("snapshot_ref"), "{error}");
    }

    #[test]
    fn a_free_text_digest_outside_a_known_path_is_still_reported() {
        let mut event = encoded(&CoreAgentEvent::Environment(
            crate::EnvironmentEvent::ActiveEnvironmentCleared,
        ));
        event.payload = serde_json::json!({
            "environment": "active_environment_cleared",
            "notes": digest(b"anything").to_string(),
        });
        // The tolerated unknown field holds a digest: exactly the kind of field that
        // has to name itself free text or become a typed ref.
        assert!(check_typed_walk_sees_every_ref(&event).is_err());
    }

    #[test]
    fn an_event_that_does_not_decode_is_refused_as_data() {
        for event in [
            StoredEvent::new("lightspeed.test.event", 1, serde_json::json!({})),
            StoredEvent::new(
                crate::event_kind::RUN_STARTED,
                1,
                serde_json::json!({ "run": "started" }),
            ),
        ] {
            let error = event_blob_roots(&event).unwrap_err();
            assert!(error.is_data_rejection(), "{error}");
        }
    }

    #[test]
    fn a_serialize_impl_refusing_itself_comes_back_as_an_error() {
        struct Refuses;
        impl Serialize for Refuses {
            fn serialize<S: ser::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(ser::Error::custom("refused"))
            }
        }
        assert!(typed_blob_refs(&Refuses).is_err());
    }
}
