//! Logging client against a local mock server.

use bunnysdk::Config;
use bunnysdk::logging::{LogQuery, LoggingClient, RawLogQuery};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> LoggingClient {
    LoggingClient::with_config("account-key", Config::new().base_url(server.uri())).unwrap()
}

#[tokio::test]
async fn get_logs_sends_filters_and_parses_entries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/pullzones/1337/logs"))
        .and(header("AccessKey", "account-key"))
        .and(query_param("status", "4xx,5xx"))
        .and(query_param("cacheStatus", "MISS"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{
                "timestamp": "2026-05-08T14:32:11.265Z",
                "pullZoneId": 1337,
                "requestId": "648fd832",
                "cacheStatus": "MISS",
                "statusCode": 404,
                "bytesSent": 1095,
                "remoteIp": "163.172.53.0",
                "countryCode": "FI",
                "edgeLocation": "WA",
                "scheme": "https",
                "host": "example.b-cdn.net",
                "path": "/video.mp4",
                "url": "https://example.b-cdn.net/video.mp4",
                "asn": 15169
            }],
            "pagination": { "offset": 0, "limit": 50, "returned": 1, "hasMore": false },
            "query": { "pullZoneId": 1337 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let query = LogQuery {
        status: Some("4xx,5xx".into()),
        cache_status: Some("MISS".into()),
        limit: Some(50),
        ..Default::default()
    };
    let page = client(&server).get_logs(1337, &query).await.unwrap();
    assert_eq!(page.data.len(), 1);
    let entry = &page.data[0];
    assert_eq!(entry.status_code, 404);
    assert_eq!(entry.cache_status, "MISS");
    assert_eq!(entry.referer, None);
    assert_eq!(entry.asn, Some(15169));
    assert!(!page.pagination.has_more);
}

#[tokio::test]
async fn logging_errors_are_typed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "code": "logging_disabled", "message": "Logging is not enabled.", "details": [] }
        })))
        .mount(&server)
        .await;
    let err = client(&server)
        .get_logs(1, &LogQuery::default())
        .await
        .unwrap_err();
    assert!(err.is_not_found());
    assert_eq!(
        err.api().unwrap().error_key.as_deref(),
        Some("logging_disabled")
    );
}

#[tokio::test]
async fn raw_logs_are_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/03-07-26/1337.log"))
        .and(query_param("status", "4,5"))
        .respond_with(ResponseTemplate::new(200).set_body_string("HIT|200|1507167062421|412\n"))
        .mount(&server)
        .await;
    let query = RawLogQuery {
        status: Some("4,5".into()),
        ..Default::default()
    };
    let text = client(&server)
        .get_raw_logs(1337, "03-07-26", &query)
        .await
        .unwrap();
    assert!(text.starts_with("HIT|200|"));
}
