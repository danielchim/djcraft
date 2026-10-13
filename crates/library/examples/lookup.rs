//! Looks a track up with every provider and prints the candidates.
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
        for found in lookup::search(provider, artist, title)? {
            println!("  {found:?}");
        }
    }
    Ok(())
}
