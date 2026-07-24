use super::*;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{post, put},
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

type UploadedParts = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

const MINIMAL_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9JfQAAAABJRU5ErkJggg==";
const MINIMAL_JPEG_BASE64: &str = concat!(
    "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////",
    "//////////////////////////////////////////2wBDAf//////////////////////////",
    "//////////////////////////////////////////////////////////wAARCAABAAEDASIA",
    "AhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAf/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oA",
    "DAMBAAIQAxAAAAF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAA",
    "AAAAAAAAAAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEB",
    "PwF//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAA",
    "AAAP/aAAgBAQABPyF//9k="
);

#[derive(Clone)]
struct PreparedPart {
    index: u32,
    block_size: Option<u64>,
}

#[derive(Clone)]
struct PrepareResponseConfig {
    block_size: Option<u64>,
    parts: Vec<PreparedPart>,
}

impl Default for PrepareResponseConfig {
    fn default() -> Self {
        Self {
            block_size: Some(5),
            parts: vec![PreparedPart {
                index: 1,
                block_size: Some(5),
            }],
        }
    }
}

#[derive(Clone)]
struct UploadTestState {
    base_url: String,
    failure_phase: Arc<Mutex<Option<&'static str>>>,
    prepare_response: Arc<Mutex<PrepareResponseConfig>>,
    prepare_payloads: Arc<Mutex<Vec<Value>>>,
    file_payloads: Arc<Mutex<Vec<Value>>>,
    finish_payloads: Arc<Mutex<Vec<Value>>>,
    put_parts: UploadedParts,
}

async fn files_handler(
    State(state): State<UploadTestState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let is_merge = payload.get("upload_id").is_some();
    state.file_payloads.lock().unwrap().push(payload);
    if is_merge && *state.failure_phase.lock().unwrap() == Some("merge") {
        return (
            StatusCode::BAD_GATEWAY,
            "merge failed https://temporary.test/?auth_token=secret",
        )
            .into_response();
    }
    Json(json!({
        "file_info": "file-info",
        "file_uuid": "file-uuid",
        "ttl": 1,
        "raw_url": format!("{}/temporary?auth_token=secret", state.base_url)
    }))
    .into_response()
}

async fn prepare_handler(
    State(state): State<UploadTestState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    state.prepare_payloads.lock().unwrap().push(payload);
    if *state.failure_phase.lock().unwrap() == Some("prepare") {
        return (
            StatusCode::BAD_GATEWAY,
            "prepare failed https://temporary.test/?auth_token=secret",
        )
            .into_response();
    }

    let prepared = state.prepare_response.lock().unwrap().clone();
    let parts = prepared
        .parts
        .iter()
        .map(|part| {
            let mut value = json!({
                "index": part.index,
                "presigned_url": format!(
                    "{}/presigned/{}?signature=secret",
                    state.base_url, part.index
                )
            });
            if let Some(block_size) = part.block_size {
                value["block_size"] = json!(block_size);
            }
            value
        })
        .collect::<Vec<_>>();
    let mut response = json!({
        "upload_id": "upload-1",
        "parts": parts,
        "upload_config": {"concurrency": 1, "retry_timeout": 1, "retry_delay": 0}
    });
    if let Some(block_size) = prepared.block_size {
        response["block_size"] = json!(block_size);
    }
    Json(response).into_response()
}

async fn part_handler(
    State(state): State<UploadTestState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    state.finish_payloads.lock().unwrap().push(payload);
    if *state.failure_phase.lock().unwrap() == Some("part_finish") {
        return (
            StatusCode::BAD_GATEWAY,
            "part finish failed https://temporary.test/?auth_token=secret",
        )
            .into_response();
    }
    Json(json!({})).into_response()
}

async fn put_handler(
    State(state): State<UploadTestState>,
    AxumPath(part_index): AxumPath<u32>,
    body: Bytes,
) -> impl IntoResponse {
    state
        .put_parts
        .lock()
        .unwrap()
        .push((part_index, body.to_vec()));
    if *state.failure_phase.lock().unwrap() == Some("put") {
        return (
            StatusCode::BAD_GATEWAY,
            "put failed https://temporary.test/?auth_token=secret",
        )
            .into_response();
    }
    StatusCode::OK.into_response()
}

