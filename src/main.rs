mod config;
mod log;
mod process;
mod proxy;
mod server;
mod util;

use crate::config::Config;
use crate::log::init_log;
use crate::process::ProcessManager;
use crate::util::{get_architecture, AppFiles, download_with_limit, get_isp, is_proxy_link};
use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde_json::json;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::signal;
use tokio::time::{sleep, Duration};

lazy_static::lazy_static! {
    static ref CONFIG: Config = Config::from_env();
    static ref APP_FILES: AppFiles = AppFiles::new(&CONFIG.file_path);
    static ref PROC_MGR: Arc<ProcessManager> = Arc::new(ProcessManager::new());
    pub static ref SUBSCRIPTION: tokio::sync::RwLock<String> = tokio::sync::RwLock::new(String::new());
}

// 全局统计信息
static WS_CONNECTIONS: AtomicI64 = AtomicI64::new(0);
static TOTAL_BYTES: AtomicI64 = AtomicI64::new(0);

#[tokio::main]
async fn main() -> Result<()> {
    init_log();
    log_info!("配置初始化完成");
    log_info!("UUID: {}", CONFIG.uuid);
    log_info!("Argo端口: {}", CONFIG.argo_port);
    log_info!("HTTP端口: {}", CONFIG.port);

    tokio::fs::create_dir_all(&CONFIG.file_path).await?;
    cleanup_old().await;

    generate_xray_config().await;
    argo_type().await;

    // 启动代理服务器（Argo端口）
    let proxy_handle = tokio::spawn(proxy::start_proxy_server());

    // 启动内部HTTP服务器
    let http_handle = tokio::spawn(server::start_http_server());

    // 启动主流程（下载、运行子进程等）
    let main_handle = tokio::spawn(start_main_process());

    // 信号处理
    shutdown_signal().await;
    log_info!("收到关闭信号，正在清理...");

    // 停止所有子进程
    PROC_MGR.stop_all().await;

    // 清理临时文件
    cleanup_files_on_exit().await;

    log_info!("程序退出");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn cleanup_old() {
    delete_nodes().await;
    // 清空目录内容，但保留目录本身
    if let Ok(mut entries) = tokio::fs::read_dir(&CONFIG.file_path).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                let _ = tokio::fs::remove_dir_all(path).await;
            } else {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
    }
    // 确保目录存在
    let _ = tokio::fs::create_dir_all(&CONFIG.file_path).await;
}

async fn delete_nodes() {
    if CONFIG.upload_url.is_empty() {
        return;
    }
    let sub_path = APP_FILES.sub();
    if let Ok(data) = tokio::fs::read_to_string(&sub_path).await {
        if let Ok(decoded) = BASE64.decode(data.trim()) {
            if let Ok(text) = String::from_utf8(decoded) {
                let nodes: Vec<String> = text
                    .lines()
                    .filter(|line| is_proxy_link(line))
                    .map(String::from)
                    .collect();
                if !nodes.is_empty() {
                    let json = serde_json::json!({ "nodes": nodes });
                    let client = reqwest::Client::new();
                    let url = format!("{}/api/delete-nodes", CONFIG.upload_url);
                    if let Err(e) = client.post(&url).json(&json).send().await {
                        log_warn!("删除节点失败: {}", e);
                    }
                }
            }
        }
    }
}

async fn generate_xray_config() {
    let inbounds = generate_inbounds();
    let outbounds = generate_outbounds();

    let config = json!({
        "log": {
            "access": "/dev/null",
            "error": "/dev/null",
            "loglevel": "none"
        },
        "dns": {
            "servers": [
                "https+local://8.8.8.8/dns-query",
                "https+local://1.1.1.1/dns-query",
                "8.8.8.8",
                "1.1.1.1"
            ],
            "queryStrategy": "UseIP",
            "disableCache": false
        },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "routing": {
            "domainStrategy": "IPIfNonMatch",
            "rules": []
        }
    });

    let data = serde_json::to_string_pretty(&config).unwrap();
    if let Err(e) = tokio::fs::write(APP_FILES.config(), data).await {
        log_error!("写入配置文件失败: {}", e);
    } else {
        log_info!("Xray配置文件生成完成");
    }
}

