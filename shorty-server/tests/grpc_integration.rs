//! Интеграционные тесты gRPC-сервиса.
//!
//! Запуск: `cargo test --test grpc_integration -- --ignored`
//! Требует запущенного gRPC-сервера на 127.0.0.1:50051 (или другом порту).

use shorty_server::auth::{AuthConfig, Claims, create_token, load_keys};
use tonic::Request;

use shorty_server::grpc::shorty::shorty_service_client::ShortyServiceClient;
use shorty_server::grpc::shorty::{
    CreateLinkRequest, GetLinkRequest, ListLinksRequest, StreamLinksRequest,
};

/// Адрес gRPC-сервера для тестов
const GRPC_ADDR: &str = "http://127.0.0.1:50051";

/// Получить валидный JWT токен для тестов
fn test_token() -> String {
    let config = AuthConfig::default();
    let (encoding_key, _) = load_keys(&config).unwrap();

    let claims = Claims::new(
        "test-user".to_string(),
        "admin".to_string(),
        config.issuer.clone(),
        config.audience.clone(),
        300,
    );

    create_token(&claims, encoding_key).unwrap()
}

/// Создать клиент
async fn create_client() -> ShortyServiceClient<tonic::transport::Channel> {
    ShortyServiceClient::connect(GRPC_ADDR.to_string())
        .await
        .expect("failed to connect to gRPC server")
}

/// Добавить токен авторизации в metadata запроса
fn inject_token<T>(mut request: Request<T>, token: &str) -> Request<T> {
    request.metadata_mut().insert(
        "authorization",
        tonic::metadata::MetadataValue::try_from(format!("Bearer {}", token)).unwrap(),
    );
    request
}

#[tokio::test]
#[ignore = "requires running gRPC server"]
async fn grpc_get_link_not_found() {
    let mut client = create_client().await;
    let token = test_token();

    let request = Request::new(GetLinkRequest {
        code: "nonexistent-code-12345".to_string(),
    });
    let request = inject_token(request, &token);

    let response = client.get_link(request).await;
    assert!(response.is_err());
    assert_eq!(response.unwrap_err().code(), tonic::Code::NotFound);
}

#[tokio::test]
#[ignore = "requires running gRPC server"]
async fn grpc_create_link_success() {
    let mut client = create_client().await;
    let token = test_token();

    let request = Request::new(CreateLinkRequest {
        target_url: "https://example.com/grpc-test".to_string(),
        custom_code: Some("grpc-test".to_string()),
        ttl_seconds: Some(3600),
    });
    let request = inject_token(request, &token);

    let response = client.create_link(request).await;
    assert!(response.is_ok());

    let link = response.unwrap().into_inner();
    assert_eq!(link.code, "grpc-test");
    assert_eq!(link.target_url, "https://example.com/grpc-test");
}

#[tokio::test]
#[ignore = "requires running gRPC server"]
async fn grpc_list_links_success() {
    let mut client = create_client().await;
    let token = test_token();

    let request = Request::new(ListLinksRequest {
        page_size: 10,
        page_token: None,
    });
    let request = inject_token(request, &token);

    let response = client.list_links(request).await;
    assert!(response.is_ok());

    let list = response.unwrap().into_inner();
    assert!(list.links.len() <= 10);
}

#[tokio::test]
#[ignore = "requires running gRPC server"]
async fn grpc_stream_links_success() {
    let mut client = create_client().await;
    let token = test_token();

    let request = Request::new(StreamLinksRequest { batch_size: 5 });
    let request = inject_token(request, &token);

    let mut stream = client.stream_links(request).await.unwrap().into_inner();

    let mut count = 0;
    while let Some(result) = stream.message().await.unwrap() {
        count += 1;
        if count >= 3 {
            break;
        }
        assert!(result.link.is_some());
    }
    assert!(count > 0, "stream should return at least one event");
}

#[tokio::test]
#[ignore = "requires running gRPC server"]
async fn grpc_unauthorized_without_token() {
    let mut client = create_client().await;

    let request = Request::new(GetLinkRequest {
        code: "test".to_string(),
    });

    let response = client.get_link(request).await;
    assert!(response.is_err());
    assert_eq!(response.unwrap_err().code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
#[ignore = "requires running gRPC server"]
async fn grpc_unauthorized_with_invalid_token() {
    let mut client = create_client().await;

    let request = Request::new(GetLinkRequest {
        code: "test".to_string(),
    });
    let request = inject_token(request, "invalid-token");

    let response = client.get_link(request).await;
    assert!(response.is_err());
    assert_eq!(response.unwrap_err().code(), tonic::Code::Unauthenticated);
}
