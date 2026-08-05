use std::{
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

use swc_common::{
    SourceMap,
    errors::{EmitterWriter, Handler, HandlerFlags},
    sync::Lrc,
};

/// Captures diagnostics into a string instead of stderr.
#[derive(Clone, Default)]
pub struct DiagBuf(Arc<Mutex<Vec<u8>>>);

impl DiagBuf {
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for DiagBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn capturing_handler(cm: &Lrc<SourceMap>) -> (Handler, DiagBuf) {
    let buf = DiagBuf::default();
    let emitter = EmitterWriter::new(Box::new(buf.clone()), Some(cm.clone()), false, false);
    let handler = Handler::with_emitter_and_flags(
        Box::new(emitter),
        HandlerFlags {
            can_emit_warnings: true,
            treat_err_as_bug: false,
            ..Default::default()
        },
    );
    (handler, buf)
}

pub fn compile_fixture(path: &Path) -> Result<(ztsc::Output, String), (anyhow::Error, String)> {
    // Keep diagnostics (and thus snapshots) machine-independent.
    let path = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path);
    let cm: Lrc<SourceMap> = Default::default();
    let (handler, buf) = capturing_handler(&cm);
    match ztsc::compile_file(&cm, &handler, path) {
        Ok(out) => Ok((out, buf.contents())),
        Err(e) => Err((e, buf.contents())),
    }
}