fn generate_inbounds() -> Vec<serde_json::Value> {
    let uuid = &CONFIG.uuid;
    vec![
        json!({
            "port": 3001,
            "protocol": "vless",
            "settings": {
                "clients": [{"id": uuid, "flow": "xtls-rprx-vision"}],
                "decryption": "none",
                "fallbacks": [
                    {"dest": 3002},
                    {"path": "/vless-argo", "dest": 3003},
                    {"path": "/vmess-argo", "dest": 3004},
                    {"path": "/trojan-argo", "dest": 3005}
                ]
            },
            "streamSettings": {"network": "tcp"}
        }),
        json!({
            "port": 3002,
            "listen": "127.0.0.1",
            "protocol": "vless",
            "settings": {
                "clients": [{"id": uuid}],
                "decryption": "none"
            },
            "streamSettings": {"network": "tcp", "security": "none"}
        }),
        json!({
            "port": 3003,
            "listen": "127.0.0.1",
            "protocol": "vless",
            "settings": {
                "clients": [{"id": uuid, "level": 0}],
                "decryption": "none"
            },
            "streamSettings": {
                "network": "ws",
                "security": "none",
                "wsSettings": {"path": "/vless-argo"}
            },
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic"],
                "metadataOnly": false
            }
        }),
        json!({
            "port": 3004,
            "listen": "127.0.0.1",
            "protocol": "vmess",
            "settings": {
                "clients": [{"id": uuid, "alterId": 0}]
            },
            "streamSettings": {
                "network": "ws",
                "wsSettings": {"path": "/vmess-argo"}
            },
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic"],
                "metadataOnly": false
            }
        }),
        json!({
            "port": 3005,
            "listen": "127.0.0.1",
            "protocol": "trojan",
            "settings": {
                "clients": [{"password": uuid}]
            },
            "streamSettings": {
                "network": "ws",
                "security": "none",
                "wsSettings": {"path": "/trojan-argo"}
            },
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic"],
                "metadataOnly": false
            }
        }),
    ]
}

fn generate_outbounds() -> Vec<serde_json::Value> {
    vec![
        json!({
            "protocol": "freedom",
            "tag": "direct",
            "settings": {"domainStrategy": "UseIP"}
        }),
        json!({
            "protocol": "blackhole",
            "tag": "block",
            "settings": {}
        }),
    ]
}

async fn argo_type() {
    if CONFIG.argo_auth.is_empty() || CONFIG.argo_domain.is_empty() {
        log_info!("ARGO_DOMAIN 或 ARGO_AUTH 为空，使用快速隧道");
        return;
    }
    if CONFIG.argo_auth.contains("TunnelSecret") {
        if let Ok(tunnel_config) = serde_json::from_str::<serde_json::Value>(&CONFIG.argo_auth) {
            let _ = tokio::fs::write(APP_FILES.tunnel_json(), &CONFIG.argo_auth).await;
            let tunnel_id = tunnel_config["TunnelID"].as_str().unwrap_or("");
            let yaml = format!(
                r#"tunnel: {}
credentials-file: {}
protocol: http2

ingress:
  - hostname: {}
    service: http://localhost:{}
    originRequest:
      noTLSVerify: true
  - service: http_status:404
"#,
                tunnel_id,
                APP_FILES.tunnel_json(),
                CONFIG.argo_domain,
                CONFIG.argo_port
            );
            let _ = tokio::fs::write(APP_FILES.tunnel_yml(), yaml).await;
            log_info!("隧道YAML配置生成成功");
        } else {
            log_error!("解析隧道配置失败");
        }
    } else {
        log_info!("ARGO_AUTH 不是TunnelSecret格式，使用token连接隧道");
    }
}

async fn start_main_process() {
    log_info!("开始服务器初始化...");
    download_files_and_run().await;
    log_info!("等待隧道启动...");
    sleep(Duration::from_secs(5)).await;
    extract_domains().await;
    add_visit_task().await;
    start_monitor_script().await;
    log_info!("服务器初始化完成");
}

