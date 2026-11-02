//! The line logged for a received text message. A test binary of its own:
//! tracing caches per call site whether anybody listens, so a capturing
//! subscriber next to parallel tests without one loses events at random.

mod common;

use common::*;
use localsend::http::client::LsHttpClientV2;
use localsend::http::dto_v2::{PrepareUploadRequestDtoV2, RegisterDtoV2};
use localsend::model::discovery::{DeviceType, ProtocolType};
use localsend::model::transfer::FileDto;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn text_message_logged_without_its_text() {
    let capture = LogCapture::default();
    let writer = capture.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt().with_writer(move || writer.clone()).with_ansi(false).finish(),
    )
    .unwrap();

    let mut node = NodeSpec::new(0).start().await;
    let sender = identity(1);
    let client =
        LsHttpClientV2::try_new(&sender.key_pem, &sender.cert_pem, Some(node.fp.clone()), None)
            .unwrap();
    let text = "PIN 4711 è segreto";
    let message = FileDto {
        id: "m".into(),
        file_name: "m.txt".into(),
        size: text.len() as u64,
        file_type: "text/plain".into(),
        sha256: None,
        preview: Some(text.into()),
        metadata: None,
    };
    let request = PrepareUploadRequestDtoV2 {
        info: RegisterDtoV2 {
            alias: "phone".into(),
            version: "2.2".into(),
            device_model: None,
            device_type: Some(DeviceType::Mobile),
            fingerprint: sender.fingerprint.clone(),
            port: 1,
            protocol: ProtocolType::Https,
            download: false,
        },
        files: HashMap::from([("m".to_string(), message)]),
    };
    client
        .prepare_upload(
            ProtocolType::Https,
            "127.0.0.1",
            node.port,
            None,
            request,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    node.wait_for(|r| matches!(r, Rec::Text(_)).then_some(())).await;

    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("text message from phone (18 characters)"), "{log}");
    assert!(!log.contains("4711"), "the text must never be logged");
}
