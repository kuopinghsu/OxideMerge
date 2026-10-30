//! Background read-only analysis jobs. Dropping/cancelling never joins on the UI thread.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread::JoinHandle;
pub struct Task<T> {
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<AtomicUsize>,
    handle: Option<JoinHandle<anyhow::Result<T>>>,
}
impl<T: Send + 'static> Task<T> {
    pub fn start(
        work: impl FnOnce(&AtomicBool, &AtomicUsize) -> anyhow::Result<T> + Send + 'static,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let (c, p) = (cancel.clone(), progress.clone());
        Self {
            cancel,
            progress,
            handle: Some(std::thread::spawn(move || work(&c, &p))),
        }
    }
    pub fn poll(&mut self) -> Option<anyhow::Result<T>> {
        if !self.handle.as_ref()?.is_finished() {
            return None;
        }
        let result = self
            .handle
            .take()
            .unwrap()
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Analysis worker failed")));
        Some(if self.cancel.load(Ordering::Relaxed) {
            Err(anyhow::anyhow!("Analysis cancelled"))
        } else {
            result
        })
    }
    pub fn status(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!(
                "{}: {}",
                crate::i18n::tr("analysis.progress"),
                self.progress.load(Ordering::Relaxed)
            ));
            if ui.button(crate::i18n::tr("btn.cancel")).clicked() {
                self.cancel.store(true, Ordering::Relaxed);
            }
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
}
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