async fn download_files_and_run() {
    let arch = get_architecture();
    let base_url = if arch == "arm" {
        "https://arm64.ssss.nyc.mn/"
    } else {
        "https://amd64.ssss.nyc.mn/"
    };

    let mut downloads = Vec::new();
    downloads.push(("web".to_string(), APP_FILES.web(), format!("{}web", base_url)));
    downloads.push(("bot".to_string(), APP_FILES.bot(), format!("{}bot", base_url)));

    if !CONFIG.nezha_server.is_empty() && !CONFIG.nezha_key.is_empty() {
        if !CONFIG.nezha_port.is_empty() {
            downloads.push(("agent".to_string(), APP_FILES.npm(), format!("{}agent", base_url)));
        } else {
            downloads.push(("v1".to_string(), APP_FILES.php(), format!("{}v1", base_url)));
        }
    }

    download_with_limit(downloads, 3).await;
    run_nezha().await;
    run_xray().await;
    run_cloudflared().await;
}

async fn run_nezha() {
    if CONFIG.nezha_server.is_empty() || CONFIG.nezha_key.is_empty() {
        log_info!("哪吒监控变量为空，跳过运行");
        return;
    }

    if CONFIG.nezha_port.is_empty() {
        // 哪吒 v1 客户端 (使用 php 文件)
        let port = CONFIG.nezha_server.split(':').nth(1).unwrap_or("443");
        let tls_ports = ["443", "8443", "2096", "2087", "2083", "2053"];
        let nezha_tls = if tls_ports.contains(&port) { "true" } else { "false" };

        let yaml = format!(
            r#"client_secret: {}
debug: false
disable_auto_update: true
disable_command_execute: false
disable_force_update: true
disable_nat: false
disable_send_query: false
gpu: false
insecure_tls: true
ip_report_period: 1800
report_delay: 4
server: {}
skip_connection_count: true
skip_procs_count: true
temperature: false
tls: {}
use_gitee_to_upgrade: false
use_ipv6_country_code: false
uuid: {}"#,
            CONFIG.nezha_key, CONFIG.nezha_server, nezha_tls, CONFIG.uuid
        );

        let _ = tokio::fs::write(APP_FILES.nezha_config(), yaml).await;
        if let Err(e) = PROC_MGR
            .start(
                "nezha",
                APP_FILES.php(),
                vec!["-c".to_string(), APP_FILES.nezha_config()],
                false,
            )
            .await
        {
            log_error!("运行哪吒失败: {}", e);
        }
        sleep(Duration::from_secs(1)).await;
    } else {
        // 哪吒 agent (使用 npm 文件)
        let mut args = vec![
            "-s".to_string(),
            format!("{}:{}", CONFIG.nezha_server, CONFIG.nezha_port),
            "-p".to_string(),
            CONFIG.nezha_key.clone(),
        ];
        let tls_ports = ["443", "8443", "2096", "2087", "2083", "2053"];
        if tls_ports.contains(&CONFIG.nezha_port.as_str()) {
            args.push("--tls".to_string());
        }
        args.extend_from_slice(&[
            "--disable-auto-update".to_string(),
            "--report-delay".to_string(),
            "4".to_string(),
            "--skip-conn".to_string(),
            "--skip-procs".to_string(),
        ]);
        if let Err(e) = PROC_MGR.start("nezha", APP_FILES.npm(), args, false).await {
            log_error!("运行哪吒失败: {}", e);
        }
        sleep(Duration::from_secs(1)).await;
    }
}

async fn run_xray() {
    if let Err(e) = PROC_MGR
        .start(
            "xray",
            APP_FILES.web(),
            vec!["-c".to_string(), APP_FILES.config()],
            false,
        )
        .await
    {
        log_error!("运行Xray失败: {}", e);
    } else {
        sleep(Duration::from_secs(1)).await;
    }
}

