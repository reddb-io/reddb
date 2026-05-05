use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::config::EcRegistry;
use super::consolidation;
use crate::storage::unified::store::UnifiedStore;

pub struct EcWorker {
    running: Arc<AtomicBool>,
}

impl EcWorker {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&self, registry: Arc<EcRegistry>, store: Arc<UnifiedStore>) {
        if self.running.load(Ordering::SeqCst) {
            return;
        }
        self.running.store(true, Ordering::SeqCst);

        let running = Arc::clone(&self.running);

        std::thread::Builder::new()
            .name("reddb-ec-worker".into())
            .spawn(move || {
                while running.load(Ordering::SeqCst) {
                    let configs = registry.async_configs();
                    if configs.is_empty() {
                        std::thread::sleep(Duration::from_secs(10));
                        continue;
                    }

                    let min_interval = configs
                        .iter()
                        .map(|c| c.consolidation_interval_secs)
                        .min()
                        .unwrap_or(60);

                    std::thread::sleep(Duration::from_secs(min_interval));

                    if !running.load(Ordering::SeqCst) {
                        break;
                    }

                    for config in &configs {
                        let _ = consolidation::consolidate(&store, config, None);
                    }
                }
            })
            .ok();
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}
