//! Request identity: what an async result was asked for, and whether that is still what
//! the page wants.
//!
//! Every asynchronous thing the editor does — a load, an apply, the diff confirmation, the
//! version poll's digest check — is issued *for* one document, one view and one generation
//! of the page. `LoadableComponentMaster` spawns a fresh `load()` per `Msg::Load` with no
//! cancellation (`COMP/src/loadable_component.rs`, `Msg::Load`), so two loads for two
//! different views can be in flight at once and finish in either order. Applying the
//! second-to-last answer over the last one is how a view switch ends up writing view A's
//! text into view B's path (`docs/REVIEW-2026-09-07.md` F4).
//!
//! The rule this module implements: **name the thing you are matching against, and check it
//! at the point of use.** A result carries the [`RequestId`] it was requested for; the page
//! drops it unless [`RequestTracker::accepts`] still recognises it.
//!
//! Pure module: no `web-sys`/`wasm-bindgen`, so it is unit-tested natively
//! (`cargo test --lib`) without a browser.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::model::DocId;

/// The independent streams of asynchronous work the page runs.
///
/// Each has its own issue counter, so a background digest check never invalidates an
/// in-flight apply and vice versa — only a *newer request of the same channel*, or a
/// change of document/view, does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// `load()`: the document text, digest, keys and (once) the grants.
    Load,
    /// `PUT`: applying the edited text.
    Apply,
    /// The apply confirmation's diff of the loaded text against the edited one.
    Diff,
    /// The digest re-read triggered by a `GET /meta/version` token change.
    Digest,
}

impl Channel {
    const COUNT: usize = 4;

    fn index(self) -> usize {
        match self {
            Channel::Load => 0,
            Channel::Apply => 1,
            Channel::Diff => 2,
            Channel::Digest => 3,
        }
    }
}

/// The identity one request was issued for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId {
    /// Which stream of work this belongs to.
    pub channel: Channel,
    /// The document as it was when the request went out.
    pub doc: DocId,
    /// The view (key-path prefix, empty for the whole document) it was asked for.
    pub view: String,
    /// Bumped whenever the page changes document or view: everything issued before is
    /// answering a question the page is no longer asking.
    pub epoch: u64,
    /// Per-channel issue counter: a newer request on the same channel supersedes this one.
    pub seq: u64,
}

struct Inner {
    doc: Cell<DocId>,
    view: RefCell<String>,
    epoch: Cell<u64>,
    seq: [Cell<u64>; Channel::COUNT],
}

/// What the page is currently asking for, and the counters that decide whether an answer
/// still belongs to it.
///
/// Cheap to clone (one `Rc`), so a clone can travel into an async block and check its own
/// staleness before it reports anything back.
#[derive(Clone)]
pub struct RequestTracker {
    inner: Rc<Inner>,
}

impl RequestTracker {
    /// A tracker for `doc`, whole document, generation zero.
    pub fn new(doc: DocId) -> Self {
        Self {
            inner: Rc::new(Inner {
                doc: Cell::new(doc),
                view: RefCell::new(String::new()),
                epoch: Cell::new(0),
                seq: Default::default(),
            }),
        }
    }

    /// The document the page is showing.
    pub fn doc(&self) -> DocId {
        self.inner.doc.get()
    }

    /// The view the page is showing (empty for the whole document).
    pub fn view(&self) -> String {
        self.inner.view.borrow().clone()
    }

    /// True if the page is showing the whole document.
    pub fn whole_document(&self) -> bool {
        self.inner.view.borrow().is_empty()
    }

    /// Issue an id for a request about to go out on `channel`, superseding any earlier
    /// request of that channel.
    ///
    /// Takes `&self` because `LoadableComponent::load()` only ever gets one.
    pub fn issue(&self, channel: Channel) -> RequestId {
        let seq = &self.inner.seq[channel.index()];
        seq.set(seq.get() + 1);
        RequestId {
            channel,
            doc: self.inner.doc.get(),
            view: self.inner.view.borrow().clone(),
            epoch: self.inner.epoch.get(),
            seq: seq.get(),
        }
    }

    /// True if `id`'s answer is still the one the page is waiting for.
    pub fn accepts(&self, id: &RequestId) -> bool {
        id.epoch == self.inner.epoch.get()
            && id.doc == self.inner.doc.get()
            && *self.inner.view.borrow() == id.view
            && id.seq == self.inner.seq[id.channel.index()].get()
    }

