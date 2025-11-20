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

pub struct Transcoder {
    input_url: String,
    output_url: String,
    running: Arc<AtomicBool>,
}

impl Transcoder {
    pub fn new(input_url: String, output_url: String, running: Arc<AtomicBool>) -> Self {
        Self {
            input_url,
            output_url,
            running,
        }
    }

    pub fn run(&self) -> Result<()> {
        ffmpeg::init()?;

        // 1. Open Input
        let mut input_opts = ffmpeg::Dictionary::new();
        // Force TCP for RTSP to avoid UDP packet loss issues
        if self.input_url.starts_with("rtsp://") {
            info!("Enforcing TCP transport for RTSP input");
            input_opts.set("rtsp_transport", "tcp");
            // Set socket timeout to 5 seconds (in microseconds) to detect network issues
            input_opts.set("stimeout", "5000000");
        }
        
        let mut ictx = ffmpeg::format::input_with_dictionary(&self.input_url, input_opts)?;
        
        // 2. Open Output
        let mut octx = ffmpeg::format::output_as(&self.output_url, "flv")?;

        // 3. Copy Streams
        // We need to collect the mapping of input stream index to output stream index
        let mut stream_mapping = vec![0isize; ictx.nb_streams() as usize];
        let mut stream_index = 0;

        for (i, istream) in ictx.streams().enumerate() {
            let codec_type = istream.parameters().medium();
            
            // We only care about Video and Audio
            if codec_type == ffmpeg::media::Type::Video || codec_type == ffmpeg::media::Type::Audio {
                let mut ostream = octx.add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::None))?;
                ostream.set_parameters(istream.parameters());
                
                // Copy timebase is important? Usually for remuxing we just copy parameters.
                // ostream.set_time_base(istream.time_base()); 
                
                stream_mapping[i] = stream_index;
                stream_index += 1;
            } else {
                stream_mapping[i] = -1;
            }
        }

        // 4. Write Header
        octx.write_header()?;

        info!("Transcoder started: {} -> {}", self.input_url, self.output_url);

        // Initialize stream states for output streams
        let mut stream_states = vec![StreamState::new(); octx.nb_streams() as usize];

        // 5. Packet Loop
        for (stream, mut packet) in ictx.packets() {
            // Check cancellation signal
            if !self.running.load(Ordering::Relaxed) {
                info!("Transcoder stopping requested.");
                break;
            }

            let istream_index = stream.index();
            let ostream_index = stream_mapping[istream_index];

            if ostream_index < 0 {
                continue;
            }

            // let istream = ictx.stream(istream_index).ok_or(anyhow!("Input stream not found"))?;
            let ostream = octx.stream(ostream_index as usize).ok_or(anyhow!("Output stream not found"))?;

            // Rescale timestamps
            packet.rescale_ts(stream.time_base(), ostream.time_base());
            packet.set_position(-1);
            packet.set_stream(ostream_index as usize);

            // --- Robust Timestamp Handling ---
            let state = &mut stream_states[ostream_index as usize];
            
            let mut dts = packet.dts();
            let mut pts = packet.pts();

            // 1. Fix missing DTS
            if dts.is_none() {
                // If we have a last_dts, increment it slightly (e.g. 1 unit)
                // If it's the first packet, start at 0
                let new_dts = if state.last_dts == i64::MIN {
                    0
                } else {
                    state.last_dts + 1
                };
                // warn!("Fixed missing DTS: {:?} -> {}", dts, new_dts);
                dts = Some(new_dts);
            }
            let mut dts_val = dts.unwrap();

            // 2. Fix missing PTS
            if pts.is_none() {
                // Assume PTS = DTS if missing
                pts = Some(dts_val);
            }
            let mut pts_val = pts.unwrap();

            // 3. Ensure PTS >= DTS
            if pts_val < dts_val {
                // warn!("Fixed PTS < DTS: pts={} dts={}", pts_val, dts_val);
                pts_val = dts_val;
            }

            // 4. Ensure Monotonicity (DTS must increase)
            if state.last_dts != i64::MIN && dts_val <= state.last_dts {
                let corrected_dts = state.last_dts + 1;
                // warn!("Fixed non-monotonic DTS: {} -> {}", dts_val, corrected_dts);
                dts_val = corrected_dts;
                
                // Adjust PTS if needed to maintain PTS >= DTS
                if pts_val < dts_val {
                    pts_val = dts_val;
                }
            }

            // Update state
            state.last_dts = dts_val;
            state.last_pts = pts_val;

            // Apply back to packet
            packet.set_dts(Some(dts_val));
            packet.set_pts(Some(pts_val));
            // ---------------------------------

            packet.write_interleaved(&mut octx)?;
        }

        // 6. Write Trailer
        octx.write_trailer()?;
        info!("Transcoder finished.");

        Ok(())
    }
}
