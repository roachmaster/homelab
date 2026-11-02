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

#[cfg(all(test, feature = "remote-inspection"))]
mod remote_tests {
    use forge_bootstrap::Registry;
    use forge_ide::{
        assembly::{register_projects, IdeConfig, ProjectAlias},
        remote::{
            InspectCall, InspectReply, RemoteInspectError, RemoteInspectInput, RemoteInspectTask,
            ReportLimits,
        },
        task::InspectRequest,
    };
    use forge_ingress_contract::Response;
    use forge_io_client::IoClient;
    use forge_provider::{Factory, InstanceProvider, Singleton};
    use forge_remote::Target;
    use forge_task::Task;

    struct InProcess(forge_ide::model::ProjectReport);
    impl IoClient for InProcess {
        type Request = InspectCall;
        type Response = InspectReply;
        type Error = RemoteInspectError;
        fn execute(&self, call: InspectCall) -> Result<InspectReply, RemoteInspectError> {
            Ok(InspectReply {
                responder: call.target,
                response: Response {
                    header: (&call.request.header).into(),
                    payload: self.0.clone(),
                },
            })
        }
    }

    #[test]
    fn public_batch_and_remote_contract_work_without_transport_or_yaml() {
        let root =
            std::env::temp_dir().join(format!("forge-ide-batch-consumer-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        {
            let alias = ProjectAlias::new("workspace").unwrap();
            let group = register_projects(
                "consumer.projects",
                vec![(alias.clone(), IdeConfig::project(root.clone()))],
            )
            .unwrap();
            let ((), projects) = Registry::new()
                .register(group)
                .unwrap()
                .freeze()
                .unwrap()
                .into_factories();
            let provider = Singleton::new(projects.factories()[0].1.create().unwrap());
            let report = provider
                .acquire()
                .unwrap()
                .run(InspectRequest::Project)
                .unwrap();
            let task =
                RemoteInspectTask::new(InProcess(report.clone()), ReportLimits::default()).unwrap();
            let output = task
                .run(RemoteInspectInput {
                    target: Target::new("test", "incarnation").unwrap(),
                    project: alias,
                    request_id: "consumer-1".into(),
                    correlation_id: None,
                    expires_unix_ms: 1234,
                })
                .unwrap();
            assert_eq!(output.response.payload, report);
        }
    }
}
