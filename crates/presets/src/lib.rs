//! Read-only model presets, from [models.dev][directory].
//!
//! The catalog describes models in three units: the serving [`Provider`], the
//! [`CreatorModel`] as the lab that made it describes it, and the
//! [`Offering`] of one by the other — what it costs there, and what that
//! provider states differently. [`Catalog`] holds all three and answers
//! lookups against them.
//!
//! [directory]: https://models.dev
//!
//! ## Separate from configured models, on purpose
//!
//! These are *not* the models a deployment can call. A configured model is a
//! row a user wrote — a `base_url`, a credential, a wire — and lives behind
//! `GET /api/models` in the service. A preset is a third party's catalogue of
//! offers, read-only and identical for every user. Materializing a catalog
//! into user rows is ruled out: once inserted, "the user chose this model"
//! and "we seeded this model" become indistinguishable. Keeping the catalog
//! in its own unit is what makes that separation structural rather than a
//! convention.
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
//! println!(
//!     "{} offerings of {} models across {} providers",
//!     catalog.len(),
//!     catalog.models().len(),
//!     catalog.providers().len(),
//! );
//! # Ok(())
//! # }
//! ```
//!
//! ## Deliberately absent
//!
//! - **No persistence.** This crate never writes a preset anywhere; it parses
//!   a catalog and hands it back whole. A caller may store it — the service
//!   does, in tables of its own — but the write is the caller's, not this
//!   crate's.
//! - **No credentials, no base URL a request uses.** A provider's `api` is
//!   carried because the catalog states it; nothing here sends a request to
//!   it.
//! - **No ranking, no filtering.** Choosing between two models is the
//!   caller's, and so is narrowing a list — the service does it in its store.

mod catalog;
mod error;
mod fetch;
mod wire;

pub use catalog::{
    Catalog, Cost, CreatorModel, Limit, Modalities, Offering, Overrides, Preset, Provider, Serving,
    Spec,
};
pub use error::Error;

/// Where the default presets come from.
///
/// models.dev's `catalog.json` — every serving provider with the models it
/// serves, and the provider-agnostic model metadata beside them, in one
/// response. Specialized model types (upstream's `decision` models) are left
/// out by default, which is what a chat service wants. Used when a deployment
/// configures no other source.
pub const DEFAULT_URL: &str = "https://models.dev/catalog.json";
