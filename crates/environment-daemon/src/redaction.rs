//! Shared byte redaction for captured process and job output.

use environment_protocol::shared::SecretString;
use std::collections::BTreeMap;

pub(crate) fn redactions_for_secret_env(
    secret_env: &BTreeMap<String, SecretString>,
) -> Vec<Vec<u8>> {
    secret_env
        .values()
        .filter(|value| !value.is_empty())
        .map(|value| value.expose().as_bytes().to_vec())
        .collect()
}

pub(crate) fn redact_bytes(bytes: &[u8], redactions: &[Vec<u8>]) -> Vec<u8> {
    let mut output = bytes.to_vec();
    for secret in redactions {
        if secret.is_empty() || secret.len() > output.len() {
            continue;
        }
        let mut index = 0;
        while let Some(offset) = find_subslice(&output[index..], secret) {
            let start = index + offset;
            let end = start + secret.len();
            output.splice(start..end, b"<redacted>".iter().copied());
            index = start + b"<redacted>".len();
        }
    }
    output
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_repeated_and_adjacent_secrets_without_decoding_output() {
        let secrets = vec![b"token".to_vec(), b"password".to_vec()];
        assert_eq!(
            redact_bytes(b"\xfftoken tokenpassword\0", &secrets),
            b"\xff<redacted> <redacted><redacted>\0"
        );
    }

    #[test]
    fn empty_or_absent_secrets_leave_output_unchanged() {
        let secrets = vec![
            Vec::new(),
            b"longer-than-output".to_vec(),
            b"other".to_vec(),
        ];
        assert_eq!(redact_bytes(b"text", &secrets), b"text");
        assert!(redact_bytes(b"", &secrets).is_empty());
    }
}
