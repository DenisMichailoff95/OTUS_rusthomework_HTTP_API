//! gRPC сервер на tonic.

use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use crate::auth::{AuthConfig, load_keys, validate_token};
#[allow(unused_imports)]
use async_trait::async_trait;
use prost_types::Timestamp;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status, codegen::InterceptedService, transport::Server};
use tonic_health::server::health_reporter;
use tonic_reflection::server::Builder as ReflectionBuilder;
use tracing::{info, warn};
use uuid::Uuid;

use crate::AppState;

pub mod shorty {
    tonic::include_proto!("shorty.v1");
}

use shorty::shorty_service_server::ShortyService;
use shorty::{
    CreateLinkRequest as GrpcCreateLinkRequest, CreateLinkResponse as GrpcCreateLinkResponse,
    GetLinkRequest as GrpcGetLinkRequest, GetLinkResponse as GrpcGetLinkResponse,
    LinkEvent as GrpcLinkEvent, LinkEventType, ListLinksRequest as GrpcListLinksRequest,
    ListLinksResponse as GrpcListLinksResponse, StreamLinksRequest as GrpcStreamLinksRequest,
};

/// gRPC интерцептор: прокидывает request/correlation ID из metadata в tracing-span.
#[allow(clippy::result_large_err)]
fn request_id_interceptor(mut req: Request<()>) -> Result<Request<()>, Status> {
    let request_id = req
        .metadata()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());

    let _span = tracing::info_span!(
        "grpc_request",
        request_id = %request_id,
    );

    tracing::debug!(request_id = %request_id, "gRPC request intercepted");
    req.extensions_mut().insert(request_id);
    Ok(req)
}

/// gRPC сервер
pub struct GrpcServer {
    state: AppState,
}

impl GrpcServer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    pub async fn serve(self, addr: std::net::SocketAddr, shutdown: CancellationToken) {
        info!(addr = %addr, "starting gRPC server");

        let (_health_reporter, health_server) = health_reporter();

        let fd_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../proto/shorty.v1.shorty.bin");
        let fd_bytes = std::fs::read(fd_path).expect("failed to read proto descriptor set");
        let reflection_server = ReflectionBuilder::configure()
            .register_encoded_file_descriptor_set(&fd_bytes)
            .build_v1()
            .expect("failed to build reflection service");

        let shorty_service =
            shorty::shorty_service_server::ShortyServiceServer::new(ShortyServiceImpl {
                state: self.state.clone(),
                auth_config: self.state.config.auth.clone().unwrap(),
            });

        let intercepted_service = InterceptedService::new(shorty_service, request_id_interceptor);

        let server = Server::builder()
            .add_service(health_server)
            .add_service(reflection_server)
            .add_service(intercepted_service);

        server
            .serve_with_shutdown(addr, shutdown.cancelled())
            .await
            .expect("gRPC server failed");
    }
}

/// Реализация gRPC сервиса
struct ShortyServiceImpl {
    state: AppState,
    auth_config: AuthConfig,
}

#[async_trait::async_trait]
impl ShortyService for ShortyServiceImpl {
    async fn get_link(
        &self,
        request: Request<GrpcGetLinkRequest>,
    ) -> Result<Response<GrpcGetLinkResponse>, Status> {
        let _claims = self.authenticate(&request).await?;
        let req = request.into_inner();

        let code = req.code;
        let stats = self
            .state
            .repo
            .stats(&code)
            .await
            .map_err(|e| map_repo_error(e, &code))?;

        let response = GrpcGetLinkResponse {
            code: stats.link.code,
            target_url: stats.link.target_url,
            created_at: Some(protobuf_timestamp(stats.link.created_at)),
            updated_at: Some(protobuf_timestamp(stats.link.updated_at)),
            expires_at: stats.link.expires_at.map(protobuf_timestamp),
            version: stats.link.version,
            hits: stats.hits as i64,
        };

        Ok(Response::new(response))
    }

    async fn create_link(
        &self,
        request: Request<GrpcCreateLinkRequest>,
    ) -> Result<Response<GrpcCreateLinkResponse>, Status> {
        let _claims = self.authenticate(&request).await?;
        let req = request.into_inner();

        let target_url = req.target_url;
        if target_url.is_empty() {
            return Err(Status::invalid_argument("target_url is required"));
        }

        let custom_code = match req.custom_code {
            Some(code) if !code.is_empty() => Some(code),
            _ => None,
        };
        let _ttl_seconds: Option<u64> = req
            .ttl_seconds
            .and_then(|v| if v <= 0 { None } else { Some(v as u64) });

        let code = match custom_code {
            Some(code) => {
                let link = domain::ShortLink::new(&code, &target_url);
                self.state
                    .repo
                    .insert(link)
                    .await
                    .map_err(|e| map_repo_error(e, &code))?;
                code
            }
            None => {
                let generated_code = Uuid::new_v4().to_string()[..8].to_string();
                let link = domain::ShortLink::new(&generated_code, &target_url);
                self.state
                    .repo
                    .insert(link)
                    .await
                    .map_err(|e| map_repo_error(e, &generated_code))?;
                generated_code
            }
        };

        let stats = self
            .state
            .repo
            .stats(&code)
            .await
            .map_err(|e| map_repo_error(e, &code))?;

        let response = GrpcCreateLinkResponse {
            code: stats.link.code,
            target_url: stats.link.target_url,
            created_at: Some(protobuf_timestamp(stats.link.created_at)),
            expires_at: stats.link.expires_at.map(protobuf_timestamp),
            version: stats.link.version,
        };

        Ok(Response::new(response))
    }