async fn upload_test_server() -> (UploadTestState, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let state = UploadTestState {
        base_url,
        failure_phase: Arc::new(Mutex::new(None)),
        prepare_response: Arc::new(Mutex::new(PrepareResponseConfig::default())),
        prepare_payloads: Arc::new(Mutex::new(Vec::new())),
        file_payloads: Arc::new(Mutex::new(Vec::new())),
        finish_payloads: Arc::new(Mutex::new(Vec::new())),
        put_parts: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v2/users/user/files", post(files_handler))
        .route("/v2/users/user/upload_prepare", post(prepare_handler))
        .route("/v2/users/user/upload_part_finish", post(part_handler))
        .route("/presigned/{part_index}", put(put_handler))
        .with_state(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, task)
}

#[test]
fn url_upload_payload_uses_only_documented_fields() {
    let payload = serde_json::to_value(UploadUrlPayload {
        file_type: 1,
        url: "https://example.test/image.png",
        srv_send_msg: false,
    })
    .unwrap();

    assert_eq!(payload["file_type"], 1);
    assert_eq!(payload["url"], "https://example.test/image.png");
    assert_eq!(payload["srv_send_msg"], false);
    assert!(payload.get("file_data").is_none());
}

#[test]
fn png_data_url_generates_safe_png_file_name() {
    let image = ImagePayload::from_base64(format!("data:image/png;base64,{MINIMAL_PNG_BASE64}"));
    let upload = resolve_image_upload(&image).unwrap();

    assert_eq!(upload.file_name, "image.png");
    assert_eq!(ImageFormat::detect(&upload.bytes), Some(ImageFormat::Png));
}

#[test]
fn jpeg_base64_generates_safe_jpg_file_name() {
    let image = ImagePayload::from_base64(MINIMAL_JPEG_BASE64);
    let upload = resolve_image_upload(&image).unwrap();

    assert_eq!(upload.file_name, "image.jpg");
    assert_eq!(ImageFormat::detect(&upload.bytes), Some(ImageFormat::Jpeg));
}

#[test]
fn unrecognized_base64_is_rejected_as_invalid_media() {
    let error = resolve_image_upload(&ImagePayload::from_base64("aGVsbG8="))
        .err()
        .expect("arbitrary base64 data must be rejected");

    assert!(matches!(
        error,
        ApiError::InvalidMedia("base64 data is not a supported PNG or JPEG")
    ));
}

#[test]
fn local_image_keeps_basename_and_requires_matching_supported_extension() {
    let png_bytes = STANDARD.decode(MINIMAL_PNG_BASE64).unwrap();
    let file_name = format!("qq-maid-upload-{}.png", fastrand::u64(..));
    let path = std::env::temp_dir().join(&file_name);
    std::fs::write(&path, &png_bytes).unwrap();

    let upload =
        resolve_image_upload(&ImagePayload::from_local_path(path.to_string_lossy())).unwrap();
    assert_eq!(upload.file_name, file_name);
    assert_eq!(upload.bytes, png_bytes);

    let mismatched_path = path.with_extension("jpg");
    std::fs::write(&mismatched_path, &upload.bytes).unwrap();
    let error = resolve_image_upload(&ImagePayload::from_local_path(
        mismatched_path.to_string_lossy(),
    ))
    .err()
    .expect("mismatched local extension must be rejected");

    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(mismatched_path);
    assert!(matches!(
        error,
        ApiError::InvalidMedia("local image file extension does not match image data")
    ));
}

#[test]
fn c2c_and_group_upload_endpoints_are_never_interchanged() {
    assert_eq!(
        upload_endpoint(
            "https://api.example.test",
            UploadScene::C2c,
            "user",
            "files"
        ),
        "https://api.example.test/v2/users/user/files"
    );
    assert_eq!(
        upload_endpoint(
            "https://api.example.test",
            UploadScene::Group,
            "group",
            "files"
        ),
        "https://api.example.test/v2/groups/group/files"
    );
}

#[tokio::test]
async fn url_upload_uses_files_endpoint_and_never_sends_file_data() {
    let (state, task) = upload_test_server().await;
    let image = upload_url_media(
        &qq_maid_common::http_client::client(),
        &state.base_url,
        UploadScene::C2c,
        "user",
        "QQBot test",
        "https://example.test/image.png",
    )
    .await
    .unwrap();
    task.abort();

    assert_eq!(image.file_info.as_deref(), Some("file-info"));
    let payloads = state.file_payloads.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["url"], "https://example.test/image.png");
    assert!(payloads[0].get("file_data").is_none());
    assert!(payloads[0].get("upload_id").is_none());
}

