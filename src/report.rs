//! Progress reporting as data.
//!
//! Every long-running stage reports through one [`Reporter`] by emitting
//! [`Event`]s. The library never decides how progress looks; the CLI turns
//! events into terminal output and tests can simply record them.

use std::error::Error;
use std::fmt;

/// A stage of the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Fetching packages into the cache. Work is measured in bytes.
    Download,
    /// Applying packages to the rootfs. Work is measured in packages.
    Assemble,
    /// Writing the layer and image. Work is measured in steps.
    Image,
}

/// Something that happened during a phase.
#[derive(Clone, Copy)]
pub enum Event<'a> {
    /// A phase started with `total` units of work ahead.
    Begin { phase: Phase, total: u64 },
    /// `units` of work finished; `item` names what was being worked on.
    Advance {
        phase: Phase,
        units: u64,
        item: &'a str,
    },
    /// Work on `item` failed. The phase may carry on with other items.
    Failed {
        phase: Phase,
        item: &'a str,
        error: &'a dyn fmt::Display,
    },
    /// A phase finished. `summary` is the phase's headline number: bytes
    /// downloaded, rootfs entries assembled, or image bytes written.
    End { phase: Phase, summary: u64 },
}

/// Receives progress events. Implementations must be cheap and must not
/// block, since events are emitted from the middle of the work.
pub trait Reporter: Sync {
    fn report(&self, event: Event<'_>);
}

/// Discard all events.
impl Reporter for () {
    fn report(&self, _: Event<'_>) {}
}

/// Render an error and its causes as `outer: cause: root cause`.
///
/// Some library errors already include their source in their own message;
/// a cause whose text is already shown is skipped instead of repeated.
pub fn render_chain(error: &dyn Error) -> String {
    let mut out = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

#[cfg(test)]
pub(crate) mod test_utils {
    use std::sync::Mutex;

    use super::*;

    /// Records events as strings, for asserting on what was reported.
    #[derive(Default)]
    pub struct Recorder(pub Mutex<Vec<String>>);

    impl Reporter for Recorder {
        fn report(&self, event: Event<'_>) {
            let line = match event {
                Event::Begin { phase, total } => format!("begin {phase:?} {total}"),
                Event::Advance { phase, units, item } => {
                    format!("advance {phase:?} {units} {item}")
                }
                Event::Failed { phase, item, error } => format!("failed {phase:?} {item}: {error}"),
                Event::End { phase, summary } => format!("end {phase:?} {summary}"),
            };
            self.0.lock().unwrap().push(line);
        }
    }
}
