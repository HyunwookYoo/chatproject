#[cfg(not(all(feature = "test-hooks", debug_assertions)))]
#[tokio::test]
async fn hooks_are_unavailable_without_debug_feature() {
    let error = chat_core::CoreSlot::new()
        .test_hooks_start("ws://127.0.0.1:47000".into(), "disabled".into())
        .await
        .unwrap_err();
    assert_eq!(error.code, chat_core::ErrorCode::NotAvailable);
}
