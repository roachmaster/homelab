//! The client every scene ships with: it only assembles containers and asks
//! their providers for instances. Run with
//! `cargo run -p forge-scene --example dog_client`.

#[rustfmt::skip]
#[path = "../generated/dog.rs"]
mod dog;

fn main() -> Result<(), forge_foundation::FoundationError> {
    for line in dog::client()? {
        println!("{line}");
    }
    Ok(())
}