async fn run_cloudflared() {
    if !tokio::fs::try_exists(APP_FILES.bot()).await.unwrap_or(false) {
        log_error!("cloudflared文件不存在");
        return;
    }

    let mut args = vec![
        "tunnel".to_string(),
        "--edge-ip-version".to_string(),
        "auto".to_string(),
        "--no-autoupdate".to_string(),
        "--protocol".to_string(),
        "http2".to_string(),
    ];

    if !CONFIG.argo_auth.is_empty() && !CONFIG.argo_domain.is_empty() {
        if CONFIG.argo_auth.len() >= 120 && CONFIG.argo_auth.len() <= 250 {
            args.push("run".to_string());
            args.push("--token".to_string());
            args.push(CONFIG.argo_auth.clone());
        } else if CONFIG.argo_auth.contains("TunnelSecret") {
            // 等待配置文件生成
            for _ in 0..10 {
                if tokio::fs::try_exists(APP_FILES.tunnel_yml()).await.unwrap_or(false) {
                    break;
                }
                sleep(Duration::from_secs(1)).await;
            }
            args.push("--config".to_string());
            args.push(APP_FILES.tunnel_yml());
            args.push("run".to_string());
        } else {
            args.push("--logfile".to_string());
            args.push(APP_FILES.boot_log());
            args.push("--loglevel".to_string());
            args.push("info".to_string());
            args.push("--url".to_string());
            args.push(format!("http://localhost:{}", CONFIG.argo_port));
        }
    } else {
        args.push("--logfile".to_string());
        args.push(APP_FILES.boot_log());
        args.push("--loglevel".to_string());
        args.push("info".to_string());
        args.push("--url".to_string());
        args.push(format!("http://localhost:{}", CONFIG.argo_port));
    }

    if let Err(e) = PROC_MGR.start("cloudflared", APP_FILES.bot(), args, true).await {
        log_error!("运行cloudflared失败: {}", e);
    }
}

async fn extract_domains() {
    if !CONFIG.argo_auth.is_empty() && !CONFIG.argo_domain.is_empty() {
        log_info!("使用固定域名: {}", CONFIG.argo_domain);
        generate_links(&CONFIG.argo_domain).await;
        return;
    }

    if let Ok(data) = tokio::fs::read_to_string(APP_FILES.boot_log()).await {
        for line in data.lines() {
            if line.contains("trycloudflare.com") {
                if let Some(start) = line.find("https://").or_else(|| line.find("http://")) {
                    let remaining = &line[start..];
                    let end = remaining.find(' ').unwrap_or(remaining.len());
                    let url = &remaining[..end];
                    let domain = url
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .trim_end_matches('/');
                    log_info!("找到临时域名: {}", domain);
                    generate_links(domain).await;
                    return;
                }
            }
        }
    }

    log_warn!("未找到域名，重新运行cloudflared");
    restart_cloudflared().await;
}

async fn restart_cloudflared() {
    PROC_MGR.stop("cloudflared").await;
    sleep(Duration::from_secs(3)).await;
    let _ = tokio::fs::remove_file(APP_FILES.boot_log()).await;
    run_cloudflared().await;
    sleep(Duration::from_secs(5)).await;
    extract_domains().await;
}

async fn generate_links(domain: &str) {
    let isp = get_isp().await;
    let node_name = if !CONFIG.name.is_empty() {
        format!("{}-{}", CONFIG.name, isp)
    } else {
        isp
    };

    let vmess_config = serde_json::json!({
        "v": "2",
        "ps": node_name,
        "add": CONFIG.cfip,
        "port": CONFIG.cfport,
        "id": CONFIG.uuid,
        "aid": "0",
        "scy": "none",
        "net": "ws",
        "type": "none",
        "host": domain,
        "path": "/vmess-argo?ed=2560",
        "tls": "tls",
        "sni": domain,
        "alpn": "",
        "fp": "firefox"
    });
    let vmess_json = serde_json::to_string(&vmess_config).unwrap();
    let vmess_base64 = BASE64.encode(vmess_json);

    let sub_txt = format!(
        r#"
vless://{}@{}:{}?encryption=none&security=tls&sni={}&fp=firefox&type=ws&host={}&path=%2Fvless-argo%3Fed%3D2560#{}

vmess://{}

trojan://{}@{}:{}?security=tls&sni={}&fp=firefox&type=ws&host={}&path=%2Ftrojan-argo%3Fed%3D2560#{}
"#,
        CONFIG.uuid,
        CONFIG.cfip,
        CONFIG.cfport,
        domain,
        domain,
        node_name,
        vmess_base64,
        CONFIG.uuid,
        CONFIG.cfip,
        CONFIG.cfport,
        domain,
        domain,
        node_name
    );

    *SUBSCRIPTION.write().await = sub_txt.clone();

    let encoded = BASE64.encode(&sub_txt);
    if let Err(e) = tokio::fs::write(APP_FILES.sub(), encoded).await {
        log_error!("保存订阅文件失败: {}", e);
    } else {
        log_info!("订阅文件已保存: {}", APP_FILES.sub());
    }

    upload_nodes().await;
}

