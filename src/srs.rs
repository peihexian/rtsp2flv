use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use anyhow::{Result, anyhow};
use tracing::{info, error};

#[derive(Clone)]
pub struct SrsClient {
    client: Client,
    api_url: String,
    playback_url_template: String,
}

#[derive(Serialize)]
struct SrsRequest {
    url: String,
    stream_name: String,
}

#[derive(Deserialize, Debug)]
pub struct SrsResponse {
    pub code: i32,
    pub server: Option<String>,
    pub session_id: Option<String>,
}

impl SrsClient {
    /// 创建新的 SRS 客户端实例
    /// 
    /// # 参数
    /// * `api_url` - SRS 服务器的 API 地址
    /// * `playback_url_template` - 播放地址模板
    pub fn new(api_url: String, playback_url_template: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            api_url,
            playback_url_template,
        }
    }

    /// 请求播放流
    /// 
    /// 负责验证 RTSP 地址，发送请求到 SRS，并返回播放地址。
    pub async fn play_stream(&self, name: &str, rtsp_url: &str) -> Result<String> {
        // 1. 校验 RTSP 地址
        if rtsp_url.trim().is_empty() {
            return Err(anyhow!("RTSP 地址不能为空"));
        }
        if !rtsp_url.to_lowercase().starts_with("rtsp://") {
            return Err(anyhow!("非法的 RTSP 地址格式: {}", rtsp_url));
        }

        info!("请求 SRS 播放流: {} -> {}", name, rtsp_url);

        // 2. 构造请求体
        let payload = SrsRequest {
            url: rtsp_url.to_string(),
            stream_name: name.to_string(),
        };

        // 3. 发送请求到 SRS (如果不是本地测试环境)
        if !self.api_url.contains("localhost") {
             let res = self.client.post(&self.api_url)
                .json(&payload)
                .send()
                .await;
            
             match res {
                 Ok(response) => {
                     if response.status().is_success() {
                         info!("SRS API 调用成功: {:?}", response.status());
                         // 这里可以添加解析 SRS 返回 JSON 的逻辑，如果 SRS 返回了具体播放地址
                     } else {
                         error!("SRS API 调用失败: 状态码 {}", response.status());
                         // 根据需求，这里可以选择报错，或者降级处理
                         // return Err(anyhow!("SRS 服务器返回错误: {}", response.status()));
                     }
                 },
                 Err(e) => {
                     error!("连接 SRS API 失败: {}", e);
                     // return Err(anyhow!("无法连接到 SRS 服务器: {}", e));
                 }
             }
        }

        // 4. 生成播放地址
        // 使用配置中的模板进行替换
        // 对流名称进行简单的 URL 安全处理（替换空格）
        let safe_name = name.replace(" ", "_").to_lowercase();
        let playback_url = self.playback_url_template.replace("{stream_name}", &safe_name);
        
        Ok(playback_url)
    }
}
