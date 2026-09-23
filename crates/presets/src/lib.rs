//! Read-only model presets, from the [AI Model Directory][directory].
//!
//! A preset describes what a model can do and what it costs: vision and
//! reasoning capability, tool and structured-output support, price per
//! million tokens, and the context window. [`Catalog`] holds every provider
//! and model the directory publishes and answers lookups against them.
//!
//! [directory]: https://github.com/The-Best-Codes/ai-model-directory
//!
//! ## Separate from configured models, on purpose
//!
//! These are *not* the models a deployment can call. A configured model is a
//! row a user wrote — a `base_url`, a credential, a wire — and lives behind
//! `GET /api/models` in the service. A preset is a third party's catalogue of
//! offers, read-only and identical for every user. The design note in
//! `internal-docs/a.md` rules out materializing a catalog into user rows:
//! once inserted, "the user chose this model" and "we seeded this model"
//! become indistinguishable. Keeping the catalog in its own unit is what
//! makes that separation structural rather than a convention.
//!
//! ## Fetched, not compiled in
//!
//! [`Catalog::fetch`] pulls [`DEFAULT_URL`] at run time. Model prices and
//! release dates move faster than this crate's release cadence, and a stale
//! catalog is worse than a catalog fetched at startup. A caller decides what
//! a failed fetch means; this crate returns an [`Error`] and stops.
//!
//! ```no_run
//! # async fn run() -> Result<(), presets::Error> {
//! let catalog = presets::Catalog::fetch(presets::DEFAULT_URL).await?;
//! println!("{} models across {} providers", catalog.len(), catalog.providers().len());
//! # Ok(())
//! # }
//! ```
//!
//! ## Deliberately absent
//!
//! - **No persistence.** This crate never writes a preset anywhere; it parses
//!   a catalog and hands it back whole. A caller may store it — the service
//!   does, in a table of its own — but the write is the caller's, not this
//!   crate's.
//! - **No credentials, no base URL a request uses.** `api_base_url` is
//!   carried because the directory states it; nothing here sends a request to
//!   it.
//! - **No ranking, no recommendations.** A [`Query`] narrows; it does not
//!   order by relevance. Choosing between two models is the caller's.

mod catalog;
mod error;
mod fetch;
mod wire;

pub use catalog::{Capabilities, Catalog, Limits, Modalities, Preset, Pricing, Provider, Query};
pub use error::Error;

/// Where the default presets come from.
///
/// The `all.min.json` file in the AI Model Directory — every provider and
/// model the project tracks, minified. Used when a deployment configures no
/// other source.
pub const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/The-Best-Codes/ai-model-directory/main/data/all.min.json";
