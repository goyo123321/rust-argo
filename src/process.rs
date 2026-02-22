use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

pub struct ManagedProcess {
    pub name: String,
    pub bin_path: String,
    pub args: Vec<String>,
    pub restart: bool,
    pub child: Option<tokio::process::Child>,
}

pub struct ProcessManager {
    procs: Arc<Mutex<HashMap<String, ManagedProcess>>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            procs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start(
        &self,
        name: &str,
        bin_path: String,
        args: Vec<String>,
        restart: bool,
    ) -> Result<()> {
        let mut procs = self.procs.lock().await;
        if procs.contains_key(name) {
            return Err(anyhow::anyhow!("进程 {} 已存在", name));
        }

        let mut cmd = Command::new(&bin_path);
        cmd.args(&args);
        cmd.kill_on_drop(true);
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());

        let child = cmd.spawn()?;
        info!("进程 {} (PID {}) 已启动", name, child.id().unwrap_or(0));

        let proc = ManagedProcess {
            name: name.to_string(),
            bin_path,
            args,
            restart,
            child: Some(child),
        };
        procs.insert(name.to_string(), proc);

        let procs_clone = self.procs.clone();
        let name_clone = name.to_string();
        tokio::spawn(async move {
            Self::monitor_process(procs_clone, name_clone).await;
        });

        Ok(())
    }

    async fn monitor_process(procs: Arc<Mutex<HashMap<String, ManagedProcess>>>, name: String) {
        loop {
            let mut proc_guard = procs.lock().await;
            let proc = match proc_guard.get_mut(&name) {
                Some(p) => p,
                None => break,
            };

            let mut child = match proc.child.take() {
                Some(c) => c,
                None => break,
            };

            drop(proc_guard);

            let status = child.wait().await;
            match status {
                Ok(exit) => {
                    if exit.success() {
                        info!("进程 {} 正常退出", name);
                    } else {
                        warn!("进程 {} 退出，状态码: {:?}", name, exit.code());
                    }
                }
                Err(e) => {
                    error!("进程 {} 等待错误: {}", name, e);
                }
            }

            let mut proc_guard = procs.lock().await;
            let proc = match proc_guard.get_mut(&name) {
                Some(p) => p,
                None => break,
            };

            if proc.restart {
                info!("进程 {} 将在5秒后重启", name);
                drop(proc_guard);
                sleep(Duration::from_secs(5)).await;

                let mut proc_guard = procs.lock().await;
                let proc = match proc_guard.get_mut(&name) {
                    Some(p) => p,
                    None => break,
                };

                let mut cmd = Command::new(&proc.bin_path);
                cmd.args(&proc.args);
                cmd.kill_on_drop(true);
                cmd.stdout(std::process::Stdio::inherit());
                cmd.stderr(std::process::Stdio::inherit());

                match cmd.spawn() {
                    Ok(new_child) => {
                        info!("进程 {} (PID {}) 重启成功", name, new_child.id().unwrap_or(0));
                        proc.child = Some(new_child);
                        continue;
                    }
                    Err(e) => {
                        error!("重启进程 {} 失败: {}", name, e);
                        proc_guard.remove(&name);
                        break;
                    }
                }
            } else {
                proc_guard.remove(&name);
                break;
            }
        }
    }

    pub async fn stop(&self, name: &str) {
        let mut map = self.procs.lock().await;
        if let Some(mut proc) = map.remove(name) {
            if let Some(mut child) = proc.child.take() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }

    pub async fn stop_all(&self) {
        let names: Vec<String> = {
            let map = self.procs.lock().await;
            map.keys().cloned().collect()
        };
        for name in names {
            self.stop(&name).await;
        }
    }

    /// 获取当前所有进程的快照信息 (name, pid, running, restart)
    pub async fn get_processes(&self) -> Vec<(String, u32, bool, bool)> {
        let map = self.procs.lock().await;
        map.iter()
            .map(|(name, proc)| {
                let pid = proc.child.as_ref().and_then(|c| c.id()).unwrap_or(0);
                (name.clone(), pid, proc.child.is_some(), proc.restart)
            })
            .collect()
    }
}