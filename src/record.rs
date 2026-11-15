//! Session recording, in the format understood by noVNC's
//! `tests/vnc_playback.html` player.
//!
//! `websockify --record FILE` writes `FILE.session.js` containing:
//!
//! ```js
//! var VNC_frame_encoding = 'base64';
//! var VNC_frame_data = [
//! '{12{UkZC...',   // '{' = server -> client, timestamp in ms, then payload
//! '}40{AAAA...',   // '}' = client -> server
//! 'EOF'
//! ];
//! ```

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use base64::Engine;
use log::warn;

pub struct Recorder {
    writer: BufWriter<File>,
    start: Instant,
    path: PathBuf,
    failed: bool,
}

impl Recorder {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut writer = BufWriter::new(File::create(path)?);
        writer.write_all(b"var VNC_frame_encoding = 'base64';\nvar VNC_frame_data = [\n")?;
        Ok(Self {
            writer,
            start: Instant::now(),
            path: path.to_path_buf(),
            failed: false,
        })
    }

    pub fn frame(&mut self, from_client: bool, data: &[u8]) {
        if self.failed || data.is_empty() {
            return;
        }
        let marker = if from_client { '}' } else { '{' };
        let timestamp = self.start.elapsed().as_millis();
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        if let Err(err) = writeln!(self.writer, "'{marker}{timestamp}{{{encoded}',") {
            self.fail(err);
        }
    }

    pub fn finish(&mut self) {
        if self.failed {
            return;
        }
        if let Err(err) = self
            .writer
            .write_all(b"'EOF'\n];\n")
            .and_then(|_| self.writer.flush())
        {
            self.fail(err);
        }
    }

    fn fail(&mut self, err: std::io::Error) {
        self.failed = true;
        warn!("recording to {} failed: {err}", self.path.display());
    }
}

/// `websockify` appends `.session.js` to the requested name; concurrent
/// sessions get a numeric suffix instead of overwriting each other.
pub fn path_for(base: &Path, session: usize) -> PathBuf {
    let base = base.display().to_string();
    if session <= 1 {
        PathBuf::from(format!("{base}.session.js"))
    } else {
        PathBuf::from(format!("{base}.session.{session}.js"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_playable_recording() {
        let dir = std::env::temp_dir().join(format!("novnc-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = path_for(&dir.join("demo"), 1);

        let mut rec = Recorder::create(&path).unwrap();
        rec.frame(false, b"RFB 003.008\n");
        rec.frame(true, &[0, 1, 2, 3]);
        rec.finish();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("var VNC_frame_encoding = 'base64';"));
        assert!(text.contains("'{0{"));
        assert!(text.contains("'EOF'"));
        assert!(text.ends_with("];\n"));

        // Every recorded frame must round-trip via base64.
        for line in text
            .lines()
            .filter(|l| l.starts_with('\'') && !l.contains("EOF"))
        {
            let line = line.trim_end_matches(',');
            let body = line.trim_start_matches('\'').trim_end_matches('\'');
            let start = body[1..].find('{').unwrap() + 2;
            let data = &body[start..];
            assert!(!data.is_empty());
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