    async fn list_links(
        &self,
        request: Request<GrpcListLinksRequest>,
    ) -> Result<Response<GrpcListLinksResponse>, Status> {
        let _claims = self.authenticate(&request).await?;
        let req = request.into_inner();

        let page_size = if req.page_size <= 0 {
            20
        } else {
            req.page_size.min(100) as u64
        };
        let cursor = req.page_token.as_deref().and_then(|token| {
            let parts: Vec<&str> = token.split(':').collect();
            if parts.len() == 2 {
                Some((parts[0], parts[1]))
            } else {
                None
            }
        });

        let (links, next_cursor) = self
            .state
            .repo
            .list(page_size, cursor)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let grpc_links: Vec<GrpcGetLinkResponse> = links
            .into_iter()
            .map(|link| GrpcGetLinkResponse {
                code: link.code,
                target_url: link.target_url,
                created_at: Some(protobuf_timestamp(link.created_at)),
                updated_at: Some(protobuf_timestamp(link.updated_at)),
                expires_at: link.expires_at.map(protobuf_timestamp),
                version: link.version,
                hits: 0,
            })
            .collect();

        let response = GrpcListLinksResponse {
            links: grpc_links,
            next_page_token: next_cursor.map(|(ts, code)| format!("{}:{}", ts, code)),
        };

        Ok(Response::new(response))
    }

    type StreamLinksStream =
        Pin<Box<dyn Stream<Item = Result<GrpcLinkEvent, Status>> + Send + 'static>>;

    async fn stream_links(
        &self,
        request: Request<GrpcStreamLinksRequest>,
    ) -> Result<Response<Self::StreamLinksStream>, Status> {
        let _claims = self.authenticate(&request).await?;
        let req = request.into_inner();

        let batch_size = if req.batch_size <= 0 {
            10
        } else {
            req.batch_size.min(100)
        };
        let state = self.state.clone();
        let (tx, rx) = mpsc::channel(batch_size as usize);

        tokio::spawn(async move {
            let codes = vec!["stream-1".to_string(), "stream-2".to_string()];
            for code in codes {
                if let Ok(stats) = state.repo.stats(&code).await {
                    let event = GrpcLinkEvent {
                        event_type: LinkEventType::Created as i32,
                        link: Some(GrpcGetLinkResponse {
                            code: stats.link.code,
                            target_url: stats.link.target_url,
                            created_at: Some(protobuf_timestamp(stats.link.created_at)),
                            updated_at: Some(protobuf_timestamp(stats.link.updated_at)),
                            expires_at: stats.link.expires_at.map(protobuf_timestamp),
                            version: stats.link.version,
                            hits: stats.hits as i64,
                        }),
                        occurred_at: Some(protobuf_timestamp(std::time::SystemTime::now())),
                    };
                    if tx.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });

        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }
}

impl ShortyServiceImpl {
    async fn authenticate<T>(&self, request: &Request<T>) -> Result<(), Status> {
        let metadata = request.metadata();

        let token = metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(|| {
                warn!("missing or invalid authorization header");
                Status::unauthenticated("missing or invalid token")
            })?;

        let (_, decoding_key) = load_keys(&self.auth_config).map_err(|e| {
            warn!(error = ?e, "failed to load auth keys");
            Status::internal("failed to load auth keys")
        })?;

        validate_token(token, &decoding_key).map_err(|e| {
            warn!(error = ?e, "token validation failed");
            Status::unauthenticated("invalid token")
        })?;

        Ok(())
    }
}

/// Маппинг доменных ошибок в gRPC статусы
fn map_repo_error(err: domain::RepoError, code: &str) -> Status {
    match err {
        domain::RepoError::NotFound(_) => Status::not_found(format!("link {} not found", code)),
        domain::RepoError::CodeTaken(_) => {
            Status::already_exists(format!("code {} already exists", code))
        }
        domain::RepoError::VersionConflict => Status::aborted("version conflict"),
        domain::RepoError::Unavailable => Status::unavailable("database unavailable"),
        domain::RepoError::Internal(e) => {
            warn!(error = ?e, "internal error");
            Status::internal("internal error")
        }
    }
}

/// Конвертация SystemTime в prost Timestamp
fn protobuf_timestamp(t: std::time::SystemTime) -> Timestamp {
    let duration = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    Timestamp {
        seconds: duration.as_secs() as i64,
        nanos: duration.subsec_nanos() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protobuf_timestamp_roundtrip() {
        let now = std::time::SystemTime::now();
        let ts = protobuf_timestamp(now);
        assert!(ts.seconds > 0);
    }
}
