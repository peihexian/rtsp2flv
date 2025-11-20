mod config;
mod srs;
mod transcoder;
mod stream_manager;

use axum::{
    extract::{State, Json, FromRef},
    routing::{get, post},
    Router,
    response::{IntoResponse, Response},
    http::StatusCode,
};
use std::sync::Arc;
use tower_http::services::ServeDir;
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use crate::config::AppConfig;
use crate::srs::SrsClient;
use crate::stream_manager::StreamManager;
use serde::{Serialize, Deserialize};

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    srs: SrsClient,
    stream_manager: Arc<StreamManager>,
}

// 自定义应用错误类型，用于统一处理 HTTP 响应
struct AppError(anyhow::Error);

// 实现 IntoResponse 让 AppError 可以直接作为 Handler 的返回值
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("服务器内部错误: {}", self.0),
        )
            .into_response()
    }
}

// 允许从 anyhow::Error 转换
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

// API 鉴权提取器
struct AuthToken;

#[axum::async_trait]
impl<S> axum::extract::FromRequestParts<S> for AuthToken
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut axum::http::request::Parts, state: &S) -> Result<Self, Self::Rejection> {
        // 1. 尝试从 Header 获取 Authorization
        if let Some(auth_header) = parts.headers.get("Authorization") {
            if let Ok(token) = auth_header.to_str() {
                // 支持 "Bearer <token>" 或直接 "<token>"
                let token = token.trim_start_matches("Bearer ").trim();
                let app_state = AppState::from_ref(state);
                if app_state.config.api_keys.iter().any(|k| k == token) {
                    return Ok(AuthToken);
                }
            }
        }

        // 2. (可选) 尝试从 Query 参数获取 ?token=xxx
        // 这里为了简单暂不实现，强制使用 Header

        Err((StatusCode::UNAUTHORIZED, "无效的 API Token"))
    }
}

#[tokio::main]
async fn main() {
    // 初始化日志追踪
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "rtsp2flv=debug,tower_http=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 加载配置
    let config = match AppConfig::new() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!("加载配置失败: {}", e);
            return;
        }
    };

    // 初始化 SRS 客户端
    let srs_client = SrsClient::new(
        config.srs.api_url.clone(),
        config.srs.playback_url_template.clone()
    );

    // 初始化流管理器
    let stream_manager = Arc::new(StreamManager::new());

    let state = AppState {
        config: config.clone(),
        srs: srs_client,
        stream_manager,
    };

    // 设置路由
    let app = Router::new()
        .route("/api/streams", get(list_streams))
        .route("/api/play", post(play_stream))
        .route("/api/heartbeat", post(heartbeat))
        .nest_service("/", ServeDir::new("web"))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.server.port);
    tracing::info!("服务启动监听: {}", addr);
    
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// 获取流列表接口
async fn list_streams(State(state): State<AppState>) -> Json<Vec<crate::config::StreamConfig>> {
    Json(state.config.streams.clone())
}

#[derive(Deserialize)]
struct PlayRequest {
    name: String,
    url: Option<String>,
}

#[derive(Serialize)]
struct PlayResponse {
    playback_url: String,
}

/// 播放流接口
/// 接收流名称或自定义 URL，调用 SRS 接口，返回播放地址
async fn play_stream(
    State(state): State<AppState>,
    _: AuthToken, // 验证 Token
    Json(payload): Json<PlayRequest>,
) -> Result<Json<PlayResponse>, AppError> {
    let (name, rtsp_url) = if let Some(custom_url) = &payload.url {
        if !custom_url.is_empty() {
             // 1. 如果提供了 URL，直接使用（自定义播放模式）
            if !custom_url.to_lowercase().starts_with("rtsp://") {
                 return Err(anyhow::anyhow!("自定义地址必须以 rtsp:// 开头").into());
            }
            (payload.name.as_str(), custom_url.as_str())
        } else {
             // URL 字段存在但为空字符串，视为查找配置
             let stream_config = state.config.streams.iter()
                .find(|s| s.name == payload.name)
                .ok_or_else(|| anyhow::anyhow!("未找到名称为 '{}' 的流配置", payload.name))?;
            (stream_config.name.as_str(), stream_config.url.as_str())
        }
    } else {
        // 2. 如果没有提供 URL，从配置中查找
        let stream_config = state.config.streams.iter()
            .find(|s| s.name == payload.name)
            .ok_or_else(|| anyhow::anyhow!("未找到名称为 '{}' 的流配置", payload.name))?;
        (stream_config.name.as_str(), stream_config.url.as_str())
    };

    // 1. 获取 SRS 播放地址 (用于返回给前端)
    // 注意：这里我们仍然调用 srs.play_stream 主要是为了利用它的 URL 生成逻辑
    // 实际上 SRS 的 API 调用可能是不必要的，但保留也没坏处
    let playback_url = state.srs.play_stream(name, rtsp_url).await?;
    
    // 2. 构造推流地址 (RTMP)
    // 从配置的 API URL 中提取主机名，默认端口 1935
    let api_url = reqwest::Url::parse(&state.config.srs.api_url)
        .map_err(|e| anyhow::anyhow!("配置的 SRS API URL 无效: {}", e))?;
    
    let host = api_url.host_str().unwrap_or("127.0.0.1");
    
    let safe_name = name.replace(" ", "_").to_lowercase();
    let rtmp_url = format!("rtmp://{}:1935/live/{}", host, safe_name);

    // 3. 启动转码任务
    state.stream_manager.start_stream(name.to_string(), rtsp_url.to_string(), rtmp_url);
    
    Ok(Json(PlayResponse { playback_url }))
}

#[derive(Deserialize)]
struct HeartbeatRequest {
    name: String,
}

async fn heartbeat(
    State(state): State<AppState>,
    _: AuthToken, // 验证 Token
    Json(payload): Json<HeartbeatRequest>,
) -> StatusCode {
    if state.stream_manager.heartbeat(&payload.name) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}
