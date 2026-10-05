//! Shared transfer assembly without a host clipboard or transport side effect.
use super::*;

/// Extraction retains exact UTF-8 bytes, connection-local sequence ordering and
/// expiry. Malformed transfers reject without including private payloads in errors.
#[tokio::test(start_paused = true)]
async fn shared_iroh_clipboard_retains_order_bounds_and_expiry() {
    let mut assembler = IrohClipboardAssembler::default();
    assembler.apply(r#"{"method":"client/clipboard.begin","params":{"sequence":1,"total_bytes":3,"chunks":1}}"#).unwrap();
    assembler.apply(r#"{"method":"client/clipboard.chunk","params":{"sequence":1,"index":0,"data_base64":"6Zuq"}}"#).unwrap();
    assert_eq!(
        assembler
            .apply(r#"{"method":"client/clipboard.commit","params":{"sequence":1}}"#)
            .unwrap()
            .as_deref(),
        Some("雪")
    );
    assert!(assembler.apply(r#"{"method":"client/clipboard.begin","params":{"sequence":1,"total_bytes":3,"chunks":1}}"#).is_err());
    assembler.apply(r#"{"method":"client/clipboard.begin","params":{"sequence":2,"total_bytes":6,"chunks":1}}"#).unwrap();
    let error = assembler.apply(r#"{"method":"client/clipboard.chunk","params":{"sequence":2,"index":1,"data_base64":"c2VjcmV0"}}"#).unwrap_err();
    assert!(!error.message().contains("secret"));
    assert!(assembler.expiration_deadline().is_none());
    assembler.apply(r#"{"method":"client/clipboard.begin","params":{"sequence":3,"total_bytes":6,"chunks":1}}"#).unwrap();
    assert!(assembler.expiration_deadline().is_some());
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assembler.discard_expired();
    assert!(assembler.expiration_deadline().is_none());
    assert!(
        assembler
            .apply(r#"{"method":"client/clipboard.commit","params":{"sequence":3}}"#)
            .is_err()
    );
}
