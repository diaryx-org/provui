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
//! `Vec<Seg>` a [`MetaLink`](crate::MetaLink) and a flower row carry), a byte
//! range in the body (the same coordinates [`crate::BodyLink`] and leaf's caret
//! are in), or the document as a whole — for the findings that are about the
//! file rather than about anything written in it.
//!
//! ## Nothing is lost on the way any more
//!
//! Two things this module used to reconstruct, prov now states. A
//! `LinkSite::Relation` carries the list **index** beside the field name, so a
//! broken third item of a `contents:` list arrives as `contents[2]` rather than
//! as "somewhere in `contents`" — this module used to match the finding's
//! target text against the document's own links to recover it, and gave up on
//! a list naming one target twice. And a finding carries its own
//! [severity](prov::Finding::severity), drawn on the same line this crate drew
//! it — drift or advice is a warning, a broken structure is an error — so the
//! list of warning kinds that lived here is gone with the reason for it. A
//! finding prov adds arrives with prov's own judgement of how loud it is.

use std::path::Path;

use flower_core::Seg;

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

/// How loudly to say it — prov's own line, restated as this crate's type so a
/// frontend does not depend on prov to draw a marker.
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
    /// How loudly to say it — see [`Severity`].
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

/// Place one of prov's findings, and translate it.
///
/// `subject` is the document the finding is lodged against
/// ([`Finding::subject`](prov::Finding::subject)), used to trim the message's
/// path prefix.
pub fn place(finding: &prov::Finding, subject: &Path) -> Finding {
    Finding {
        site: site_of(finding),
        severity: match finding.severity() {
            prov::Severity::Warning => Severity::Warning,
            prov::Severity::Error => Severity::Error,
        },
        message: trim_subject(&finding.to_string(), subject),
        kind: finding.kind(),
    }
}

/// Where a finding sits: the relation row — and the item in it, where prov
/// counted one — or the body span, or the document.
pub fn site_of(finding: &prov::Finding) -> Site {
    let Some(site) = link_site(finding) else {
        return field_site(finding);
    };
    match site {
        prov::LinkSite::Body(span) => Site::Body(span.clone()),
        prov::LinkSite::Relation { field, index } => {
            let mut path = vec![Seg::Key(field.clone())];
            if let Some(i) = index {
                path.push(Seg::Index(*i));
            }
            Site::Meta(path)
        }
    }
}

/// The link site a finding carries — `None` for a finding that is not about a
/// link at all.
fn link_site(finding: &prov::Finding) -> Option<&prov::LinkSite> {
    use prov::Finding as F;
    match finding {
        F::BrokenLink { site, .. }
        | F::CaseMismatch { site, .. }
        | F::MalformedId { site, .. }
        | F::StaleLabel { site, .. }
        | F::DanglingId { site, .. }
        | F::AmbiguousAlias { site, .. } => Some(site),
        _ => None,
    }
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

/// prov's `Display` begins every message with the path of the document it is
/// about. A per-document panel knows that already.
fn trim_subject(message: &str, subject: &Path) -> String {
    let prefix = format!("{}: ", subject.display());
    message.strip_prefix(&prefix).unwrap_or(message).to_string()
}
