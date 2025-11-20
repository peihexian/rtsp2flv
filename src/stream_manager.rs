use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::{info, error, warn};
use crate::transcoder::Transcoder;

pub struct StreamManager {
    // Map: Stream Name -> StreamState
    streams: Arc<Mutex<HashMap<String, StreamState>>>,
}

struct StreamState {
    running: Arc<AtomicBool>,
    last_heartbeat: Instant,
    // We keep the handle to potentially await it or just let it detach
    handle: JoinHandle<()>,
    // Store URLs for auto-restart
    input_url: String,
    output_url: String,
}

impl StreamManager {
    pub fn new() -> Self {
        let manager = Self {
            streams: Arc::new(Mutex::new(HashMap::new())),
        };
        
        // Spawn background monitoring task
        let streams_clone = manager.streams.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await; // Check every 5s
                Self::monitor_streams(streams_clone.clone());
            }
        });

        manager
    }

    pub fn start_stream(&self, name: String, input_url: String, output_url: String) {
        let mut streams = self.streams.lock().unwrap();

        if let Some(state) = streams.get_mut(&name) {
            // Check if the existing stream is actually alive
            if !state.handle.is_finished() {
                // Stream is running and healthy, just update heartbeat
                state.last_heartbeat = Instant::now();
                info!("Stream '{}' already running, heartbeat updated.", name);
                return;
            } else {
                // Stream exists but thread is finished (Zombie state)
                warn!("Stream '{}' found in zombie state (thread finished). Restarting...", name);
                // We will fall through to start a new one, effectively replacing the old entry
            }
        }

        info!("Starting new stream: {}", name);
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let name_clone = name.clone();
        let input_clone = input_url.clone();
        let output_clone = output_url.clone();

        let handle = tokio::task::spawn_blocking(move || {
            let transcoder = Transcoder::new(input_clone, output_clone, running_clone);
            match transcoder.run() {
                Ok(_) => info!("Stream '{}' finished successfully.", name_clone),
                Err(e) => error!("Stream '{}' failed: {}", name_clone, e),
            }
        });

        streams.insert(name, StreamState {
            running,
            last_heartbeat: Instant::now(),
            handle,
            input_url,
            output_url,
        });
    }

    pub fn heartbeat(&self, name: &str) -> bool {
        let mut streams = self.streams.lock().unwrap();
        if let Some(state) = streams.get_mut(name) {
            state.last_heartbeat = Instant::now();
            // debug!("Heartbeat received for stream: {}", name); // Too noisy for info, use debug if needed
            true
        } else {
            false
        }
    }

    fn monitor_streams(streams: Arc<Mutex<HashMap<String, StreamState>>>) {
        let mut streams = streams.lock().unwrap();
        let now = Instant::now();
        let timeout = Duration::from_secs(120); // 120s timeout to avoid premature closing

        // Identify streams to process
        let keys: Vec<String> = streams.keys().cloned().collect();

        for key in keys {
            let should_remove;
            let mut restart_needed = false;
            
            {
                let state = streams.get(&key).unwrap();
                let elapsed = now.duration_since(state.last_heartbeat);
                let is_timeout = elapsed > timeout;
                let is_crashed = state.handle.is_finished();

                if is_timeout {
                    info!("Stream '{}' timed out (no viewers for {:?}). Stopping...", key, elapsed);
                    state.running.store(false, Ordering::Relaxed);
                    should_remove = true;
                } else if is_crashed {
                    // Stream crashed but still has viewers (heartbeat active)
                    warn!("Stream '{}' crashed but has active viewers. Attempting auto-restart...", key);
                    should_remove = false; // Don't remove yet, we will replace it
                    restart_needed = true;
                } else {
                    should_remove = false;
                }
            }

            if restart_needed {
                // Extract needed info to restart
                if let Some(old_state) = streams.get(&key) {
                    let input_url = old_state.input_url.clone();
                    let output_url = old_state.output_url.clone();
                    
                    // Start new instance
                    let running = Arc::new(AtomicBool::new(true));
                    let running_clone = running.clone();
                    let name_clone = key.clone();
                    let input_clone = input_url.clone();
                    let output_clone = output_url.clone();

                    let handle = tokio::task::spawn_blocking(move || {
                        let transcoder = Transcoder::new(input_clone, output_clone, running_clone);
                        match transcoder.run() {
                            Ok(_) => info!("Stream '{}' finished successfully.", name_clone),
                            Err(e) => error!("Stream '{}' failed: {}", name_clone, e),
                        }
                    });

                    // Update state in map
                    streams.insert(key.clone(), StreamState {
                        running,
                        last_heartbeat: Instant::now(), // Reset heartbeat on restart
                        handle,
                        input_url,
                        output_url,
                    });
                }
            } else if should_remove {
                // Clean up if it's finished or timed out
                if let Some(state) = streams.get(&key) {
                     if state.handle.is_finished() {
                         streams.remove(&key);
                         info!("Removed stopped stream: {}", key);
                     }
                }
            }
        }
    }
}
