//! A smoke check against a real workspace on disk: classify a document's keys,
//! list its links, and say where each one lands.
use provui_core::DocumentSession;
use provui_core::links::links_in;
use provui_core::workspace::{Destination, WorkspaceView, resolve_without_workspace};

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("a document"));
    let view = WorkspaceView::discover(&path).expect("discovery");
    match &view {
        Some(v) => println!("workspace: {}", v.root_dir().display()),
        None => println!("workspace: none"),
    }
    let facets = view
        .as_ref()
        .map(|v| v.facets().clone())
        .unwrap_or_default();
    let schema = view.as_ref().map(|v| v.schema_for(&path));
    let session =
        DocumentSession::open_managed(&path, schema, facets.managed_key_names()).expect("open");

    println!("\nkeys:");
    for (key, facet) in facets.classify(session.meta()) {
        let flags = [
            facet.structural().then_some("structural"),
            facet.managed().then_some("managed"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(",");
        println!("  {key:<14} {:<9} {flags}", facet.kind());
    }

    println!("\nlinks:");
    for link in links_in(session.meta(), &facets) {
        let landing = match &view {
            Some(v) => v.resolve(&path, &link),
            None => resolve_without_workspace(&path, &link),
        };
        let mark = if matches!(landing, Destination::Document { exists: true, .. }) {
            "→"
        } else {
            "·"
        };
        println!(
            "  {mark} {:<10} {:<22} {}",
            link.relation.name,
            link.display(),
            landing.describe()
        );
    }
}
