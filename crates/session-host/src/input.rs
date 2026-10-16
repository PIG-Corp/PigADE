//! A bounded PTY input worker. Blocking slave input never occupies the terminal actor.
//!
//! Do not set O_NONBLOCK on a duplicated master descriptor here: duplicates share
//! open-file-description flags with portable-pty's output reader. The worker may
//! block in the OS writer while the actor continues to resize or kill the child.
//! Killing the last slave holder unblocks the write. Drop disconnects an idle
//! worker; it deliberately never joins a potentially blocked writer thread.
use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

pub const MAX_INPUT_BYTES: usize = 16 * 1024;
pub const INPUT_QUEUE_CAPACITY: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputWriteError {
    TooLarge,
    Full,
    Closed,
    Io(String),
}
impl fmt::Display for InputWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "PTY input exceeds {MAX_INPUT_BYTES}-byte limit"),
            Self::Full => write!(f, "PTY input queue full; input was not queued"),
            Self::Closed => write!(f, "PTY input worker closed"),
            Self::Io(error) => write!(f, "PTY input write failed: {error}"),
        }
    }
}
impl std::error::Error for InputWriteError {}

#[derive(Clone)]
pub struct InputWriter {
    sender: mpsc::SyncSender<Vec<u8>>,
    error: Arc<Mutex<Option<String>>>,
}
impl InputWriter {
    /// Eight pending chunks plus one in-flight chunk: at most 144 KiB of input.
    /// This owns one worker per session, not one thread per input command.
    pub fn new(mut writer: Box<dyn Write + Send>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(INPUT_QUEUE_CAPACITY);
        let error = Arc::new(Mutex::new(None));
        let worker_error = error.clone();
        thread::spawn(move || {
            while let Ok(bytes) = receiver.recv() {
                if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    // Record the OS error only. Input bytes are never logged.
                    *worker_error.lock().expect("input error lock") = Some(error.to_string());
                    break;
                }
            }
        });
        Self { sender, error }
    }

    /// Nonblocking admission. Full/TooLarge means none of this chunk was queued.
    /// A successful return confirms admission, not consumption by the CLI.
    /// Callers must retain/retry terminal-response chunks on Full, or report an
    /// explicit blocked state. They must not park the terminal/control actor.
    pub fn try_write(&self, bytes: Vec<u8>) -> Result<(), InputWriteError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(InputWriteError::TooLarge);
        }
        if let Some(error) = self.error() {
            return Err(InputWriteError::Io(error));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        self.sender.try_send(bytes).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => InputWriteError::Full,
            mpsc::TrySendError::Disconnected(_) => self
                .error()
                .map(InputWriteError::Io)
                .unwrap_or(InputWriteError::Closed),
        })
    }

    /// Asynchronous OS error latch; polling does not consume or hide the error.
    pub fn error(&self) -> Option<String> {
        self.error.lock().expect("input error lock").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Condvar;
    use std::time::{Duration, Instant};

    struct BlockedWriter {
        gate: Arc<(Mutex<bool>, Condvar)>,
        started: Option<mpsc::Sender<()>>,
        observed: Arc<Mutex<Vec<u8>>>,
    }
    impl Write for BlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
            }
            let (mutex, signal) = &*self.gate;
            let mut ready = mutex.lock().unwrap();
            while !*ready {
                ready = signal.wait(ready).unwrap();
            }
            self.observed.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_worker_has_bounded_nonblocking_fifo_admission() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let (started, receiver) = mpsc::channel();
        let input = InputWriter::new(Box::new(BlockedWriter {
            gate: gate.clone(),
            observed: observed.clone(),
            started: Some(started),
        }));
        input.try_write(vec![0; MAX_INPUT_BYTES]).unwrap();
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        for value in 1..=INPUT_QUEUE_CAPACITY {
            input.try_write(vec![value as u8; MAX_INPUT_BYTES]).unwrap();
        }
        assert_eq!(
            input.try_write(vec![255; MAX_INPUT_BYTES]),
            Err(InputWriteError::Full)
        );
        assert_eq!(
            input.try_write(vec![0; MAX_INPUT_BYTES + 1]),
            Err(InputWriteError::TooLarge)
        );
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let deadline = Instant::now() + Duration::from_secs(2);
        while observed.lock().unwrap().len() != (INPUT_QUEUE_CAPACITY + 1) * MAX_INPUT_BYTES {
            assert!(
                Instant::now() < deadline,
                "writer did not drain accepted input"
            );
            thread::sleep(Duration::from_millis(1));
        }
        let observed = observed.lock().unwrap();
        for (value, chunk) in observed.chunks(MAX_INPUT_BYTES).enumerate() {
            assert!(chunk.iter().all(|byte| *byte == value as u8));
        }
    }

    struct FailedWriter;
    impl Write for FailedWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn asynchronous_error_is_latched_without_logging_input() {
        let input = InputWriter::new(Box::new(FailedWriter));
        input.try_write(b"synthetic input".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while input.error().is_none() {
            assert!(Instant::now() < deadline, "writer did not publish error");
            thread::sleep(Duration::from_millis(1));
        }
        let first = input.error();
        assert_eq!(first, input.error());
        assert!(matches!(
            input.try_write(vec![0]),
            Err(InputWriteError::Io(_))
        ));
        assert!(!first.unwrap().contains("synthetic input"));
    }
}
