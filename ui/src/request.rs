//! Request identity: what an async result was asked for, and whether that is still what
//! the page wants.
//!
//! Every asynchronous thing the page does — a load, a row write, the "Edit as text"
//! subtree fetch — is issued *for* one document and one generation of the page. `LoadableComponentMaster` spawns a fresh `load()` per
//! `Msg::Load` with no cancellation (`COMP/src/loadable_component.rs`, `Msg::Load`), so
//! two loads can be in flight at once and finish in either order. Applying the
//! second-to-last answer over the last one is how a page ends up showing — and then
//! writing — state that belongs to a document it has already left
//! (`docs/REVIEW-2026-09-07.md` F4).
//!
//! The rule this module implements: **name the thing you are matching against, and check
//! it at the point of use.** A result carries the [`RequestId`] it was requested for; the
//! page drops it unless [`RequestTracker::accepts`] still recognises it.
//!
//! What a request is *about* — the row path being written, the subtree being fetched as
//! text — travels in the message payload rather than in the identity: the tree always
//! shows the whole document, so a per-channel sequence plus the document and the epoch is
//! the whole of "is this still the answer we want".
//!
//! Pure module: no `web-sys`/`wasm-bindgen`, so it is unit-tested natively
//! (`cargo test --lib`) without a browser.

use std::cell::Cell;
use std::rc::Rc;

use crate::model::DocId;

/// The independent streams of asynchronous work the page runs.
///
/// Each has its own issue counter, so a background digest check never invalidates an
/// in-flight write and vice versa — only a *newer request of the same channel*, a change
/// of document, or an explicit invalidation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// `load()`: the document, its digest, the grants, the registrations, the guest.
    Load,
    /// A row write (`PUT`/`DELETE`), or the text dialog's apply.
    Apply,
    /// The "Edit as text" dialog's fetch of one subtree as YAML.
    Text,
}

impl Channel {
    const COUNT: usize = 3;

    fn index(self) -> usize {
        match self {
            Channel::Load => 0,
            Channel::Apply => 1,
            Channel::Text => 2,
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
    /// Bumped whenever the page changes document or explicitly invalidates: everything
    /// issued before is answering a question the page is no longer asking.
    pub epoch: u64,
    /// Per-channel issue counter: a newer request on the same channel supersedes this one.
    pub seq: u64,
}

struct Inner {
    doc: Cell<DocId>,
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
    /// A tracker for `doc`, generation zero.
    pub fn new(doc: DocId) -> Self {
        Self {
            inner: Rc::new(Inner {
                doc: Cell::new(doc),
                epoch: Cell::new(0),
                seq: Default::default(),
            }),
        }
    }

    /// The document the page is showing.
    pub fn doc(&self) -> DocId {
        self.inner.doc.get()
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
            epoch: self.inner.epoch.get(),
            seq: seq.get(),
        }
    }

    /// True if `id`'s answer is still the one the page is waiting for.
    pub fn accepts(&self, id: &RequestId) -> bool {
        id.epoch == self.inner.epoch.get()
            && id.doc == self.inner.doc.get()
            && id.seq == self.inner.seq[id.channel.index()].get()
    }

    /// Show `doc` from now on. Returns false if it is already the shown one; otherwise
    /// everything in flight becomes stale.
    pub fn set_doc(&mut self, doc: DocId) -> bool {
        if self.inner.doc.get() == doc {
            return false;
        }
        self.inner.doc.set(doc);
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
        // A reload triggered by the version poll must never make the answer to an
        // in-flight write look stale, nor the text dialog's fetch strand the load beside
        // it.
        let tracker = RequestTracker::new(DocId::Guest(200));
        let apply = tracker.issue(Channel::Apply);

        let load = tracker.issue(Channel::Load);
        let text = tracker.issue(Channel::Text);

        assert!(tracker.accepts(&apply));
        assert!(tracker.accepts(&load));
        assert!(tracker.accepts(&text));

        let text2 = tracker.issue(Channel::Text);
        assert!(!tracker.accepts(&text));
        assert!(tracker.accepts(&text2));
        // A newer text fetch supersedes the older one, but the write beside it is
        // untouched.
        assert!(tracker.accepts(&apply));
    }

    #[test]
    fn a_document_switch_strands_everything() {
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let load = tracker.issue(Channel::Load);
        let apply = tracker.issue(Channel::Apply);

        assert!(tracker.set_doc(DocId::Datacenter));

        assert!(!tracker.accepts(&load));
        assert!(!tracker.accepts(&apply));
        assert_eq!(tracker.doc(), DocId::Datacenter);
        assert!(!tracker.set_doc(DocId::Datacenter));
    }

    #[test]
    fn invalidate_strands_everything_in_flight() {
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let load = tracker.issue(Channel::Load);
        let text = tracker.issue(Channel::Text);

        tracker.invalidate();

        assert!(!tracker.accepts(&load));
        assert!(!tracker.accepts(&text));
    }

    #[test]
    fn a_reload_invalidates_a_stale_write_answer() {
        // `docs/REVIEW-2026-09-08-pass2.md` P8: `Msg::Reload` does not change the
        // document (so `set_doc` never fires to bump the epoch on its own), but it must
        // still strand a request issued before it — otherwise a 409 for a write issued
        // before the reload lands after it and is rendered over a tree with nothing
        // pending about it.
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let apply = tracker.issue(Channel::Apply);
        let text = tracker.issue(Channel::Text);

        // The Reload handler.
        tracker.invalidate();
        let reload = tracker.issue(Channel::Load);

        assert!(!tracker.accepts(&apply));
        assert!(!tracker.accepts(&text));
        assert!(tracker.accepts(&reload));
    }

    #[test]
    fn a_clone_sees_the_same_state() {
        // The clone that travels into the async block must observe the change that
        // happened while it was awaiting.
        let mut tracker = RequestTracker::new(DocId::Guest(200));
        let in_flight = tracker.clone();
        let id = in_flight.issue(Channel::Load);

        tracker.set_doc(DocId::Datacenter);

        assert!(!in_flight.accepts(&id));
    }
}
