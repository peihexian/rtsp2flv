use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::{info, error, warn};
use crate::transcoder::Transcoder;

pub struct StreamManager {
    // 映射: 流名称 -> 流状态
    streams: Arc<Mutex<HashMap<String, StreamState>>>,
}

struct StreamState {
    running: Arc<AtomicBool>,
    last_heartbeat: Instant,
    // 保留句柄以便等待或分离
    handle: JoinHandle<()>,
    // 存储 URL 用于自动重启
    input_url: String,
    output_url: String,
    // 重启计数器
    restart_count: u32,
    // 上次尝试重启的时间
    last_restart_attempt: Instant,
}

impl StreamManager {
    pub fn new() -> Self {
        let manager = Self {
            streams: Arc::new(Mutex::new(HashMap::new())),
        };
        
        // 启动后台监控任务
        let streams_clone = manager.streams.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await; // 每 5 秒检查一次
                Self::monitor_streams(streams_clone.clone());
            }
        });

        manager
    }

    pub fn start_stream(&self, name: String, input_url: String, output_url: String) {
        let mut streams = self.streams.lock().unwrap();

        if let Some(state) = streams.get_mut(&name) {
            // 检查现有流是否确实存活
            if !state.handle.is_finished() {
                // 流正在运行且健康，仅更新心跳
                state.last_heartbeat = Instant::now();
                info!("流 '{}' 正在运行，已更新心跳。", name);
                return;
            } else {
                // 流存在但线程已结束（僵尸状态）
                warn!("流 '{}' 处于僵尸状态（线程已结束）。正在重启...", name);
                // 继续执行以启动新流，实际上会替换旧条目
            }
        }

        info!("启动新流: {}", name);
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let name_clone = name.clone();
        let input_clone = input_url.clone();
        let output_clone = output_url.clone();

        let handle = tokio::task::spawn_blocking(move || {
            let transcoder = Transcoder::new(input_clone, output_clone, running_clone);
            match transcoder.run() {
                Ok(_) => info!("流 '{}' 已成功结束。", name_clone),
                Err(e) => error!("流 '{}' 失败: {}", name_clone, e),
            }
        });

        streams.insert(name, StreamState {
            running,
            last_heartbeat: Instant::now(),
            handle,
            input_url,
            output_url,
            restart_count: 0,
            last_restart_attempt: Instant::now(),
        });
    }

    pub fn heartbeat(&self, name: &str) -> bool {
        let mut streams = self.streams.lock().unwrap();
        if let Some(state) = streams.get_mut(name) {
            state.last_heartbeat = Instant::now();
            true
        } else {
            false
        }
    }

    fn monitor_streams(streams: Arc<Mutex<HashMap<String, StreamState>>>) {
        let mut streams = streams.lock().unwrap();
        let now = Instant::now();
        let timeout = Duration::from_secs(120); // 120秒超时，避免过早关闭

        // 识别需要处理的流
        let keys: Vec<String> = streams.keys().cloned().collect();

        for key in keys {
            let should_remove;
            let mut restart_needed = false;
            
            {
                let state = streams.get_mut(&key).unwrap();
                let elapsed = now.duration_since(state.last_heartbeat);
                let is_timeout = elapsed > timeout;
                let is_crashed = state.handle.is_finished();

                // 如果流运行稳定超过 60 秒，重置重启计数
                if !is_crashed && now.duration_since(state.last_restart_attempt) > Duration::from_secs(60) {
                    if state.restart_count > 0 {
                        state.restart_count = 0;
                    }
                }

                if is_timeout {
                    info!("流 '{}' 超时（{:?} 无观众）。正在停止...", key, elapsed);
                    state.running.store(false, Ordering::Relaxed);
                    should_remove = true;
                } else if is_crashed {
                    // 流崩溃但仍有观众（心跳活跃）
                    warn!("流 '{}' 已崩溃但有活跃观众。", key);
                    
                    // 检查重启频率
                    if state.restart_count >= 5 {
                        error!("流 '{}' 重启次数过多（{} 次），停止自动重启。", key, state.restart_count);
                        should_remove = true;
                    } else if now.duration_since(state.last_restart_attempt) < Duration::from_secs(10) {
                        warn!("流 '{}' 崩溃过快，等待冷却...", key);
                        should_remove = false; // 暂时保留，下次循环再试
                    } else {
                        warn!("尝试自动重启流 '{}' (第 {} 次)...", key, state.restart_count + 1);
                        should_remove = false;
                        restart_needed = true;
                    }
                } else {
                    should_remove = false;
                }
            }

            if restart_needed {
                // 提取重启所需信息
                if let Some(old_state) = streams.get(&key) {
                    let input_url = old_state.input_url.clone();
                    let output_url = old_state.output_url.clone();
                    let restart_count = old_state.restart_count + 1;
                    
                    // 启动新实例
                    let running = Arc::new(AtomicBool::new(true));
                    let running_clone = running.clone();
                    let name_clone = key.clone();
                    let input_clone = input_url.clone();
                    let output_clone = output_url.clone();

                    let handle = tokio::task::spawn_blocking(move || {
                        let transcoder = Transcoder::new(input_clone, output_clone, running_clone);
                        match transcoder.run() {
                            Ok(_) => info!("流 '{}' 已成功结束。", name_clone),
                            Err(e) => error!("流 '{}' 失败: {}", name_clone, e),
                        }
                    });

                    // 更新 Map 中的状态
                    streams.insert(key.clone(), StreamState {
                        running,
                        last_heartbeat: Instant::now(), // 重启时重置心跳
                        handle,
                        input_url,
                        output_url,
                        restart_count,
                        last_restart_attempt: Instant::now(),
                    });
                }
            } else if should_remove {
                // 如果已完成或超时，进行清理
                if let Some(state) = streams.get(&key) {
                     if state.handle.is_finished() {
                         streams.remove(&key);
                         info!("已移除停止的流: {}", key);
                     }
                }
            }
        }
    }
}
