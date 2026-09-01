//! The rule builders [`config_schema`](crate::config_schema) is written in.
//!
//! Public, and deliberately so. A [`Schema`](flower_core::Schema) resolves a
//! path by **first match wins** (`rule_for` returns the first rule whose pattern
//! matches), so an application that keeps its own keys in a config document —
//! prov permits a config surface to carry fields it never reads — composes by
//! *prepending* its rules to [`config_rules`](crate::config_schema::config_rules):
//!
//! ```ignore
//! let mut rules = my_app_rules();              // `myapp.*`, and any narrowing
//! rules.extend(provui_core::config_schema::config_rules(&config));
//! let schema = Schema::new(rules);
//! ```
//!
//! Prepending is what makes an *overlay* possible rather than only an addition:
//! an app that wants a narrower vocabulary for a key this crate governs openly
//! (`views.*.group`, say) puts its own rule first and shadows the generic one.
//! Appending would leave the generic rule winning and the app's rule dead.
//!
//! These builders exist so an overlay's rows come out looking like the ones
//! beside them — same tints, same consequence vocabulary — without every
//! frontend restating what a "costly" field looks like.

use fig::Value;
use flower_core::schema::{Constraint, FieldRule};
use flower_core::{
    Consequence, FieldType, Icon, PathPat, Presentation, SegPat, Severity, Term, Tint,
};

/// A dotted path pattern: `*` means "any key at this depth" (`fields.*.type`),
/// `[]` means "each item of this sequence" (`audiences.[].name`).
///
/// The two are not interchangeable and the difference is easy to get wrong in
/// the silent direction: an `*` against a sequence matches nothing, governs
/// nothing, and reports no error — the rows simply come out untyped.
pub fn path(segments: &[&str]) -> PathPat {
    PathPat(
        segments
            .iter()
            .map(|s| match *s {
                "*" => SegPat::AnyKey,
                "[]" => SegPat::EachItem,
                key => SegPat::Key(key.to_string()),
            })
            .collect(),
    )
}

/// A titled, icon-bearing presentation.
pub fn present(title: &str, icon: Icon) -> Presentation {
    Presentation::default().title(title).icon(icon)
}

/// A vocabulary term with an optional one-line gloss (an empty gloss is none).
pub fn term(value: &str, gloss: &str) -> Term {
    Term::value(value).description_opt((!gloss.is_empty()).then_some(gloss))
}

/// A free-text field.
pub fn text(at: PathPat, title: &str, icon: Icon) -> FieldRule {
    FieldRule::new(at)
        .ty(FieldType::Str)
        .present(present(title, icon))
}

/// A boolean field.
pub fn toggle(at: PathPat, title: &str) -> FieldRule {
    FieldRule::new(at)
        .ty(FieldType::Bool)
        .present(present(title, Icon::Toggle))
}

/// A closed pick-list: anything else is a value prov would ignore, so the editor
/// rejects it rather than writing it and letting the default quietly win.
pub fn choice(at: PathPat, title: &str, icon: Icon, values: &[(&str, &str)]) -> FieldRule {
    choice_terms(
        at,
        title,
        icon,
        values.iter().map(|(v, g)| term(v, g)).collect(),
    )
}

/// [`choice`] over terms already built.
pub fn choice_terms(at: PathPat, title: &str, icon: Icon, values: Vec<Term>) -> FieldRule {
    FieldRule::new(at)
        .ty(FieldType::Str)
        .constraint(Constraint::Enum {
            values,
            closed: true,
        })
        .present(present(title, icon))
}

/// An offered-but-not-enforced pick-list — for a vocabulary this crate cannot
/// see the whole of (see `metadata.format`), or one where a value it does not
/// know is still legitimate (see `views.*.group`).
pub fn open_choice(at: PathPat, title: &str, icon: Icon, values: &[(&str, &str)]) -> FieldRule {
    open_choice_terms(
        at,
        title,
        icon,
        values.iter().map(|(v, g)| term(v, g)).collect(),
    )
}

/// [`open_choice`] over terms already built.
pub fn open_choice_terms(at: PathPat, title: &str, icon: Icon, values: Vec<Term>) -> FieldRule {
    FieldRule::new(at)
        .ty(FieldType::Str)
        .constraint(Constraint::Enum {
            values,
            closed: false,
        })
        .present(present(title, icon))
}

/// Mark a rule as one where *every* answer costs the same thing.
///
/// For the axes with no safe direction: whichever value you land on, the
/// workspace gets rewritten. The tint is what draws the row; the
/// [`Consequence`] is what Apply reads before carrying the change out.
///
/// Both, not either — they answer different questions. The tint says how
/// loudly to draw a field the reader is *looking* at; the consequence says
/// what happens if they go through with it. A row can be drawn calmly and
/// still be expensive to change.
pub fn costly(mut rule: FieldRule, tint: Tint, why: &str) -> FieldRule {
    let present = std::mem::take(&mut rule.present);
    rule.present = present.tint(tint).description(why);
    rule.on_change(Consequence::always(why).severity(Severity::Confirm))
}

/// Mark one *answer* as the costly one.
///
/// The shape most of a workspace config actually has: `recycle_bin` is not
/// dangerous, `recycle_bin: false` is; `fields.*.reify` is not expensive,
/// turning it *on* is. Marking the field would warn on the harmless answer too,
/// and a warning that fires either way is how a reader learns to click through
/// warnings.
///
/// No tint, deliberately. A tint is a property of the row, and the row is not
/// dangerous — one of its answers is. The warning belongs to the moment the
/// answer is chosen, which is what the consequence carries.
pub fn costly_when(
    rule: FieldRule,
    value: impl Into<Value>,
    severity: Severity,
    why: &str,
) -> FieldRule {
    rule.on_change(Consequence::when(value, why).severity(severity))
}
