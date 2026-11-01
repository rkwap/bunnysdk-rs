//! Resumable (TUS) Stream uploads against a local mock server.

use std::time::Duration;

use bunnysdk::Config;
use bunnysdk::stream::{StreamClient, TusUpload};
use wiremock::matchers::{body_bytes, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> StreamClient {
    StreamClient::with_config("library-key", Config::new().base_url(server.uri())).unwrap()
}

async fn mount_create(server: &MockServer, size: &str) {
    Mock::given(method("POST"))
        .and(path("/tusupload"))
        .and(header("Tus-Resumable", "1.0.0"))
        .and(header("Upload-Length", size))
        .and(header("LibraryId", "9"))
        .and(header("VideoId", "vid-1"))
        .and(header_exists("AuthorizationSignature"))
        .and(header_exists("AuthorizationExpire"))
        .respond_with(ResponseTemplate::new(201).insert_header("Location", "/tusupload/abc"))
        .expect(1)
        .mount(server)
        .await;
}

fn patch(offset: &str, body: &[u8], new_offset: &str) -> Mock {
    Mock::given(method("PATCH"))
        .and(path("/tusupload/abc"))
        .and(header("Upload-Offset", offset))
        .and(header("Content-Type", "application/offset+octet-stream"))
        .and(body_bytes(body.to_vec()))
        .respond_with(ResponseTemplate::new(204).insert_header("Upload-Offset", new_offset))
}

#[tokio::test]
async fn uploads_in_chunks() {
    let server = MockServer::start().await;
    mount_create(&server, "10").await;
    patch("0", b"0123", "4").expect(1).mount(&server).await;
    patch("4", b"4567", "8").expect(1).mount(&server).await;
    patch("8", b"89", "10").expect(1).mount(&server).await;

    let options = TusUpload::new("My video", "video/mp4").chunk_size(4);
    client(&server)
        .upload_resumable(9, "vid-1", &b"0123456789"[..], 10, &options)
        .await
        .unwrap();

    // The account key never goes to the TUS endpoint; the signature does.
    for request in server.received_requests().await.unwrap() {
        assert!(!request.headers.contains_key("accesskey"));
    }
}

#[tokio::test]
async fn resumes_from_server_offset_after_a_failed_chunk() {
    let server = MockServer::start().await;
    mount_create(&server, "8").await;
    patch("0", b"0123", "4").expect(1).mount(&server).await;
    // The second chunk fails once, after the server stored 2 of its 4 bytes...
    Mock::given(method("PATCH"))
        .and(header("Upload-Offset", "4"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/tusupload/abc"))
        .respond_with(ResponseTemplate::new(200).insert_header("Upload-Offset", "6"))
        .expect(1)
        .mount(&server)
        .await;
    // ...so only the remaining 2 bytes are re-sent.
    patch("6", b"67", "8").expect(1).mount(&server).await;

    let options = TusUpload::new("My video", "video/mp4").chunk_size(4);
    client(&server)
        .upload_resumable(9, "vid-1", &b"01234567"[..], 8, &options)
        .await
        .unwrap();
}

#[tokio::test]
async fn keeps_resuming_when_the_offset_check_also_fails() {
    let server = MockServer::start().await;
    mount_create(&server, "8").await;
    patch("0", b"0123", "4").expect(1).mount(&server).await;
    Mock::given(method("PATCH"))
        .and(header("Upload-Offset", "4"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // The first offset check fails too; the upload must not give up.
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/tusupload/abc"))
        .respond_with(ResponseTemplate::new(200).insert_header("Upload-Offset", "4"))
        .expect(1)
        .mount(&server)
        .await;
    patch("4", b"4567", "8").expect(1).mount(&server).await;

    let options = TusUpload::new("My video", "video/mp4").chunk_size(4);
    client(&server)
        .upload_resumable(9, "vid-1", &b"01234567"[..], 8, &options)
        .await
        .unwrap();
}

#[tokio::test]
async fn short_source_is_an_error() {
    let server = MockServer::start().await;
    mount_create(&server, "10").await;
    let options = TusUpload::new("My video", "video/mp4");
    let err = client(&server)
        .upload_resumable(9, "vid-1", &b"short"[..], 10, &options)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("ended after 5 of 10 bytes"),
        "{err}"
    );
}

#[test]
fn presigned_authorization() {
    let stream = StreamClient::new("library-key").unwrap();
    let auth = stream.tus_authorization(9, "vid-1", Duration::from_secs(3600));
    assert_eq!(auth.endpoint, "https://video.bunnycdn.com/tusupload");
    assert_eq!(auth.signature.len(), 64);
    let headers = auth.headers();
    assert_eq!(headers[2], ("LibraryId", "9".to_owned()));
    assert_eq!(headers[3], ("VideoId", "vid-1".to_owned()));
    let json = serde_json::to_value(&auth).unwrap();
    assert_eq!(json["libraryId"], 9);
    assert_eq!(json["videoId"], "vid-1");
}
