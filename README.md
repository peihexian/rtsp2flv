# RTSP2FLV & SRS 服务部署与使用文档

本文档详细说明了 SRS 流媒体服务的部署方法，以及 RTSP 转 FLV 服务（rtsp2flv）的配置与前端集成指南。

## 1. SRS 服务部署

SRS (Simple Realtime Server) 是一个高效的实时视频服务器，本系统使用它来分发 FLV 流。

### 1.1 目录结构
建议在服务器的 `/data/srs` 目录下部署，结构如下：
```bash
/data/srs
├── docker-compose.yml  # 容器编排文件
└── startup.sh          # 启动脚本
```

### 1.2 配置文件 (docker-compose.yml)
创建 `docker-compose.yml` 文件：

```yaml
version: '3.8'
services:
  srs:
    image: ossrs/srs:6.0.183
    container_name: srs_server
    restart: always # 关键：实现开机自启
    ports:
      - "1935:1935"       # RTMP 推流端口
      - "1985:1985"       # HTTP API 端口
      - "8180:8080"       # HTTP/HLS/WebRTC 拉流端口
      - "8000:8000/udp"   # SRT/UDP
      - "10080:10080/udp" # WebRTC/UDP
    # 如果需要持久化配置或日志，可以取消注释以下 volumes
    # volumes:
    #   - ./srs-conf:/usr/local/srs/conf
```

### 1.3 启动服务
可以使用以下 `startup.sh` 脚本或直接运行命令启动：

```bash
# startup.sh 内容
docker compose up -d
```

执行启动：
```bash
cd /data/srs
sh startup.sh
```

---

## 2. RTSP2FLV 服务使用

RTSP2FLV 是一个中间件，负责按需将 RTSP 流转码并推送到 SRS。

### 2.1 配置文件 (config.yaml)
在程序运行目录下需要 `config.yaml` 文件，配置 SRS 地址和预定义的 RTSP 流：

```yaml
server:
  port: 3000 # 本服务监听端口

srs:
  # SRS 服务器的 HTTP API 地址 (注意 IP 需要是 rtsp2flv 服务能访问到的地址)
  api_url: "http://172.0.34.94:1985/api/v1/streams"
  # 播放地址模板，{stream_name} 会被替换为实际流名称
  playback_url_template: "http://172.0.34.94:8180/live/{stream_name}.flv"

# API 访问密钥列表
api_keys:
  - "secret-token-1"
  - "secret-token-2"

streams:
  - name: "Camera 1"
    url: "rtsp://172.0.34.130:8554/stream"
  - name: "Test Stream"
    url: "rtsp://wowzaec2demo.streamlock.net/vod/mp4:BigBuckBunny_115k.mov"
```

### 2.2 安全配置
在生产环境中，务必配置 `api_keys` 以确保 API 安全：

```yaml
api_keys:
  - "your-production-token-here"
  - "backup-token-for-mobile-app"
```

**安全建议**：
- 使用强随机字符串作为 Token
- 为不同客户端使用不同的 Token
- 定期轮换 Token
- 不要在公开代码仓库中暴露真实的 Token

### 2.3 启动服务
确保配置文件存在后，直接运行程序：

```bash
# 开发环境
cargo run

# 生产环境 (编译后)
./rtsp2flv
```

---

## 3. 前端程序集成指南

前端通过 HTTP API 与 rtsp2flv 服务交互。**核心逻辑是"按需播放"和"心跳保活"。**

### 3.1 API 认证

所有需要修改状态的 API（播放、心跳）都需要提供有效的 API Token 进行认证。

#### 认证方式
- **Header 方式** (推荐): 在请求头中添加 `Authorization: <token>` 或 `Authorization: Bearer <token>`
- **配置文件**: 在 `config.yaml` 的 `api_keys` 字段中配置允许的 Token 列表

#### 认证要求
- `/api/streams` (GET) - **无需认证**
- `/api/play` (POST) - **需要认证**
- `/api/heartbeat` (POST) - **需要认证**

### 3.2 获取流列表
获取所有预配置的流信息。

- **URL**: `/api/streams`
- **Method**: `GET`
- **认证**: 无需认证
- **Response**:
  ```json
  [
    { "name": "Camera 1", "url": "rtsp://..." },
    { "name": "Test Stream", "url": "rtsp://..." }
  ]
  ```

### 3.3 开始播放 (Play)
请求播放某个流。如果流未启动，服务会启动转码任务。

- **URL**: `/api/play`
- **Method**: `POST`
- **认证**: **需要认证**
- **Content-Type**: `application/json`
- **Headers**:
  ```http
  Authorization: <your-api-token>
  # 或
  Authorization: Bearer <your-api-token>
  ```
