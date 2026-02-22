use axum::{
    extract::Request,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::net::SocketAddr;
use tower_http::services::ServeDir;
use tracing::info;
use crate::{CONFIG, SUBSCRIPTION};

pub async fn start_http_server() {
    let app = Router::new()
        .route(&format!("/{}", CONFIG.sub_path), get(subscription_handler))
        .fallback_service(get(index_handler));

    let addr = SocketAddr::from(([0, 0, 0, 0], CONFIG.port.parse().unwrap()));
    info!("HTTP服务运行在内部端口: {}", CONFIG.port);
    axum::serve(
        tokio::net::TcpListener::bind(&addr).await.unwrap(),
        app.into_make_service(),
    )
    .await
    .unwrap();
}

async fn subscription_handler() -> String {
    let sub = SUBSCRIPTION.read().await;
    BASE64.encode(sub.as_bytes())
}

async fn index_handler(req: Request<axum::body::Body>) -> impl IntoResponse {
    let path = req.uri().path();
    if path == "/" {
        if let Ok(content) = tokio::fs::read_to_string("index.html").await {
            return Html(content).into_response();
        }
        if let Ok(content) = tokio::fs::read_to_string("/app/index.html").await {
            return Html(content).into_response();
        }
        if let Ok(content) = tokio::fs::read_to_string("./index.html").await {
            return Html(content).into_response();
        }
        "Hello world!".into_response()
    } else {
        let serve_dir = ServeDir::new(".");
        match serve_dir.try_call(req).await {
            Ok(resp) => resp.into_response(),
            Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        }
    }
}