    /// Show `view` from now on. Returns false (and changes nothing) if it is already the
    /// selected one; otherwise everything in flight becomes stale.
    pub fn set_view(&mut self, view: String) -> bool {
        if *self.inner.view.borrow() == view {
            return false;
        }
        *self.inner.view.borrow_mut() = view;
        self.bump_epoch();
        true
    }

    /// Show `doc` from now on, back at the whole document. Returns false if it is already
    /// the shown one.
    pub fn set_doc(&mut self, doc: DocId) -> bool {
        if self.inner.doc.get() == doc {
            return false;
        }
        self.inner.doc.set(doc);
        self.inner.view.borrow_mut().clear();
        self.bump_epoch();
        true
    }

    /// Drop everything in flight without changing what is shown.
    pub fn invalidate(&mut self) {
        self.bump_epoch();
    }

    fn bump_epoch(&self) {
        self.inner.epoch.set(self.inner.epoch.get() + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_request_is_accepted() {
        let tracker = RequestTracker::new(DocId::Guest(200));
        let id = tracker.issue(Channel::Load);

        assert_eq!(id.doc, DocId::Guest(200));
        assert_eq!(id.view, "");
        assert!(tracker.accepts(&id));
    }

    #[test]
    fn a_view_switch_strands_the_load_it_replaced() {
        // The F4 race: a slow load for the whole document, then a switch to `traefik`,
        // then the slow answer arrives. It must not be applied.
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let slow = tracker.issue(Channel::Load);

        assert!(tracker.set_view("traefik".to_string()));
        let fresh = tracker.issue(Channel::Load);

        assert!(!tracker.accepts(&slow));
        assert!(tracker.accepts(&fresh));
        assert_eq!(slow.view, "");
        assert_eq!(fresh.view, "traefik");
    }

    #[test]
    fn a_switch_back_does_not_resurrect_the_stranded_load() {
        // Same view string, but a different generation of the page: the answer in flight
        // was requested against a digest and a buffer that are both gone.
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let stranded = tracker.issue(Channel::Load);

        tracker.set_view("traefik".to_string());
        tracker.set_view(String::new());

        assert!(!tracker.accepts(&stranded));
    }

    #[test]
    fn selecting_the_shown_view_changes_nothing() {
        let mut tracker = RequestTracker::new(DocId::Datacenter);
        let id = tracker.issue(Channel::Load);

        assert!(!tracker.set_view(String::new()));
        assert!(tracker.accepts(&id));
    }

    #[test]
    fn a_newer_request_supersedes_the_older_one_of_the_same_channel() {
        let tracker = RequestTracker::new(DocId::Guest(200));
        let first = tracker.issue(Channel::Load);
        let second = tracker.issue(Channel::Load);

        assert!(!tracker.accepts(&first));
        assert!(tracker.accepts(&second));
    }

    #[test]
    fn channels_do_not_invalidate_each_other() {
        // A background digest poll (or a re-render's diff) must never make the answer to
        // an in-flight write look stale.
        let tracker = RequestTracker::new(DocId::Guest(200));
        let apply = tracker.issue(Channel::Apply);

        let _ = tracker.issue(Channel::Digest);
        let _ = tracker.issue(Channel::Load);
        let _ = tracker.issue(Channel::Diff);

        assert!(tracker.accepts(&apply));
    }

    #[test]
    fn a_document_switch_strands_everything_and_resets_the_view() {
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        tracker.set_view("traefik".to_string());
        let load = tracker.issue(Channel::Load);
        let apply = tracker.issue(Channel::Apply);

        assert!(tracker.set_doc(DocId::Datacenter));

        assert!(!tracker.accepts(&load));
        assert!(!tracker.accepts(&apply));
        assert_eq!(tracker.view(), "");
        assert!(tracker.whole_document());
        assert!(!tracker.set_doc(DocId::Datacenter));
    }

    #[test]
    fn invalidate_strands_everything_in_flight() {
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let load = tracker.issue(Channel::Load);
        let digest = tracker.issue(Channel::Digest);

        tracker.invalidate();

        assert!(!tracker.accepts(&load));
        assert!(!tracker.accepts(&digest));
        assert_eq!(tracker.view(), "");
    }

    #[test]
    fn a_clone_sees_the_same_state() {
        // The clone that travels into the async block must observe the switch that
        // happened while it was awaiting.
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let in_flight = tracker.clone();
        let id = in_flight.issue(Channel::Load);

        tracker.set_view("netbird".to_string());

        assert!(!in_flight.accepts(&id));
    }
}
