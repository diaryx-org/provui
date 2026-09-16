//! What is wrong with a document, placed where an editor can draw it.
//!
//! prov's [`Finding`](prov::Finding) is a workspace-level answer: a broken link,
//! a term outside a closed vocabulary, a child that does not link back. It names
//! the document and — for the link findings — the *site*, which is either a
//! relation's name or a byte span in the body. That is exactly the right shape
//! for a report and one step short of what an editor needs, which is a place in
//! one of its two panes.
//!
//! This module is that step. A [`Site`] is either a metadata path (the same
//! `Vec<Seg>` a [`MetaLink`] and a flower row carry), a byte
//! range in the body (the same coordinates [`crate::BodyLink`] and leaf's caret
//! are in), or the document as a whole — for the findings that are about the
//! file rather than about anything written in it.
//!
//! ## What is lost on the way, and where
//!
//! prov's `LinkSite::Relation` carries a field **name** and nothing more, so a
//! broken third item of a `contents:` list arrives as "somewhere in `contents`".
//! [`site_of`] recovers the index where it can, by matching the finding's target
//! text against the links that key actually holds — which is exact whenever the
//! list has no duplicate targets, and falls back to the key itself when it does.
//! That is stated here rather than papered over: a frontend drawing a marker on
//! `contents` rather than on `contents[2]` is drawing the truth prov gave it.
//!
//! prov has **no severity of its own**. The split below is this crate's, and it
//! is a narrow one: the findings that are drift or advice rather than a broken
//! structure are warnings, everything else is an error. It is a rendering hint —
//! nothing is suppressed by it — and it lives in one list so a frontend does not
//! grow its own.

use std::path::Path;

use flower_core::Seg;

use crate::links::MetaLink;

/// Where in a document a finding sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
    /// In the metadata, at this path — `[Key("part_of")]`, or
    /// `[Key("contents"), Index(2)]` where the item was recoverable. The path a
    /// frontend compares against its metadata cursor.
    Meta(Vec<Seg>),
    /// In the prose body, at this byte range — the same coordinates
    /// [`crate::BodyLink::span`] and leaf's caret use, so a finding about a body
    /// link washes under it without conversion.
    Body(std::ops::Range<usize>),
    /// About the document rather than about anything written in it: an
    /// unreadable file, a fixity mismatch, an orphan, a config key prov ignores.
    Document,
}

/// How loudly to say it. prov draws no such line; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Something is broken: a link resolves to nothing, a closed vocabulary is
    /// violated, a document cannot be read.
    Error,
    /// Something has drifted or is being advised against, and nothing is broken:
    /// a near-miss spelling in an open vocabulary, a link that resolves only
    /// case-insensitively, a stale label, a confirmation older than the document.
    Warning,
}

/// One of prov's findings, placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Where in the document it sits.
    pub site: Site,
    /// This crate's rendering hint — see [`Severity`].
    pub severity: Severity,
    /// prov's own sentence about it, with the leading `path: ` dropped where it
    /// was there: a per-document panel already knows which document it is
    /// showing, and repeating the path in every row costs the width the message
    /// needs.
    pub message: String,
    /// prov's stable snake_case name for the kind
    /// ([`Finding::kind`](prov::Finding::kind)) — `broken_link`, `unknown_term`,
    /// … For a frontend that branches on the kind rather than reading the prose,
    /// and the `id` an applied highlight carries.
    pub kind: &'static str,
}

/// The findings that are drift or advice rather than a broken structure.
///
/// Spelled as prov's own `kind` strings rather than as a match on the variants,
/// so a kind prov adds lands in the `Error` half — which is the safe default:
/// a new finding shown too loudly is noticed and fixed, one shown too quietly
/// is not.
const WARNING_KINDS: &[&str] = &[
    // An open vocabulary admits new values; this only nudges toward an
    // existing spelling. prov's own doc comment calls it a warning.
    "term_near_miss",
    // Resolves, but only case-insensitively — portable archives want the exact
    // name, and nothing is broken on this machine.
    "case_mismatch",
    // An id link whose label no longer matches the target's title. The id is
    // the real reference and still resolves; the label is decoration.
    "stale_label",
    // The document changed after it was confirmed. An edit after a review is
    // the ordinary course of events, and prov calls it a demotion rather than
    // an error.
    "confirmation_stale",
    // A config surface written by a newer prov. Nothing is broken here; the
    // resolution is to upgrade prov, not to edit the workspace.
    "config_spec_ahead",
    // Diagnosis-only population reports about coverage that is no longer
    // maintained. Both name a decision to make, not a breakage to repair.
    "legacy_body_hash",
    "legacy_deletions_pointer",
];

