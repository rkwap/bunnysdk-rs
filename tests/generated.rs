//! Generated API clients against a local mock server.

use bunnysdk::Config;
use bunnysdk::containers::{ContainersClient, ListApplicationsQuery};
use bunnysdk::core::{
    CoreClient, GetPullZoneQuery, ListPullZonesQuery, PullZoneSettingsModel, PullZoneType,
};
use bunnysdk::stream::{CreateVideoModel, StreamClient, UploadVideoQuery, VideoModelStatus};
use serde_json::json;
use wiremock::matchers::{body_bytes, body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn core(server: &MockServer) -> CoreClient {
    CoreClient::with_config("account-key", Config::new().base_url(server.uri())).unwrap()
}

#[tokio::test]
async fn core_list_sends_query_and_decodes_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pullzone"))
        .and(header("AccessKey", "account-key"))
        .and(query_param("search", "my zone"))
        .and(query_param("includeCertificate", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "Id": 1, "Name": "a", "Type": 0, "Hostnames": [] },
            { "Id": 2, "Name": "b", "Type": 7, "BrandNewField": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let query = ListPullZonesQuery {
        search: Some("my zone".into()),
        include_certificate: Some(true),
        ..Default::default()
    };
    let zones = core(&server).await.list_pull_zones(&query).await.unwrap();
    assert_eq!(zones.len(), 2);
    assert_eq!(zones[0].name.as_deref(), Some("a"));
    assert_eq!(zones[0].r#type, Some(PullZoneType::Premium));
    // Unknown enum values and unknown fields don't break decoding.
    assert_eq!(zones[1].r#type, Some(PullZoneType::Other(7)));

    // Unset query fields are not sent.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].url.query_pairs().count(), 2);
}

#[tokio::test]
async fn core_paginated_list_decodes_items() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pullzone"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [{ "Id": 3 }], "CurrentPage": 2, "TotalItems": 3, "HasMoreItems": false
        })))
        .mount(&server)
        .await;
    let query = ListPullZonesQuery {
        page: Some(2),
        ..Default::default()
    };
    let zones = core(&server).await.list_pull_zones(&query).await.unwrap();
    assert_eq!(zones[0].id, Some(3));
}

#[tokio::test]
async fn core_update_sends_only_set_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/pullzone/42"))
        .and(body_json(
            json!({ "OriginUrl": "https://example.com", "EnableGeoZoneUS": false }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Id": 42 })))
        .expect(1)
        .mount(&server)
        .await;

    let body = PullZoneSettingsModel {
        origin_url: Some("https://example.com".into()),
        enable_geo_zone_us: Some(false),
        ..Default::default()
    };
    let zone = core(&server)
        .await
        .update_pull_zone(42, &body)
        .await
        .unwrap();
    assert_eq!(zone.id, Some(42));
}

#[tokio::test]
async fn core_errors_are_typed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pullzone/7"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/pullzone/8"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "ErrorKey": "pullzone.validation", "Field": "OriginUrl", "Message": "Invalid origin"
        })))
        .mount(&server)
        .await;

    let client = core(&server).await;
    let err = client
        .get_pull_zone(7, &GetPullZoneQuery::default())
        .await
        .unwrap_err();
    assert!(err.is_not_found());

    let err = client
        .update_pull_zone(8, &PullZoneSettingsModel::default())
        .await
        .unwrap_err();
    let api = err.api().unwrap();
    assert_eq!(api.status, 400);
    assert_eq!(api.field.as_deref(), Some("OriginUrl"));
    assert_eq!(api.message, "Invalid origin");
}

#[tokio::test]
async fn path_parameters_are_encoded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/library/9/videos/a%2Fb%20c"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "guid": "a/b c" })))
        .expect(1)
        .mount(&server)
        .await;

    let stream =
        StreamClient::with_config("library-key", Config::new().base_url(server.uri())).unwrap();
    let video = stream.get_video(9, "a/b c").await.unwrap();
    assert_eq!(video.guid, "a/b c");
}

#[tokio::test]
async fn stream_create_and_upload_video() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/library/9/videos"))
        .and(header("AccessKey", "library-key"))
        .and(body_json(json!({ "title": "Intro" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "guid": "vid-1", "title": "Intro", "videoLibraryId": 9, "status": 0
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/library/9/videos/vid-1"))
        .and(header("Content-Type", "application/octet-stream"))
        .and(body_bytes(b"video-bytes".to_vec()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "success": true, "message": "OK", "statusCode": 200 })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let stream =
        StreamClient::with_config("library-key", Config::new().base_url(server.uri())).unwrap();
    let video = stream
        .create_video(
            9,
            &CreateVideoModel {
                title: "Intro".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(video.guid, "vid-1");
    assert_eq!(video.status, VideoModelStatus::Created);

    let status = stream
        .upload_video(
            9,
            &video.guid,
            &UploadVideoQuery::default(),
            &b"video-bytes"[..],
        )
        .await
        .unwrap();
    assert_eq!(status.success, Some(true));
}

#[tokio::test]
async fn stream_error_message_is_extracted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "success": false, "message": "Library not found", "statusCode": 400
        })))
        .mount(&server)
        .await;
    let stream =
        StreamClient::with_config("library-key", Config::new().base_url(server.uri())).unwrap();
    let err = stream.get_video(1, "x").await.unwrap_err();
    assert_eq!(err.api().unwrap().message, "Library not found");
}

