use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub upload_url: String,
    pub project_url: String,
    pub auto_access: bool,
    pub file_path: String,
    pub sub_path: String,
    pub port: String,
    pub argo_port: String,
    pub uuid: String,
    pub nezha_server: String,
    pub nezha_port: String,
    pub nezha_key: String,
    pub argo_domain: String,
    pub argo_auth: String,
    pub cfip: String,
    pub cfport: String,
    pub name: String,
    pub monitor_key: String,
    pub monitor_server: String,
    pub monitor_url: String,
}

impl Config {
    pub fn from_env() -> Self {
        let default_port = get_env("PORT", "3000");
        Self {
            upload_url: get_env("UPLOAD_URL", ""),
            project_url: get_env("PROJECT_URL", ""),
            auto_access: get_env("AUTO_ACCESS", "false") == "true",
            file_path: get_env("FILE_PATH", "./tmp"),
            sub_path: get_env("SUB_PATH", "sub"),
            port: get_env("SERVER_PORT", &default_port),
            argo_port: get_env("ARGO_PORT", "7860"),
            uuid: get_env("UUID", "e2cae6af-5cdd-fa48-4137-ad3e617fbab0"),
            nezha_server: get_env("NEZHA_SERVER", ""),
            nezha_port: get_env("NEZHA_PORT", ""),
            nezha_key: get_env("NEZHA_KEY", ""),
            argo_domain: get_env("ARGO_DOMAIN", ""),
            argo_auth: get_env("ARGO_AUTH", ""),
            cfip: get_env("CFIP", "cdns.doon.eu.org"),
            cfport: get_env("CFPORT", "443"),
            name: get_env("NAME", ""),
            monitor_key: get_env("MONITOR_KEY", ""),
            monitor_server: get_env("MONITOR_SERVER", ""),
            monitor_url: get_env("MONITOR_URL", ""),
        }
    }
}

fn get_env(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}