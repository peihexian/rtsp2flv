use anyhow::{Result, anyhow};
use ffmpeg_next as ffmpeg;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tracing::info;

#[derive(Clone, Copy)]
struct StreamState {
    last_dts: i64,
    last_pts: i64,
}

impl StreamState {
    fn new() -> Self {
        Self {
            last_dts: i64::MIN,
            last_pts: i64::MIN,
        }
    }
}

/// RTSP 转 FLV 转码器
/// 
/// 使用 FFmpeg 将 RTSP 流转码/封装为 FLV 格式。
pub struct Transcoder {
    input_url: String,
    output_url: String,
    running: Arc<AtomicBool>,
}

impl Transcoder {
    /// 创建新的转码器实例
    pub fn new(input_url: String, output_url: String, running: Arc<AtomicBool>) -> Self {
        Self {
            input_url,
            output_url,
            running,
        }
    }

    /// 运行转码任务
    /// 
    /// 这是一个阻塞操作，直到流结束或被停止。
    pub fn run(&self) -> Result<()> {
        ffmpeg::init()?;

        // 1. 打开输入
        let mut input_opts = ffmpeg::Dictionary::new();
        // 强制使用 TCP 传输 RTSP 以避免 UDP 丢包问题
        if self.input_url.starts_with("rtsp://") {
            info!("强制使用 TCP 传输 RTSP 输入");
            input_opts.set("rtsp_transport", "tcp");
            // 设置 socket 超时为 5 秒 (单位: 微秒) 以检测网络问题
            input_opts.set("stimeout", "5000000");
        }
        
        let mut ictx = ffmpeg::format::input_with_dictionary(&self.input_url, input_opts)?;
        
        // 2. 打开输出
        let mut octx = ffmpeg::format::output_as(&self.output_url, "flv")?;

        // 3. 复制流配置
        // 我们需要收集输入流索引到输出流索引的映射
        let mut stream_mapping = vec![0isize; ictx.nb_streams() as usize];
        let mut stream_index = 0;

        for (i, istream) in ictx.streams().enumerate() {
            let codec_type = istream.parameters().medium();
            
            // 我们只关心视频和音频
            if codec_type == ffmpeg::media::Type::Video || codec_type == ffmpeg::media::Type::Audio {
                let mut ostream = octx.add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::None))?;
                ostream.set_parameters(istream.parameters());
                
                // 复制 timebase 重要吗？通常对于重新封装，我们只需要复制参数。
                // ostream.set_time_base(istream.time_base()); 
                
                stream_mapping[i] = stream_index;
                stream_index += 1;
            } else {
                stream_mapping[i] = -1;
            }
        }

        // 4. 写入文件头
        octx.write_header()?;

        info!("转码器已启动: {} -> {}", self.input_url, self.output_url);

        // 初始化输出流的状态
        let mut stream_states = vec![StreamState::new(); octx.nb_streams() as usize];

        // 5. 数据包循环
        for (stream, mut packet) in ictx.packets() {
            // 检查取消信号
            if !self.running.load(Ordering::Relaxed) {
                info!("收到停止转码请求。");
                break;
            }

            let istream_index = stream.index();
            let ostream_index = stream_mapping[istream_index];

            if ostream_index < 0 {
                continue;
            }

            // let istream = ictx.stream(istream_index).ok_or(anyhow!("Input stream not found"))?;
            let ostream = octx.stream(ostream_index as usize).ok_or(anyhow!("输出流未找到"))?;

            // 重新缩放时间戳
            packet.rescale_ts(stream.time_base(), ostream.time_base());
            packet.set_position(-1);
            packet.set_stream(ostream_index as usize);

            // --- 健壮的时间戳处理 ---
            let state = &mut stream_states[ostream_index as usize];
            
            let mut dts = packet.dts();
            let mut pts = packet.pts();

            // 1. 修复缺失的 DTS
            if dts.is_none() {
                // 如果有 last_dts，稍微增加它（例如 1 个单位）
                // 如果是第一个包，从 0 开始
                let new_dts = if state.last_dts == i64::MIN {
                    0
                } else {
                    state.last_dts + 1
                };
                // warn!("修复缺失的 DTS: {:?} -> {}", dts, new_dts);
                dts = Some(new_dts);
            }
            let mut dts_val = dts.unwrap();

            // 2. 修复缺失的 PTS
            if pts.is_none() {
                // 如果缺失，假设 PTS = DTS
                pts = Some(dts_val);
            }
            let mut pts_val = pts.unwrap();

            // 3. 确保 PTS >= DTS
            if pts_val < dts_val {
                // warn!("修复 PTS < DTS: pts={} dts={}", pts_val, dts_val);
                pts_val = dts_val;
            }

            // 4. 确保单调性 (DTS 必须增加)
            if state.last_dts != i64::MIN && dts_val <= state.last_dts {
                let corrected_dts = state.last_dts + 1;
                // warn!("修复非单调 DTS: {} -> {}", dts_val, corrected_dts);
                dts_val = corrected_dts;
                
                // 如果需要，调整 PTS 以保持 PTS >= DTS
                if pts_val < dts_val {
                    pts_val = dts_val;
                }
            }

            // 更新状态
            state.last_dts = dts_val;
            state.last_pts = pts_val;

            // 应用回数据包
            packet.set_dts(Some(dts_val));
            packet.set_pts(Some(pts_val));
            // ---------------------------------

            packet.write_interleaved(&mut octx)?;
        }

        // 6. 写入文件尾
        octx.write_trailer()?;
        info!("转码器已结束。");

        Ok(())
    }
}
