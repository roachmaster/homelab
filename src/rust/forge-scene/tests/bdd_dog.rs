//! Step bindings for features/dog.feature over the generated dog scene.
//!
//! Each binding is fixed: Given checks the capability exists in the wiring,
//! When selects an environment (a set of bindings), Then calls ports through
//! the container's providers and compares with what the scene data says.

#[rustfmt::skip]
#[path = "../generated/dog.rs"]
mod dog;

use dog::*;
use forge_foundation::{Assemble, FoundationError, LazySingletonProvider, Provide};
use forge_scene::bdd::{ensure, verify, Steps};
use std::sync::Arc;

#[derive(Default)]
struct World {
    scene: Option<Result<DogScene, FoundationError>>,
    collars: Vec<(String, Arc<dyn Collar>)>,
    dogs: Vec<Arc<dyn Dog>>,
    sessions: Vec<Arc<dyn Training>>,
    registry: Option<LazySingletonProvider<dyn Chip, RegistryChipFactory>>,
}

impl World {
    fn wire(&mut self, env: DogEnv) -> Result<(), String> {
        self.scene = Some(DogScene::assemble(env));
        Ok(())
    }

    fn scene(&self) -> Result<&DogScene, String> {
        match &self.scene {
            Some(Ok(scene)) => Ok(scene),
            Some(Err(error)) => Err(format!("the scene did not assemble: {error}")),
            None => Err("no environment was chosen".into()),
        }
    }

    fn dog(&self) -> Result<Arc<dyn Dog>, String> {
        self.scene()?
            .dog()
            .provider()
            .and_then(|p| p.provide())
            .map_err(|e| e.to_string())
    }

    fn owner(&self) -> Result<String, String> {
        let dog = self.dog()?;
        dog.owner(Encounter("hello".into()))
            .map(|n| n.0)
            .map_err(|e| e.to_string())
    }

    fn collar_tag(&self) -> Result<String, String> {
        let collar = self.scene()?.collar().provider().and_then(|p| p.provide());
        let collar = collar.map_err(|e| e.to_string())?;
        collar
            .read(Glance("look".into()))
            .map(|t| t.0)
            .map_err(|e| e.to_string())
    }

    fn chip_record(&self) -> Result<String, String> {
        let chip = self.scene()?.chip().provider().and_then(|p| p.provide());
        let chip = chip.map_err(|e| e.to_string())?;
        chip.scan(Scan("shelter".into()))
            .map(|r| r.0)
            .map_err(|e| e.to_string())
    }

    fn mood(&self) -> Result<String, String> {
        self.dog()?
            .settle(Crate("night".into()))
            .map(|m| m.0)
            .map_err(|e| e.to_string())
    }
}

fn arrange(_: &mut World, _: &[String]) -> Result<(), String> {
    Ok(())
}

