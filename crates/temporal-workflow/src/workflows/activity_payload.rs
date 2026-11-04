use temporalio_sdk::ActivityExecutionError;

/// A payload that does not convert says nothing about the activity: its result sits in
/// history and this build cannot read it. Taking the failure branch would close a session
/// as a failed bootstrap, or mark a healthy bot degraded, and no later build could undo
/// that. A panic fails the workflow task instead, and the task retries until a build that
/// reads the payload is running.
// The large error is the SDK's own type, passed straight through.
#[allow(clippy::result_large_err)]
pub(crate) fn fail_task_if_undecodable<T>(
    result: Result<T, ActivityExecutionError>,
) -> Result<T, ActivityExecutionError> {
    if let Err(ActivityExecutionError::Serialization(error)) = &result {
        panic!(
            "activity payload does not convert with this build ({}): {error}",
            std::any::type_name::<T>()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use temporalio_common::data_converters::PayloadConversionError;

    use super::*;

    #[test]
    #[should_panic(expected = "activity payload does not convert with this build")]
    fn an_undecodable_payload_fails_the_task() {
        let _ = fail_task_if_undecodable::<()>(Err(ActivityExecutionError::Serialization(
            PayloadConversionError::WrongEncoding,
        )));
    }

    #[test]
    fn a_result_passes_through() {
        assert!(matches!(fail_task_if_undecodable(Ok(7)), Ok(7)));
    }
}