#[tokio::test]
async fn single_part_upload_uses_numeric_fields_and_minimal_complete_payload() {
    let (state, task) = upload_test_server().await;
    upload_bytes_media(
        &qq_maid_common::http_client::client(),
        &state.base_url,
        UploadScene::C2c,
        "user",
        "QQBot test",
        b"hello",
        "image.png",
    )
    .await
    .unwrap();
    task.abort();

    assert_eq!(
        state.put_parts.lock().unwrap().as_slice(),
        &[(1, b"hello".to_vec())]
    );
    let prepare_payloads = state.prepare_payloads.lock().unwrap();
    assert_eq!(
        prepare_payloads.as_slice(),
        &[json!({
            "file_type": 1,
            "file_size": 5,
            "file_name": "image.png",
            "md5": hex_digest::<Md5>(b"hello"),
            "sha1": hex_digest::<Sha1>(b"hello"),
            "md5_10m": hex_digest::<Md5>(b"hello"),
        })]
    );
    assert_eq!(
        state.finish_payloads.lock().unwrap().as_slice(),
        &[json!({
            "upload_id": "upload-1",
            "part_index": 1,
            "block_size": 5,
            "md5": hex_digest::<Md5>(b"hello"),
        })]
    );
    assert_eq!(
        state.file_payloads.lock().unwrap().as_slice(),
        &[json!({"upload_id": "upload-1"})]
    );
}

#[tokio::test]
async fn byte_upload_does_not_cache_expiring_file_info() {
    let (state, task) = upload_test_server().await;
    let client = qq_maid_common::http_client::client();
    for bytes in [b"hello".as_slice(), b"local".as_slice()] {
        upload_bytes_media(
            &client,
            &state.base_url,
            UploadScene::C2c,
            "user",
            "QQBot test",
            bytes,
            "image.png",
        )
        .await
        .unwrap();
    }
    task.abort();

    assert_eq!(
        state.file_payloads.lock().unwrap().len(),
        2,
        "ttl=1 response must not create an unbounded file_info cache"
    );
}

