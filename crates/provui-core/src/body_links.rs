//! The links a document's **prose** declares — where each one sits in the body
//! text, what it says, and what shape of target it names.
//!
//! [`crate::links`] is the frontmatter half of this question and answers it with
//! a metadata path; this is the body half, and answers it with a byte range.
//! They are separate modules because they are separate address spaces: a
//! [`MetaLink`](crate::MetaLink) sits at `contents[2]`, a [`BodyLink`] sits at
//! bytes 412…431 of the prose, and the two never coincide — a session's metadata
//! model and its leaf buffer share no offsets by construction.
//!
//! What they do share is the *target*: both carry a [`prov::Link`] and a
//! [`TargetKind`], so one resolver answers for both (see [`crate::AnyLink`] and
//! [`crate::WorkspaceView::resolve`]). A frontend that follows a link from the
//! metadata cursor and a frontend that follows one from the body caret are
//! asking the same question of the same workspace, and neither should have to
//! know which pane it came from.
//!
//! The scan itself is prov's — [`prov::link::scan_body_links`], the single
//! body-scan seam prov's own census, check and rename use. That matters more
//! than it looks: getting this right means being code-aware (a `[[x]]` inside a
//! fence is not a link), reaching into footnote definitions (which a walk from
//! the document root does not), and never mistaking bracket prose for a link.
//! prov has all of that behind one call, and a second implementation here would
//! be a second set of those bugs.

use std::ops::Range;
use std::path::PathBuf;

use prov::{ContentFormat, Link};

use crate::links::{TargetKind, kind_of};
use crate::session::SessionError;

/// One link written in a document's prose body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyLink {
    /// Byte range of the whole link construct **within the body text** — the
    /// buffer a [`leaf_core::Doc`] holds, not the file on disk. A prov document
    /// is frontmatter plus body, and the frontmatter is not in these
    /// coordinates: offset 0 is the first byte of the prose.
    ///
    /// The same coordinates prov's own [`LinkSite::Body`](prov::LinkSite)
    /// reports, and the same ones a
    /// [`leaf_core::Highlight`] is painted in, so a
    /// finding about a body link can be washed under it without any conversion.
    pub span: Range<usize>,
    /// The parsed link: its label, its target, and the wrapper it was written
    /// in. [`Link::render`](prov::Link::render) puts it back the way it came.
    pub link: Link,
    /// What shape of thing the target names, by syntax — classified by the same
    /// helper [`MetaLink`](crate::MetaLink) uses, so `id:ajp7eq` means the same
    /// thing in prose as it does in `contents`.
    pub kind: TargetKind,
}

impl BodyLink {
    /// The target as written, locator and all.
    pub fn target(&self) -> &str {
        &self.link.target
    }

    /// The `#locator` suffix, when the target has one.
    pub fn locator(&self) -> Option<&str> {
        self.link.locator()
    }

    /// What to put on a status line: the link's label when it was written with
    /// one, and the target otherwise.
    pub fn display(&self) -> &str {
        self.link.label.as_deref().unwrap_or(&self.link.target)
    }
}

/// Every link `body` declares, in source order.
///
/// `format` is the grammar the prose is written in, which is what makes the scan
/// syntax-aware rather than lexical. Images are left out: an `![alt](target)` is
/// one of prov's body links too, but it names a *payload* rather than a document
/// — following one would open a picture in a text editor — and prov's own census
/// skips them for the same reason.
///
/// Reference-style (`[a][ref]`) and autolink forms are left out by prov's scan,
/// which keeps only the inline `[label](target)` shape it can resolve and
/// rewrite in place. Wikilinks are in.
///
/// The `Result` is prov's scan seam kept honest rather than a failure this can
/// currently produce: [`scan_body_links`](prov::link::scan_body_links) degrades
/// to the lexical wikilink finder when the prose will not parse, so today the
/// answer is always `Ok`. Whether that degradation is the right one is prov's
/// call to revisit, and a caller that would then have to start handling a parse
/// error should not have to be recompiled to find out.
pub fn body_links(body: &str, format: ContentFormat) -> Result<Vec<BodyLink>, SessionError> {
    // prov's scan takes a *path* because that is what a document has; it reads
    // nothing but the extension off it, to pick the grammar. We already know
    // the grammar, so the path is one synthesized from it — the one place this
    // crate spells a filename that is not a file.
    let named = PathBuf::from(format!("body.{}", format.extension()));
    Ok(prov::link::scan_body_links(&named, body)
        .into_iter()
        .filter(|found| !found.image)
        .map(|found| BodyLink {
            span: found.span,
            kind: kind_of(&found.link),
            link: found.link,
        })
        .collect())
}

/// The link covering byte `offset`, if one does — the caret question.
///
/// Half-open, like every other span here: a caret sitting on the byte just past
/// a link's last is standing *after* it, which is where a reader who has just
/// typed the closing bracket is. `links` is expected in source order (which is
/// what [`body_links`] returns); the first match wins, and prov's scan does not
/// produce overlapping links.
pub fn body_link_at(links: &[BodyLink], offset: usize) -> Option<&BodyLink> {
    links
        .iter()
        .find(|link| link.span.start <= offset && offset < link.span.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "\
# Notes

See [the other note](other.md) and [[notes/third.md|Third]].

A `[[fenced.md]]` mention is not a link, and neither is [this] bracket prose.

Also [by id](id:ajp7eq), [outside](https://example.com/) and ![a picture](pic.png).
";

    fn links() -> Vec<BodyLink> {
        body_links(BODY, ContentFormat::Markdown).expect("scan")
    }

    fn at(links: &[BodyLink], target: &str) -> BodyLink {
        links
            .iter()
            .find(|l| l.target() == target)
            .unwrap_or_else(|| panic!("no link to {target}"))
            .clone()
    }

    #[test]
    fn finds_both_spellings_and_leaves_prose_and_code_alone() {
        let links = links();
        let targets: Vec<&str> = links.iter().map(|l| l.target()).collect();
        assert_eq!(
            targets,
            [
                "other.md",
                "notes/third.md",
                "id:ajp7eq",
                "https://example.com/"
            ],
            "markdown links and wikilinks, no code, no prose, no image"
        );
    }

    #[test]
    fn a_span_is_a_byte_range_in_the_body_that_slices_back_to_the_link() {
        let links = links();
        let other = at(&links, "other.md");
        assert_eq!(&BODY[other.span.clone()], "[the other note](other.md)");
        let third = at(&links, "notes/third.md");
        assert_eq!(&BODY[third.span.clone()], "[[notes/third.md|Third]]");
        assert_eq!(third.display(), "Third");
    }

    #[test]
    fn a_target_is_classified_the_way_a_metadata_target_is() {
        let links = links();
        assert_eq!(at(&links, "other.md").kind, TargetKind::Path);
        assert_eq!(at(&links, "id:ajp7eq").kind, TargetKind::Id);
        assert_eq!(
            at(&links, "https://example.com/").kind,
            TargetKind::External
        );
    }

    #[test]
    fn the_caret_question_is_the_link_the_caret_is_inside() {
        let links = links();
        let other = at(&links, "other.md");

        assert!(
            body_link_at(&links, other.span.start).is_some(),
            "on the `[`"
        );
        assert_eq!(
            body_link_at(&links, other.span.start + 1).map(BodyLink::target),
            Some("other.md"),
            "inside the label"
        );
        assert!(
            body_link_at(&links, other.span.end).is_none_or(|l| l.target() != "other.md"),
            "the byte past the end is past the link"
        );
        assert!(
            body_link_at(&links, 0).is_none(),
            "the heading is not a link"
        );
    }
}
