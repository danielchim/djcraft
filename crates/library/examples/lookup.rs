//! Looks a track up with every provider that searches by name, and prints
//! the candidates. Discogs is included when DISCOGS_TOKEN is set.
//!
//!     cargo run -p djcraft-library --example lookup -- <artist> <title>

use djcraft_library::lookup;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, artist, title] = args.as_slice() else {
        return Err("usage: lookup <artist> <title>".into());
    };
    for provider in lookup::Provider::ALL {
        println!("{}:", provider.label());
        // Discogs needs a token: DISCOGS_TOKEN=... to include it.
        let keys = lookup::Keys {
            discogs_token: std::env::var("DISCOGS_TOKEN").unwrap_or_default(),
            ..Default::default()
        };
        if provider == lookup::Provider::Discogs && keys.discogs_token.is_empty() {
            continue;
        }
        for found in lookup::search(provider, artist, title, &keys)? {
            println!("  {found:?}");
        }
    }
    Ok(())
}