fn steps() -> Steps<World> {
    Steps::new()
        // Capabilities: each one is checked against the yard, where nothing
        // optional is bound, so the capability is proven to be optional.
        .given("a dog can have an owner", |_, _| {
            let yard = wire_yard().map_err(|e| e.to_string())?;
            ensure(
                yard.collar().is_default() && yard.chip().is_default(),
                "collar and chip should be optional collaborators with empty defaults",
            )
        })
        .given("a dog can be crate trained", |_, _| {
            let yard = wire_yard().map_err(|e| e.to_string())?;
            ensure(yard.training().is_default(), "training should be optional")
        })
        .given("a collar can be engraved with any name", arrange)
        .given("a household can keep one dog", |w, _| w.wire(DogEnv::Prod))
        .given("a dog can be trained in sessions", |w, _| {
            w.wire(DogEnv::Prod)
        })
        .given("a chip registry can wait until someone scans", |w, _| {
            w.registry = Some(LazySingletonProvider::new(RegistryChipFactory));
            Ok(())
        })
        .given("a kennel needs a dog", arrange)
        .given("a dog may be watched by a sitter", |w, _| {
            w.wire(DogEnv::Prod)
        })
        .given("any dog can be swapped for a pretend one", arrange)
        // States: which bindings are in place.
        .when("it wears a collar", |w, _| w.wire(DogEnv::Prod))
        .when("it has no collar but is chipped", |w, _| {
            w.wire(DogEnv::Shelter)
        })
        .when("it has no collar and no chip", |w, _| w.wire(DogEnv::Yard))
        .when("it is crate trained", |w, _| w.wire(DogEnv::Prod))
        .when("it is not crate trained", |w, _| w.wire(DogEnv::Yard))
        .when("collars are engraved for {} and {}", |w, names| {
            w.wire(DogEnv::Prod)?;
            for name in names {
                let collar = w.scene()?.engraved_collar().get(name.clone());
                w.collars
                    .push((name.clone(), collar.map_err(|e| e.to_string())?));
            }
            Ok(())
        })
        .when("the family greets the dog twice", |w, _| {
            for _ in 0..2 {
                let dog = w.dog()?;
                w.dogs.push(dog);
            }
            Ok(())
        })
        .when("it has two training sessions", |w, _| {
            let provider = w
                .scene()?
                .training()
                .provider()
                .map_err(|e| e.to_string())?;
            for _ in 0..2 {
                w.sessions
                    .push(provider.provide().map_err(|e| e.to_string())?);
            }
            Ok(())
        })
        .when("nobody has scanned the chip yet", arrange)
        .when("there is no dog to board", |w, _| w.wire(DogEnv::Lonely))
        .when(
            "the sitter is watching and the dog meets one stranger",
            |w, _| w.owner().map(|_| ()),
        )
        .when("we are testing", |w, _| w.wire(DogEnv::Test))
        // Behaviours: observed through the container's providers.
        .then("a stranger can read its owner's name", |w, _| {
            let (tag, owner) = (w.collar_tag()?, w.owner()?);
            ensure(
                tag == "Ana" && owner == "Ana",
                format!("collar {tag:?}, owner {owner:?}"),
            )
        })
        .then("a shelter scan finds its owner", |w, _| {
            let (tag, record, owner) = (w.collar_tag()?, w.chip_record()?, w.owner()?);
            ensure(
                tag.is_empty() && record == "Ana" && owner == "Ana",
                format!("collar {tag:?}, chip {record:?}, owner {owner:?}"),
            )
        })
        .then("it is treated as a stray", |w, _| {
            let owner = w.owner()?;
            ensure(owner == "stray", format!("the dog answered {owner:?}"))
        })
        .then("it is calm in a crate", |w, _| {
            let mood = w.mood()?;
            ensure(mood == "calm", format!("the dog was {mood:?}"))
        })
        .then("it whines in a crate", |w, _| {
            let mood = w.mood()?;
            ensure(mood == "whines", format!("the dog was {mood:?}"))
        })
        .then("each collar shows its own name", |w, _| {
            ensure(w.collars.len() == 2, "two collars should exist")?;
            for (name, collar) in &w.collars {
                let tag = collar
                    .read(Glance("look".into()))
                    .map_err(|e| e.to_string())?
                    .0;
                ensure(&tag == name, format!("the {name} collar shows {tag:?}"))?;
            }
            ensure(
                !Arc::ptr_eq(&w.collars[0].1, &w.collars[1].1),
                "the collars are one collar",
            )
        })
        .then("they greet the same dog both times", |w, _| {
            ensure(
                Arc::ptr_eq(&w.dogs[0], &w.dogs[1]),
                "two different dogs answered",
            )
        })
        .then("each session starts fresh", |w, _| {
            ensure(
                !Arc::ptr_eq(&w.sessions[0], &w.sessions[1]),
                "both sessions were one session",
            )
        })
        .then("the registry has not been looked up", |w, _| {
            let registry = w.registry.as_ref().ok_or("no registry")?;
            ensure(
                !registry.is_initialized(),
                "the registry was looked up early",
            )
        })
        .then("the first scan looks it up once", |w, _| {
            let registry = w.registry.as_ref().ok_or("no registry")?;
            let first = registry.provide().map_err(|e| e.to_string())?;
            let second = registry.provide().map_err(|e| e.to_string())?;
            ensure(
                registry.is_initialized() && Arc::ptr_eq(&first, &second),
                "the registry should be looked up exactly once",
            )
        })
        .then("the kennel will not open", |w, _| match &w.scene {
            Some(Err(FoundationError::Freeze(_))) => Ok(()),
            Some(Ok(_)) => Err("the kennel opened with no dog".into()),
            other => Err(format!(
                "unexpected outcome: {:?}",
                other.as_ref().map(|r| r.as_ref().err())
            )),
        })
        .then("it says a dog is missing", |w, _| match &w.scene {
            Some(Err(error)) => ensure(
                error.missing_binding() == Some("dog"),
                format!("the refusal was {error}"),
            ),
            _ => Err("there was no refusal".into()),
        })
        .then("the sitter has seen one visit", |w, _| {
            let calls = w.scene()?.sitter().calls();
            ensure(calls == 1, format!("the sitter saw {calls} visits"))
        })
        .then("the pretend dog answers", |w, _| {
            let owner = w.owner()?;
            ensure(owner == "pretend", format!("the dog answered {owner:?}"))
        })
        .then("the collar still shows {}", |w, name| {
            let tag = w.collar_tag()?;
            ensure(tag == name[0], format!("the collar shows {tag:?}"))
        })
        .then("the kennel still boards whoever the dog is", |w, _| {
            let kennel = w.scene()?.kennel().provider().and_then(|p| p.provide());
            let ticket = kennel
                .and_then(|k| k.board(Stay("weekend".into())))
                .map_err(|e| e.to_string())?;
            ensure(
                ticket.0 == w.owner()?,
                format!("the kennel boarded {:?}", ticket.0),
            )
        })
}

#[test]
fn dog_feature() {
    let report = verify(include_str!("../features/dog.feature"), &steps())
        .unwrap_or_else(|errors| panic!("dog.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 12);
}
