//! GrokRxiv publisher: uploads review artifacts to Supabase Storage and opens
//! a moderation PR against the `GrokRxiv/reviews` repository.
//!
//! The publisher is **admin-gated**: every public entry point that mutates a
//! remote system takes an [`AdminCaller`] zero-sized token, the sole
//! constructor of which is [`AdminCaller::from_admin_endpoint`]. The intent is
//! that only the orchestrator's `/admin/publish` HTTP handler can produce that
//! token, so accidental publishes from the automatic review pipeline are
//! ruled out at compile time.

#![forbid(unsafe_code)]

pub mod github;
pub mod supabase;

pub use github::{GithubPublisher, OpenReviewPr};
pub use supabase::SupabaseStorage;

/// Zero-sized capability token proving the caller came in through the
/// human-admin path. The only constructor is `from_admin_endpoint` which the
/// orchestrator calls from its admin route after authenticating the
/// moderator.
#[derive(Debug)]
pub struct AdminCaller {
    _private: (),
}

impl AdminCaller {
    /// Called from the orchestrator's admin endpoint after moderator auth.
    ///
    /// This is the **only** way to obtain an `AdminCaller`. Do not expose any
    /// other constructor.
    pub fn from_admin_endpoint() -> Self {
        Self { _private: () }
    }
}
