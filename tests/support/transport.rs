//! Explicit native owner retained by the actual test fixture.
use docsbase_memory::platform::transport::{self, Transport};
use std::sync::mpsc;
use std::thread::JoinHandle;

pub struct TestTransport {
    transport: Transport,
    stop: Option<mpsc::Sender<()>>,
    owner: Option<JoinHandle<()>>,
}
impl TestTransport {
    pub fn new(handles: usize) -> Self {
        let settings = docsbase_memory::daemon::settings::standalone().unwrap();
        let policy = docsbase_memory::ipc::transport::policy(handles, settings.settings()).unwrap();
        let (ready, capability) = mpsc::sync_channel(0);
        let (stop, stopping) = mpsc::channel();
        let owner = std::thread::spawn(move || {
            transport::run(&policy, move |transport| {
                ready.send(transport).unwrap();
                let _ = stopping.recv();
                Ok(())
            })
            .unwrap();
        });
        Self {
            transport: capability.recv().unwrap(),
            stop: Some(stop),
            owner: Some(owner),
        }
    }
}
impl std::ops::Deref for TestTransport {
    type Target = Transport;
    fn deref(&self) -> &Transport {
        &self.transport
    }
}
impl Drop for TestTransport {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(owner) = self.owner.take() {
            owner.join().unwrap();
        }
    }
}