#[tokio::test]
async fn containers_rfc7807_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps"))
        .and(query_param("limit", "5"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "title": "Validation failed",
            "status": 422,
            "detail": "limit is too small",
            "errors": [{ "field": "limit", "message": "must be >= 10" }]
        })))
        .mount(&server)
        .await;

    let client =
        ContainersClient::with_config("account-key", Config::new().base_url(server.uri())).unwrap();
    let query = ListApplicationsQuery {
        limit: Some(5),
        ..Default::default()
    };
    let err = client.list_applications(&query).await.unwrap_err();
    let api = err.api().unwrap();
    assert_eq!(api.message, "limit is too small");
    assert_eq!(api.validation_errors[0].field.as_deref(), Some("limit"));
}

#[tokio::test]
async fn stream_live_streams() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/library/9/live"))
        .and(body_json(json!({ "title": "Launch", "dvrEnabled": true })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "guid": "live-1", "title": "Launch" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let stream =
        StreamClient::with_config("library-key", Config::new().base_url(server.uri())).unwrap();
    let body = bunnysdk::stream::CreateLiveStreamModel {
        title: Some("Launch".into()),
        dvr_enabled: Some(true),
        ..Default::default()
    };
    let live = stream.create_live_stream(9, &body).await.unwrap();
    assert_eq!(live.guid, "live-1");
}

#[tokio::test]
async fn partner_list_customers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/partner/customers"))
        .and(header("AccessKey", "partner-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{}])))
        .expect(1)
        .mount(&server)
        .await;
    let partner = bunnysdk::partner::PartnerClient::with_config(
        "partner-key",
        Config::new().base_url(server.uri()),
    )
    .unwrap();
    assert_eq!(partner.list_customers().await.unwrap().len(), 1);
}

#[tokio::test]
async fn paged_list_accepts_a_plain_array() {
    // `page` defaults to 0, where bunny.net answers with a plain array.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/videolibrary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "Id": 5, "Name": "lib" }])))
        .mount(&server)
        .await;
    let page = core(&server)
        .await
        .list_video_libraries(&Default::default())
        .await
        .unwrap();
    let items = page.items.unwrap();
    assert_eq!(items[0].id, Some(5));
}

#[tokio::test]
async fn non_list_decode_errors_are_not_masked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pullzone/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Items": [], "Id": "x" })))
        .mount(&server)
        .await;
    let err = core(&server)
        .await
        .get_pull_zone(1, &Default::default())
        .await
        .unwrap_err();
    assert!(matches!(err, bunnysdk::Error::Decode { .. }), "{err:?}");
}

#[tokio::test]
async fn optional_result_when_the_api_may_answer_without_a_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/compute/script/1/secrets"))
        .respond_with(ResponseTemplate::new(204))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/compute/script/1/secrets"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Id": 3, "Name": "TOKEN" })))
        .mount(&server)
        .await;

    let compute = bunnysdk::compute::ComputeClient::with_config(
        "account-key",
        Config::new().base_url(server.uri()),
    )
    .unwrap();
    let body = bunnysdk::compute::EdgeScriptUpsertSecretModel {
        name: Some("TOKEN".into()),
        secret: Some("s3cret".into()),
    };
    assert_eq!(compute.upsert_secret(1, &body).await.unwrap(), None);
    let created = compute.upsert_secret(1, &body).await.unwrap().unwrap();
    assert_eq!(created.id, Some(3));
}

#[tokio::test]
async fn csv_export_is_returned_as_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/shield/event-logs/7/export"))
        .and(header("Accept", "text/csv"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("time,ip\n1,1.2.3.4\n", "text/csv"))
        .expect(1)
        .mount(&server)
        .await;
    let shield = bunnysdk::shield::ShieldClient::with_config(
        "account-key",
        Config::new().base_url(server.uri()),
    )
    .unwrap();
    let csv = shield
        .export_event_logs(7, &Default::default())
        .await
        .unwrap();
    assert!(csv.starts_with("time,ip"));
}

#[tokio::test]
async fn undocumented_get_bodies_are_returned_as_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/partner/customer/u1/billing/9/pdf"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"%PDF-1.7".to_vec(), "application/pdf"),
        )
        .mount(&server)
        .await;
    let partner = bunnysdk::partner::PartnerClient::with_config(
        "partner-key",
        Config::new().base_url(server.uri()),
    )
    .unwrap();
    let pdf = partner.download_invoice_pdf("u1", 9).await.unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn every_client_has_a_default_base_url() {
    assert_eq!(bunnysdk::core::BASE_URL, "https://api.bunny.net");
    assert_eq!(bunnysdk::stream::BASE_URL, "https://video.bunnycdn.com");
    assert_eq!(bunnysdk::containers::BASE_URL, "https://api.bunny.net/mc");
    assert_eq!(
        bunnysdk::database::BASE_URL,
        "https://api.bunny.net/database"
    );
    assert!(bunnysdk::shield::ShieldClient::new("key").is_ok());
    assert!(bunnysdk::compute::ComputeClient::new("key").is_ok());
    assert!(bunnysdk::origin_errors::OriginErrorsClient::new("key").is_ok());
    assert_eq!(bunnysdk::partner::BASE_URL, "https://api.bunny.net");
    // A required query parameter defaults to the spec's minimum, not 0.
    assert_eq!(bunnysdk::database::ListDatabasesQuery::default().page, 1);
    assert!(bunnysdk::database::DatabaseClient::new("").is_err());
}