#[tokio::test]
async fn three_parts_use_one_based_stride_offsets_and_actual_finish_sizes() {
    let (state, task) = upload_test_server().await;
    *state.prepare_response.lock().unwrap() = PrepareResponseConfig {
        block_size: Some(4),
        parts: vec![
            PreparedPart {
                index: 3,
                block_size: Some(2),
            },
            PreparedPart {
                index: 1,
                block_size: None,
            },
            PreparedPart {
                index: 2,
                block_size: Some(4),
            },
        ],
    };
    let bytes = b"abcdefghij";

    upload_bytes_media(
        &qq_maid_common::http_client::client(),
        &state.base_url,
        UploadScene::C2c,
        "user",
        "QQBot test",
        bytes,
        "image.png",
    )
    .await
    .unwrap();
    task.abort();

    let expected_blocks = [
        (1, b"abcd".as_slice()),
        (2, b"efgh".as_slice()),
        (3, b"ij".as_slice()),
    ];
    assert_eq!(
        state.put_parts.lock().unwrap().as_slice(),
        &expected_blocks
            .iter()
            .map(|(index, block)| (*index, block.to_vec()))
            .collect::<Vec<_>>()
    );

    let expected_finish_payloads = expected_blocks
        .iter()
        .map(|(index, block)| {
            json!({
                "upload_id": "upload-1",
                "part_index": index,
                "block_size": block.len(),
                "md5": hex_digest::<Md5>(block),
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        state.finish_payloads.lock().unwrap().as_slice(),
        expected_finish_payloads.as_slice()
    );
    assert_eq!(state.file_payloads.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_part_layouts_fail_before_put_or_merge() {
    let cases = [
        (
            PrepareResponseConfig {
                block_size: Some(5),
                parts: vec![PreparedPart {
                    index: 0,
                    block_size: Some(5),
                }],
            },
            "upload prepare response has invalid part index",
        ),
        (
            PrepareResponseConfig {
                block_size: Some(3),
                parts: vec![
                    PreparedPart {
                        index: 1,
                        block_size: Some(3),
                    },
                    PreparedPart {
                        index: 1,
                        block_size: Some(2),
                    },
                ],
            },
            "upload prepare response has duplicate part index",
        ),
        (
            PrepareResponseConfig {
                block_size: Some(3),
                parts: vec![
                    PreparedPart {
                        index: 1,
                        block_size: Some(3),
                    },
                    PreparedPart {
                        index: 3,
                        block_size: Some(2),
                    },
                ],
            },
            "upload prepare response has missing part index",
        ),
        (
            PrepareResponseConfig {
                block_size: Some(0),
                parts: vec![PreparedPart {
                    index: 1,
                    block_size: Some(5),
                }],
            },
            "invalid upload block_size",
        ),
        (
            PrepareResponseConfig {
                block_size: None,
                parts: vec![PreparedPart {
                    index: 1,
                    block_size: Some(5),
                }],
            },
            "invalid upload block_size",
        ),
        (
            PrepareResponseConfig {
                block_size: Some(2),
                parts: vec![
                    PreparedPart {
                        index: 1,
                        block_size: Some(3),
                    },
                    PreparedPart {
                        index: 2,
                        block_size: Some(2),
                    },
                ],
            },
            "invalid upload part layout",
        ),
        (
            PrepareResponseConfig {
                block_size: Some(5),
                parts: vec![PreparedPart {
                    index: 1,
                    block_size: Some(6),
                }],
            },
            "invalid upload part layout",
        ),
        (
            PrepareResponseConfig {
                block_size: Some(2),
                parts: vec![
                    PreparedPart {
                        index: 1,
                        block_size: Some(2),
                    },
                    PreparedPart {
                        index: 2,
                        block_size: Some(2),
                    },
                ],
            },
            "upload prepare parts do not cover the complete image",
        ),
    ];

    for (prepare_response, expected_reason) in cases {
        let (state, task) = upload_test_server().await;
        *state.prepare_response.lock().unwrap() = prepare_response;
        let error = upload_bytes_media(
            &qq_maid_common::http_client::client(),
            &state.base_url,
            UploadScene::C2c,
            "user",
            "QQBot test",
            b"hello",
            "image.png",
        )
        .await
        .expect_err("invalid part layout must fail");
        task.abort();

        assert!(
            matches!(error, ApiError::InvalidMedia(reason) if reason == expected_reason),
            "unexpected error: {error}"
        );
        assert!(state.put_parts.lock().unwrap().is_empty());
        assert!(state.finish_payloads.lock().unwrap().is_empty());
        assert!(state.file_payloads.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn prepare_put_part_finish_and_merge_failures_are_propagated() {
    for phase in ["prepare", "put", "part_finish", "merge"] {
        let (state, task) = upload_test_server().await;
        *state.failure_phase.lock().unwrap() = Some(phase);
        let error = upload_bytes_media(
            &qq_maid_common::http_client::client(),
            &state.base_url,
            UploadScene::C2c,
            "user",
            "QQBot test",
            b"hello",
            "image.png",
        )
        .await
        .unwrap_err();
        task.abort();
        assert!(matches!(error, ApiError::Status { .. }), "phase={phase}");
        let summary = error.log_summary();
        assert!(!summary.contains("auth_token"), "phase={phase}");
        assert!(!summary.contains("temporary.test"), "phase={phase}");
    }
}