- **Body**:
  ```json
  {
    "name": "Camera 1",
    "url": "" // 可选。如果为空，使用配置文件中的 URL；如果不为空，则作为自定义 RTSP 地址播放
  }
  ```
- **Response**:
  ```json
  {
    "playback_url": "http://172.0.34.94:8180/live/camera_1.flv"
  }
  ```
  前端拿到 `playback_url` 后，使用 flv.js 或其他播放器进行播放。

- **错误响应**:
  - `401 Unauthorized`: API Token 无效或缺失
  - `400 Bad Request`: 参数错误（如 RTSP 地址格式不正确）
  - `500 Internal Server Error`: 服务器内部错误

### 3.4 心跳保活 (Heartbeat) - **重点**
为了节省资源，rtsp2flv 服务会在没有观众时自动停止转码。**前端必须定期发送心跳包来维持流的活跃状态。**

- **机制说明**:
  1. 前端调用 `/api/play` 成功后，应立即启动一个定时器。
  2. 建议每 **15-20秒** 发送一次心跳请求。
  3. 如果服务端超过一定时间（默认约 60秒）未收到心跳，将自动停止该流的转码任务。
  4. 当用户关闭页面或停止播放时，停止发送心跳，服务端会自动清理资源。

- **URL**: `/api/heartbeat`
- **Method**: `POST`
- **认证**: **需要认证**
- **Content-Type**: `application/json`
- **Headers**:
  ```http
  Authorization: <your-api-token>
  # 或
  Authorization: Bearer <your-api-token>
  ```
- **Body**:
  ```json
  {
    "name": "Camera 1" // 必须与 /api/play 中的 name 一致
  }
  ```
- **Response**:
  - `200 OK`: 心跳成功，流保持活跃。
  - `401 Unauthorized`: API Token 无效或缺失
  - `404 Not Found`: 流不存在或已停止（此时前端应提示错误或重新调用 `/api/play`）

### 3.5 前端集成示例 (完整代码)

前端集成需要处理认证逻辑，以下是完整的实现示例：

```javascript
// API Token 配置
const API_TOKEN = "your-secret-token"; // 从配置文件或用户输入获取

// 获取带认证的请求头
function getAuthHeaders() {
    const headers = { 'Content-Type': 'application/json' };
    if (API_TOKEN) {
        headers['Authorization'] = API_TOKEN;
        // 或者使用 Bearer 格式：headers['Authorization'] = `Bearer ${API_TOKEN}`;
    }
    return headers;
}

// 1. 开始播放
async function startPlay(streamName) {
    try {
        const res = await fetch('/api/play', {
            method: 'POST',
            headers: getAuthHeaders(),
            body: JSON.stringify({ name: streamName })
        });
        
        if (res.status === 401) {
            throw new Error("认证失败：无效的 API Token");
        }
        
        if (!res.ok) {
            const errorText = await res.text();
            throw new Error(`播放请求失败: ${errorText}`);
        }
        
        const data = await res.json();
        
        // 初始化播放器...
        player.load(data.playback_url);

        // 2. 启动心跳 (每20秒一次)
        const heartbeatInterval = setInterval(async () => {
            try {
                const hbRes = await fetch('/api/heartbeat', {
                    method: 'POST',
                    headers: getAuthHeaders(),
                    body: JSON.stringify({ name: streamName })
                });
                
                if (hbRes.status === 401) {
                    console.error("心跳认证失败");
                    clearInterval(heartbeatInterval);
                    return;
                }
                
                if (hbRes.status !== 200) {
                    console.error("流已停止");
                    clearInterval(heartbeatInterval);
                    // 可选：尝试重新连接
                }
            } catch (error) {
                console.error("心跳请求失败:", error);
                clearInterval(heartbeatInterval);
            }
        }, 20000);

        // 页面关闭时清除定时器
        window.addEventListener('beforeunload', () => {
            clearInterval(heartbeatInterval);
        });
        
        return heartbeatInterval;
        
    } catch (error) {
        console.error("播放失败:", error);
        alert(`播放失败: ${error.message}`);
        throw error;
    }
}

// 获取流列表（无需认证）
async function loadStreams() {
    try {
        const response = await fetch('/api/streams');
        return await response.json();
    } catch (error) {
        console.error("加载流列表失败:", error);
        return [];
    }
}
```

#### 前端集成注意事项：

1. **Token 存储**: 建议将 API Token 存储在安全的地方，如环境变量或配置文件
2. **错误处理**: 对 401 状态码进行特殊处理，提示用户检查 Token
3. **心跳频率**: 建议每 15-20 秒发送一次心跳，确保流不会超时停止
4. **认证一致性**: 确保 `/api/play` 和 `/api/heartbeat` 使用相同的认证信息
