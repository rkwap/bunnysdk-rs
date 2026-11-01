//! Storage client tests against a local mock server.

use std::time::Duration;

use bunnysdk::storage::{Region, StorageClient, UploadOptions, checksum};
use bunnysdk::{Config, Error};
use serde_json::json;
use wiremock::matchers::{body_bytes, header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup(config: Config) -> (MockServer, StorageClient) {
    let server = MockServer::start().await;
    let client = StorageClient::with_config(
        "zone",
        "secret-pw",
        Region::Frankfurt,
        config.base_url(server.uri()),
    )
    .unwrap();
    (server, client)
}

#[tokio::test]
async fn list_sends_auth_and_parses_objects() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("GET"))
        .and(path("/zone/images/"))
        .and(header("AccessKey", "secret-pw"))
        .and(header(
            "User-Agent",
            concat!("bunnysdk-rs/", env!("CARGO_PKG_VERSION")),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "Guid": "d6445d80-a797-4535-bf0e-d3819bcdf928",
                "StorageZoneName": "zone",
                "Path": "/zone/images/",
                "ObjectName": "300kb.jpg",
                "Length": 300000,
                "LastChanged": "2017-03-10T03:06:48.203",
                "IsDirectory": false,
                "ServerId": 24,
                "UserId": "6eb577ca",
                "DateCreated": "2017-03-10T03:06:48.203",
                "StorageZoneId": 18201,
                "Checksum": null,
                "SomeFutureField": 1
            },
            { "ObjectName": "sub", "IsDirectory": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let objects = client.list("/images").await.unwrap();
    assert_eq!(objects.len(), 2);
    assert_eq!(objects[0].object_name, "300kb.jpg");
    assert_eq!(objects[0].length, 300_000);
    assert_eq!(objects[0].checksum, None);
    assert!(objects[1].is_directory);
}

#[tokio::test]
async fn list_root() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("GET"))
        .and(path("/zone/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    assert!(client.list("").await.unwrap().is_empty());
}

#[tokio::test]
async fn upload_sends_body_and_checksum() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("PUT"))
        .and(path("/zone/docs/a%20b.txt"))
        .and(header("Checksum", checksum(b"hello").as_str()))
        .and(body_bytes(b"hello".to_vec()))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"HttpCode": 201, "Message": "File uploaded."})),
        )
        .expect(1)
        .mount(&server)
        .await;
    client.upload("docs/a b.txt", &b"hello"[..]).await.unwrap();

    // No Content-Type by default, so bunny.net detects it from the file name.
    let requests = server.received_requests().await.unwrap();
    assert!(!requests[0].headers.contains_key("content-type"));
}

#[tokio::test]
async fn upload_with_options() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("PUT"))
        .and(path("/zone/logo.png"))
        .and(header("Content-Type", "image/png"))
        .and(header("Checksum", "ABCDEF"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    let options = UploadOptions::new()
        .content_type("image/png")
        .checksum("abcdef");
    client
        .upload_with("logo.png", vec![1u8, 2, 3], options)
        .await
        .unwrap();
}

#[tokio::test]
async fn download_and_stream() {
    let (server, client) = setup(Config::new()).await;
    let data = vec![7u8; 100_000];
    Mock::given(method("GET"))
        .and(path("/zone/big.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
        .mount(&server)
        .await;

    assert_eq!(client.download("big.bin").await.unwrap(), data);

    let mut download = client.download_stream("/big.bin").await.unwrap();
    assert_eq!(download.content_length(), Some(100_000));
    let mut received = Vec::new();
    while let Some(chunk) = download.chunk().await.unwrap() {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, data);
}

#[tokio::test]
async fn download_missing_file_is_not_found() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"HttpCode": 404, "Message": "Object Not Found"})),
        )
        .mount(&server)
        .await;

    let err = client.download("nope.txt").await.unwrap_err();
    assert!(err.is_not_found());
    let api = err.api().unwrap();
    assert_eq!(api.message, "Object Not Found");
    assert!(!err.to_string().contains("secret-pw"));
}

#[tokio::test]
async fn delete_file_and_directory() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("DELETE"))
        .and(path("/zone/a.txt"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/zone/dir/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    client.delete("a.txt").await.unwrap();
    client.delete("dir/").await.unwrap();
}

#[tokio::test]
async fn refuses_root_delete_and_traversal_without_sending() {
    let (server, client) = setup(Config::new()).await;
    for bad in ["", "/", "../other-zone/x"] {
        assert!(matches!(
            client.delete(bad).await,
            Err(Error::InvalidInput(_))
        ));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unauthorized_with_empty_body() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let err = client.list("").await.unwrap_err();
    assert_eq!(err.status(), Some(401));
    assert_eq!(
        err.api().unwrap().message,
        "Unauthorized. Check your API key."
    );
}

#[tokio::test]
async fn html_success_response_is_decode_error() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("<html>captive portal</html>", "text/html"),
        )
        .mount(&server)
        .await;
    let err = client.list("").await.unwrap_err();
    assert!(matches!(err, Error::Decode { status: 200, .. }), "{err}");
}

#[tokio::test]
async fn retries_rate_limit_then_succeeds() {
    let (server, client) = setup(Config::new().max_retries(2)).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    assert!(client.list("").await.unwrap().is_empty());
}

#[tokio::test]
async fn retries_are_off_by_default() {
    let (server, client) = setup(Config::new()).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(client.list("").await.unwrap_err().status(), Some(503));
}

#[tokio::test]
async fn retries_give_up_after_max() {
    let (server, client) = setup(Config::new().max_retries(2)).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "0"))
        .expect(3)
        .mount(&server)
        .await;
    assert_eq!(client.list("").await.unwrap_err().status(), Some(503));
}

#[tokio::test]
async fn retried_upload_resends_body() {
    let (server, client) = setup(Config::new().max_retries(1)).await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(body_bytes(b"data".to_vec()))
        .and(header_exists("Checksum"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    client.upload("f.txt", &b"data"[..]).await.unwrap();
}

#[tokio::test]
async fn silent_server_times_out() {
    let (server, client) = setup(Config::new().timeout(Duration::from_millis(200))).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let err = client.list("").await.unwrap_err();
    assert!(matches!(&err, Error::Http(e) if e.is_timeout()), "{err:?}");
}