/// Place one of prov's findings, and translate it.
///
/// `subject` is the document the finding is lodged against
/// ([`Finding::subject`](prov::Finding::subject)), used to trim the message's
/// path prefix. `links` is that document's metadata links, used to recover a
/// list index from a bare relation name — pass an empty slice to skip the
/// refinement and get the key alone.
pub fn place(finding: &prov::Finding, subject: &Path, links: &[MetaLink]) -> Finding {
    Finding {
        site: site_of(finding, links),
        severity: if WARNING_KINDS.contains(&finding.kind()) {
            Severity::Warning
        } else {
            Severity::Error
        },
        message: trim_subject(&finding.to_string(), subject),
        kind: finding.kind(),
    }
}

/// Where a finding sits, with prov's relation name refined to a list index
/// where the document's own links make that unambiguous.
pub fn site_of(finding: &prov::Finding, links: &[MetaLink]) -> Site {
    let Some((site, written)) = link_site(finding) else {
        return field_site(finding);
    };
    match site {
        prov::LinkSite::Body(span) => Site::Body(span.clone()),
        prov::LinkSite::Relation(name) => Site::Meta(refine(name, written.as_deref(), links)),
    }
}

/// The link site a finding carries, and the target text it was written with —
/// `None` for a finding that is not about a link at all.
fn link_site(finding: &prov::Finding) -> Option<(&prov::LinkSite, Option<String>)> {
    use prov::Finding as F;
    Some(match finding {
        F::BrokenLink { site, target, .. }
        | F::CaseMismatch { site, target, .. }
        | F::MalformedId { site, target, .. }
        | F::StaleLabel { site, target, .. } => (site, Some(target.clone())),
        // The written target is `id:<id>` (or the legacy spelling, which this
        // will simply fail to match — costing the index, not the finding).
        F::DanglingId { site, id, .. } => (site, Some(prov::link::id_target(id))),
        F::AmbiguousAlias { site, name, .. } => (site, Some(name.clone())),
        _ => return None,
    })
}

/// The metadata site of a finding that names a *field* rather than a link site,
/// and [`Site::Document`] for everything else.
fn field_site(finding: &prov::Finding) -> Site {
    use prov::Finding as F;
    match finding {
        F::UnknownTerm { field, .. } | F::TermNearMiss { field, .. } => {
            Site::Meta(vec![Seg::Key(field.clone())])
        }
        F::FieldScopeUnresolved { field, .. } => Site::Meta(vec![Seg::Key(field.clone())]),
        _ => Site::Document,
    }
}

/// `[Key(relation)]`, narrowed to the item that was written where the
/// document's links make that unambiguous.
///
/// A scalar relation is already exact. A list needs the target text to pick an
/// item, and two items with the same target make the question unanswerable —
/// in which case the key alone is the honest answer, and the caller draws the
/// marker one level up.
fn refine(relation: &str, written: Option<&str>, links: &[MetaLink]) -> Vec<Seg> {
    let key = vec![Seg::Key(relation.to_string())];
    let Some(written) = written else {
        return key;
    };
    let mut matches = links
        .iter()
        .filter(|link| link.path.first() == key.first() && link.target() == written);
    match (matches.next(), matches.next()) {
        (Some(only), None) => only.path.clone(),
        _ => key,
    }
}

/// prov's `Display` begins every message with the path of the document it is
/// about. A per-document panel knows that already.
fn trim_subject(message: &str, subject: &Path) -> String {
    let prefix = format!("{}: ", subject.display());
    message.strip_prefix(&prefix).unwrap_or(message).to_string()
}
