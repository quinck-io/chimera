use super::*;

#[test]
fn truncate_line_short_string_unchanged() {
    assert_eq!(truncate_line("hello", 1024), "hello");
}

#[test]
fn truncate_line_exact_length() {
    let s = "a".repeat(1024);
    assert_eq!(truncate_line(&s, 1024), s.as_str());
}

#[test]
fn truncate_line_over_limit() {
    let s = "a".repeat(2000);
    assert_eq!(truncate_line(&s, 1024).len(), 1024);
}

#[test]
fn truncate_line_respects_utf8_boundary() {
    // Each emoji is 4 bytes. 256 emojis = 1024 bytes exactly.
    let s = "\u{1F600}".repeat(257); // 1028 bytes
    let truncated = truncate_line(&s, 1024);
    assert!(truncated.len() <= 1024);
    assert!(truncated.is_char_boundary(truncated.len()));
    // Should be exactly 256 emojis = 1024 bytes
    assert_eq!(truncated.len(), 1024);
}

#[test]
fn truncate_line_empty_string() {
    assert_eq!(truncate_line("", 1024), "");
}

#[test]
fn to_websocket_url_https_to_wss() {
    assert_eq!(
        to_websocket_url("https://example.com/feed"),
        "wss://example.com/feed"
    );
}

#[test]
fn to_websocket_url_http_to_ws() {
    assert_eq!(
        to_websocket_url("http://localhost:8080/feed"),
        "ws://localhost:8080/feed"
    );
}

#[test]
fn to_websocket_url_already_wss() {
    assert_eq!(
        to_websocket_url("wss://example.com/feed"),
        "wss://example.com/feed"
    );
}

#[test]
fn to_websocket_url_preserves_path_and_query() {
    assert_eq!(
        to_websocket_url("https://pipelines.actions.githubusercontent.com/_ws/ingest.sock?v=1"),
        "wss://pipelines.actions.githubusercontent.com/_ws/ingest.sock?v=1"
    );
}

#[test]
fn feed_sender_truncates_content() {
    // Verify FeedLine captures truncated content via FeedSender
    let (tx, mut rx) = tokio::sync::mpsc::channel::<FeedLine>(16);
    let sender = FeedSender { tx };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let long_line = "x".repeat(2000);
        sender.send("step-1", &long_line).await;

        let line = rx.recv().await.unwrap();
        assert_eq!(line.step_id, "step-1");
        assert!(line.content.len() <= 1024);
    });
}

#[test]
fn send_batch_json_format() {
    // Verify the JSON message has PascalCase keys and correct structure
    use futures::SinkExt;
    use futures::channel::mpsc as futures_mpsc;

    let (mut tx, mut rx) = futures_mpsc::channel::<tokio_tungstenite::tungstenite::Message>(16);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        send_batch(
            &mut tx,
            "abc-123",
            &["line one".into(), "line two".into()],
            0,
        )
        .await;
        tx.close().await.unwrap();

        if let Some(msg) = futures::StreamExt::next(&mut rx).await {
            let text = msg.into_text().unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();

            assert_eq!(parsed["Count"], 2);
            assert_eq!(parsed["StepId"], "abc-123");
            assert_eq!(parsed["StartLine"], 1); // 1-based
            assert_eq!(parsed["Value"][0], "line one");
            assert_eq!(parsed["Value"][1], "line two");

            // Verify no camelCase keys leak through
            assert!(parsed.get("count").is_none());
            assert!(parsed.get("stepId").is_none());
        } else {
            panic!("expected a message");
        }
    });
}

#[test]
fn send_batch_start_line_offset() {
    use futures::SinkExt;
    use futures::channel::mpsc as futures_mpsc;

    let (mut tx, mut rx) = futures_mpsc::channel::<tokio_tungstenite::tungstenite::Message>(16);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // Simulate that 10 lines were already sent (start_line = 10)
        send_batch(&mut tx, "step-1", &["line 11".into()], 10).await;
        tx.close().await.unwrap();

        if let Some(msg) = futures::StreamExt::next(&mut rx).await {
            let parsed: serde_json::Value =
                serde_json::from_str(&msg.into_text().unwrap()).unwrap();
            assert_eq!(parsed["StartLine"], 11); // 1-based: 10 + 0 + 1
        }
    });
}

#[test]
fn send_batch_chunks_at_100_lines() {
    use futures::SinkExt;
    use futures::channel::mpsc as futures_mpsc;

    let (mut tx, mut rx) = futures_mpsc::channel::<tokio_tungstenite::tungstenite::Message>(16);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let lines: Vec<String> = (0..150).map(|i| format!("line {i}")).collect();
        send_batch(&mut tx, "step-1", &lines, 0).await;
        tx.close().await.unwrap();

        // Should produce 2 messages: 100 lines + 50 lines
        let msg1 = futures::StreamExt::next(&mut rx).await.unwrap();
        let p1: serde_json::Value = serde_json::from_str(&msg1.into_text().unwrap()).unwrap();
        assert_eq!(p1["Count"], 100);
        assert_eq!(p1["StartLine"], 1);

        let msg2 = futures::StreamExt::next(&mut rx).await.unwrap();
        let p2: serde_json::Value = serde_json::from_str(&msg2.into_text().unwrap()).unwrap();
        assert_eq!(p2["Count"], 50);
        assert_eq!(p2["StartLine"], 101);
    });
}