async fn upload_nodes() {
    if CONFIG.upload_url.is_empty() {
        return;
    }
    if !CONFIG.project_url.is_empty() {
        let sub_url = format!("{}/{}", CONFIG.project_url, CONFIG.sub_path);
        let json = serde_json::json!({ "subscription": [sub_url] });
        let client = reqwest::Client::new();
        let url = format!("{}/api/add-subscriptions", CONFIG.upload_url);
        match client.post(&url).json(&json).send().await {
            Ok(resp) => {
                if resp.status() == 200 {
                    log_info!("订阅上传成功");
                } else if resp.status() == 400 {
                    log_info!("订阅已存在");
                } else {
                    log_warn!("订阅上传失败，状态码: {}", resp.status());
                }
            }
            Err(e) => log_warn!("订阅上传失败: {}", e),
        }
    } else {
        // 从 list.txt 读取节点上传
        if let Ok(data) = tokio::fs::read_to_string(APP_FILES.list()).await {
            let nodes: Vec<String> = data
                .lines()
                .filter(|line| is_proxy_link(line))
                .map(String::from)
                .collect();
            if !nodes.is_empty() {
                let json = serde_json::json!({ "nodes": nodes });
                let client = reqwest::Client::new();
                let url = format!("{}/api/add-nodes", CONFIG.upload_url);
                if let Err(e) = client.post(&url).json(&json).send().await {
                    log_warn!("节点上传失败: {}", e);
                } else {
                    log_info!("节点上传成功");
                }
            }
        }
    }
}

async fn add_visit_task() {
    if !CONFIG.auto_access || CONFIG.project_url.is_empty() {
        log_info!("跳过自动访问任务");
        return;
    }
    let json = serde_json::json!({ "url": CONFIG.project_url });
    let client = reqwest::Client::new();
    match client.post("https://oooo.serv00.net/add-url").json(&json).send().await {
        Ok(resp) => {
            if resp.status() == 200 {
                log_info!("自动访问任务添加成功");
            } else {
                log_warn!("添加自动访问任务失败，状态码: {}", resp.status());
            }
        }
        Err(e) => log_warn!("添加自动访问任务失败: {}", e),
    }
}

async fn start_monitor_script() {
    if CONFIG.monitor_key.is_empty() || CONFIG.monitor_server.is_empty() || CONFIG.monitor_url.is_empty() {
        log_info!("监控环境变量不完整，跳过监控脚本启动");
        return;
    }
    sleep(Duration::from_secs(10)).await;
    log_info!("开始下载并运行监控脚本...");
    if let Err(e) = download_monitor_script().await {
        log_error!("下载监控脚本失败: {}", e);
        return;
    }
    // 设置执行权限
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = tokio::fs::metadata(APP_FILES.monitor()).await {
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            let _ = tokio::fs::set_permissions(APP_FILES.monitor(), perms).await;
        }
    }
    run_monitor_script().await;
}

async fn download_monitor_script() -> Result<()> {
    let url = "https://raw.githubusercontent.com/mimaldq/cf-vps-monitor/main/cf-vps-monitor.sh";
    let response = reqwest::get(url).await?;
    let bytes = response.bytes().await?;
    tokio::fs::write(APP_FILES.monitor(), bytes).await?;
    Ok(())
}

async fn run_monitor_script() {
    let args = vec![
        "-i".to_string(),
        "-k".to_string(),
        CONFIG.monitor_key.clone(),
        "-s".to_string(),
        CONFIG.monitor_server.clone(),
        "-u".to_string(),
        CONFIG.monitor_url.clone(),
    ];
    log_info!("运行监控脚本: {} {}", APP_FILES.monitor(), args.join(" "));
    let mut cmd = tokio::process::Command::new(APP_FILES.monitor());
    cmd.args(&args);
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    match cmd.spawn() {
        Ok(mut child) => {
            log_info!("监控脚本启动成功");
            let _ = child.wait().await;
            log_warn!("监控脚本退出");
        }
        Err(e) => log_error!("运行监控脚本失败: {}", e),
    }
}

async fn cleanup_files_on_exit() {
    for f in APP_FILES.all_temp_files() {
        let _ = tokio::fs::remove_file(f).await;
    }
    log_info!("临时文件清理完成");
}
