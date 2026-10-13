//! Scans a folder into a library database and prints what changed.
//!
//!     cargo run -p djcraft-library --example scan -- <library.db> <folder>

use std::path::Path;

use djcraft_library::{Library, TrackQuery};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, db, folder] = args.as_slice() else {
        return Err("usage: scan <library.db> <folder>".into());
    };
    let mut library = Library::open(Path::new(db))?;
    let source = library.add_folder(Path::new(folder))?;
    let summary = library.scan(source.id, |_| {})?;
    let total = library.tracks(&TrackQuery::default())?.len();
    println!("{summary:?}; {total} tracks in the library");
    Ok(())
}
