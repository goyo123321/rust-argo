use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{any, get},
    Router,
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
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

        let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
        let mut target_stream = target_stream;

        if let Err(e) = tokio::io::copy_bidirectional(&mut upgraded, &mut target_stream).await {
            error!("WebSocket 双向拷贝错误: {}", e);
        }
    });

    Ok(Response::new(axum::body::Body::empty()))
}

async fn handle_http_proxy(req: Request<axum::body::Body>, target_port: &str) -> Response {
    let client: Client<HttpConnector, axum::body::Body> = Client::builder(TokioExecutor::new()).build_http();

    let target_host = format!("localhost:{}", target_port);
    let uri = req.uri();
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("");
    let target_uri = format!("http://{}{}", target_host, path_and_query)
        .parse::<hyper::Uri>()
        .unwrap();

    let mut builder = hyper::Request::builder()
        .method(req.method())
        .uri(target_uri);

    let headers = req.headers();
    for (name, value) in headers.iter() {
        if name != http::header::HOST {
            builder = builder.header(name, value);
        }
    }
    builder = builder.header(http::header::HOST, &target_host);

    let hyper_req = match builder.body(req.into_body()) {
        Ok(req) => req,
        Err(e) => {
            error!("构造代理请求失败: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to build request").into_response();
        }
    };

    match client.request(hyper_req).await {
        Ok(hyper_resp) => {
            // 使用显式类型构建响应
            let status = StatusCode::from_u16(hyper_resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut builder = http::response::Builder::new().status(status);
            *builder.headers_mut().unwrap() = hyper_resp.headers().clone();
            let body = axum::body::Body::from_stream(hyper_resp.into_body());
            builder.body(body).unwrap()
        }
        Err(e) => {
            error!("HTTP代理错误: {}", e);
            (StatusCode::BAD_GATEWAY, "Proxy error").into_response()
        }
    }
}
