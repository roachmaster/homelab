//! Client example: the production container for the release actor and
//! the collaborators it binds. Building the collaborators has no side
//! effects; the actor itself is created by `ActorFactory` from the fixed
//! Root configuration path, which this example only reports.
//!
//! `cargo run -p forge-portal-release-actor --example forge-portal-release-actor-client`

use std::path::Path;

use forge_portal_release_actor::actor::ActorFactory;
use forge_portal_release_actor::ports::{ActorClock, RootFiles, RootFilesystem, SystemActorClock};

fn main() {
    let factory = ActorFactory::default();
    println!("actor configuration: {}", factory.config_path.display());

    // The production files refuse a public parent outside its release
    // before touching the filesystem.
    let refused = RootFilesystem.public_parent(Path::new("/srv/release"), Path::new("/tmp/x"));
    println!("public parent outside release: {refused:?}");

    let clock = SystemActorClock::new();
    println!(
        "monotonic seconds since construction: {:.3}",
        clock.monotonic()
    );
}
