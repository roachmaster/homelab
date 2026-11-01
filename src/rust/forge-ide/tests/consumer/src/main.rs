fn main() {
    println!("IDE consumer: capability is opt-in.");
}
#[cfg(all(test, feature = "inspection"))]
mod tests {
    use forge_bootstrap::Registry;
    use forge_ide::{
        assembly::{register_ide, IdeConfig},
        task::InspectRequest,
    };
    use forge_provider::{Factory, InstanceProvider, Singleton, Transient};
    use forge_task::Task;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "forge-ide-consumer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn imported_public_registration_freeze_factory_provider_task() {
        let t = Temp::new();
        std::fs::write(t.0.join("lib.rs"), "trait T { fn f(); }\nstruct S;\n").unwrap();
        let group = register_ide("consumer.ide", IdeConfig::project(t.0.clone())).unwrap();
        let frozen = Registry::new()
            .register(group)
            .unwrap_or_else(|_| panic!())
            .freeze()
            .unwrap_or_else(|_| panic!());
        let ((), factory) = frozen.into_factories();
        let provider = Singleton::new(factory.create().unwrap());
        let report = provider
            .acquire()
            .unwrap()
            .run(InspectRequest::Project)
            .unwrap();
        assert!(report.complete);
        assert_eq!(report.files, 1);
        let transient = Transient::new(factory);
        assert_eq!(
            transient
                .acquire()
                .unwrap()
                .run(InspectRequest::Project)
                .unwrap(),
            report
        );
    }
    #[test]
    fn registration_does_not_open_and_factory_error_is_preserved() {
        let t = Temp::new();
        let missing = t.0.join("missing");
        let group = register_ide("missing", IdeConfig::project(missing.clone())).unwrap();
        let frozen = Registry::new()
            .register(group)
            .unwrap_or_else(|_| panic!())
            .freeze()
            .unwrap_or_else(|_| panic!());
        let ((), factory) = frozen.into_factories();
        assert!(factory.create().is_err());
        std::fs::create_dir(missing).unwrap();
        assert!(factory.create().is_ok());
    }
    #[test]
    fn duplicate_preserves_original_and_invalid_configuration_rejected() {
        let t = Temp::new();
        let a = register_ide("same", IdeConfig::project(t.0.clone())).unwrap();
        let b = register_ide("same", IdeConfig::project(t.0.join("not-created"))).unwrap();
        let pending = Registry::new().register(a).unwrap_or_else(|_| panic!());
        let rejected = pending.register(b).err().unwrap();
        let (original, _, _) = rejected.into_parts();
        let ((), factory) = original
            .freeze()
            .unwrap_or_else(|_| panic!())
            .into_factories();
        assert!(factory.create().is_ok());
        assert!(register_ide("relative", IdeConfig::project(PathBuf::from("relative"))).is_err());
        assert!(register_ide("parent", IdeConfig::project(t.0.join(".."))).is_err());
    }
}
