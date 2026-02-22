use anyhow::Result;
use rand::Rng;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{error, info};

pub struct AppFiles {
    dir: PathBuf,
    random_prefix: String,
}

impl AppFiles {
    pub fn new(base_dir: &str) -> Self {
        let rand_prefix: String = (0..6)
            .map(|_| rand::thread_rng().gen_range('a'..='z'))
            .collect();
        Self {
            dir: PathBuf::from(base_dir),
            random_prefix: rand_prefix,
        }
    }

    fn path(&self, name: &str) -> String {
        self.dir
            .join(format!("{}-{}", self.random_prefix, name))
            .to_string_lossy()
            .to_string()
    }

    pub fn npm(&self) -> String {
        self.path("npm")
    }
    pub fn web(&self) -> String {
        self.path("web")
    }
    pub fn bot(&self) -> String {
        self.path("bot")
    }
    pub fn php(&self) -> String {
        self.path("php")
    }
    pub fn monitor(&self) -> String {
        self.dir.join("cf-vps-monitor.sh").to_string_lossy().to_string()
    }
    pub fn sub(&self) -> String {
        self.path("sub.txt")
    }
    pub fn list(&self) -> String {
        self.path("list.txt")
    }
    pub fn boot_log(&self) -> String {
        self.path("boot.log")
    }
    pub fn config(&self) -> String {
        self.path("config.json")
    }
    pub fn nezha_config(&self) -> String {
        self.path("config.yaml")
    }
    pub fn tunnel_json(&self) -> String {
        self.path("tunnel.json")
    }
    pub fn tunnel_yml(&self) -> String {
        self.path("tunnel.yml")
    }

    pub fn all_temp_files(&self) -> Vec<String> {
        vec![
            self.npm(),
            self.web(),
            self.bot(),
            self.php(),
            self.monitor(),
            self.sub(),
            self.list(),
            self.boot_log(),
            self.config(),
            self.nezha_config(),
            self.tunnel_json(),
            self.tunnel_yml(),
        ]
    }
}

pub async fn download_file(url: &str, dest: &str) -> Result<()> {
    let response = reqwest::get(url).await?;
    let bytes = response.bytes().await?;
    let mut file = tokio::fs::File::create(dest).await?;
    file.write_all(&bytes).await?;
    Ok(())
}

pub async fn download_with_limit(items: Vec<(String, String, String)>, limit: usize) {
    let semaphore = tokio::sync::Semaphore::new(limit);
    let mut handles = vec![];

    for (name, path, url) in items {
        let permit = semaphore.acquire_owned().await.unwrap();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            match download_file(&url, &path).await {
                Ok(_) => {
                    info!("下载 {} 成功", name);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(metadata) = tokio::fs::metadata(&path).await {
                            let mut perms = metadata.permissions();
                            perms.set_mode(0o755);
                            let _ = tokio::fs::set_permissions(&path, perms).await;
                        }
                    }
                }
                Err(e) => error!("下载 {} 失败: {}", name, e),
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }
}

pub fn get_architecture() -> &'static str {
    if cfg!(target_arch = "arm") || cfg!(target_arch = "aarch64") {
        "arm"
    } else {
        "amd"
    }
}

pub fn is_proxy_link(line: &str) -> bool {
    line.contains("vless://")
        || line.contains("vmess://")
        || line.contains("trojan://")
        || line.contains("hysteria2://")
        || line.contains("tuic://")
}

struct IspCache {
    value: String,
    time: Instant,
}

lazy_static::lazy_static! {
    static ref ISP_CACHE: Arc<Mutex<Option<IspCache>>> = Arc::new(Mutex::new(None));
}

pub async fn get_isp() -> String {
    let mut cache = ISP_CACHE.lock().await;
    if let Some(c) = cache.as_ref() {
        if c.time.elapsed() < Duration::from_secs(3600) {
            return c.value.clone();
        }
    }

    let client = reqwest::Client::new();
    if let Ok(resp) = client.get("https://ipapi.co/json/").send().await {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if let (Some(country), Some(org)) = (json["country_code"].as_str(), json["org"].as_str()) {
                let value = format!("{}_{}", country, org);
                *cache = Some(IspCache { value: value.clone(), time: Instant::now() });
                return value;
            }
        }
    }

    if let Ok(resp) = client.get("http://ip-api.com/json/").send().await {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            if json["status"] == "success" {
                if let (Some(country), Some(org)) = (json["countryCode"].as_str(), json["org"].as_str()) {
                    let value = format!("{}_{}", country, org);
                    *cache = Some(IspCache { value: value.clone(), time: Instant::now() });
                    return value;
                }
            }
        }
    }

    "Unknown".to_string()
}
