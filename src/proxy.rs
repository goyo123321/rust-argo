use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{any, get},
    Router,
};
use hyper_util::rt::TokioIo;
use serde_json::json;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tracing::{error, info};
use scopeguard;

use crate::{CONFIG, PROC_MGR, WS_CONNECTIONS};

pub async fn start_proxy_server() {
    let app = Router::new()
        .route("/stats", get(handle_stats))
        .fallback(any(handle_proxy));

    let addr = SocketAddr::from(([0, 0, 0, 0], CONFIG.argo_port.parse().unwrap()));
    info!("代理服务器启动在端口: {}", CONFIG.argo_port);
    axum::serve(
        tokio::net::TcpListener::bind(&addr).await.unwrap(),
        app.into_make_service(),
    )
    .await
    .unwrap();
}

async fn handle_stats() -> impl IntoResponse {
    let processes = PROC_MGR.get_processes().await;
    let procs: Vec<_> = processes
        .into_iter()
        .map(|(name, pid, running, restart)| {
            json!({
                "name": name,
                "pid": pid,
                "running": running,
                "restart": restart,
            })
        })
        .collect();

    let mem = json!({
        "alloc": "unknown",
        "total_alloc": "unknown",
        "sys": "unknown",
        "num_gc": 0,
    });

    let stats = json!({
        "ws_connections": WS_CONNECTIONS.load(std::sync::atomic::Ordering::Relaxed),
        "total_bytes": 0,
        "goroutines": 0,
        "memory": mem,
        "processes": procs,
    });

    Json(stats)
}

async fn handle_proxy(req: Request<axum::body::Body>) -> Response {
    let path = req.uri().path().to_string();
    let is_websocket = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_lowercase() == "websocket")
        .unwrap_or(false);

    let target_port = if path.starts_with("/vless-argo")
        || path.starts_with("/vmess-argo")
        || path.starts_with("/trojan-argo")
        || path == "/vless"
        || path == "/vmess"
        || path == "/trojan"
    {
        "3001"
    } else {
        &CONFIG.port
    };

    if is_websocket {
        match handle_websocket(req, target_port).await {
            Ok(resp) => resp,
            Err(e) => {
                error!("WebSocket代理错误: {}", e);
                (StatusCode::BAD_GATEWAY, "WebSocket proxy failed").into_response()
            }
        }
    } else {
        handle_http_proxy(req, target_port).await
    }
}

async fn handle_websocket(req: Request<axum::body::Body>, target_port: &str) -> Result<Response, anyhow::Error> {
    WS_CONNECTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let upgraded = hyper::upgrade::on(req).await?;

    let target_addr = format!("localhost:{}", target_port);
    let target_stream = TcpStream::connect(target_addr).await?;

    tokio::spawn(async move {
        let _guard = scopeguard::guard((), |_| {
            WS_CONNECTIONS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });

        let mut upgraded = TokioIo::new(upgraded);
        let mut target_stream = target_stream; // 直接使用 TcpStream，它实现了 tokio::io::AsyncRead/Write

        if let Err(e) = tokio::io::copy_bidirectional(&mut upgraded, &mut target_stream).await {
            error!("WebSocket 双向拷贝错误: {}", e);
        }
    });

    Ok(Response::new(axum::body::Body::empty()))
}

async fn handle_http_proxy(req: Request<axum::body::Body>, target_port: &str) -> Response {
    let client = reqwest::Client::new();
    let target_host = format!("localhost:{}", target_port);

    let mut new_req = reqwest::Request::new(req.method().clone(), {
        let uri = req.uri();
        let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("");
        format!("http://{}{}", target_host, path_and_query).parse().unwrap()
    });

    *new_req.headers_mut() = req.headers().clone();

    let body = req.into_body();
    let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            error!("读取body失败: {}", e);
            return (StatusCode::BAD_REQUEST, "Invalid body").into_response();
        }
    };
    *new_req.body_mut() = Some(bytes.into());

    match client.execute(new_req).await {
        Ok(resp) => {
            let mut builder = Response::builder().status(resp.status());
            for (k, v) in resp.headers() {
                builder = builder.header(k, v);
            }
            let body = axum::body::Body::from_stream(resp.bytes_stream());
            builder.body(body).unwrap()
        }
        Err(e) => {
            error!("HTTP代理错误: {}", e);
            (StatusCode::BAD_GATEWAY, "Proxy error").into_response()
        }
    }
}
