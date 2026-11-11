use forge_bootstrap::Registry;
use forge_bridge::{Bridge, DispatchError, RegistryBuilder};
use forge_ingress_contract::{AdmissionDenied, Envelope, Header};
use forge_llama::{register_llama, register_routes, Config, Operation, Reply, Request};
use forge_provider::Factory;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    env, fs,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(
        env::args_os()
            .nth(1)
            .ok_or("supply a fresh temp directory")?,
    );
    let secret = directory.join("token");
    let mut token = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&secret)?;
    std::io::Write::write_all(&mut token, b"ab0123456789cd0123456789ef0123456789ab\n")?;
    let config = Config {
        // This consumer verifies construction and dispatch without launching llama.cpp.
        server: PathBuf::from("/bin/false"),
        backend_port: 18181,
        device: "SYCL0".into(),
        library_path: None,
        gpu_lock: directory.join("gpu.lock"),
        gpu_device: Some(directory.join("absent-gpu")),
        token_file: secret,
        models: BTreeMap::from([("example".into(), directory.join("example.gguf"))]),
        context_size: 4096,
        startup_seconds: 10,
        prompt_seconds: 10,
    };
    let lock = config.gpu_lock.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let group = register_llama("llama", config.clone(), stop.clone()).expect("stage");
    let registry = Registry::new()
        .register(group)
        .unwrap_or_else(|_| panic!("first group"));
    let duplicate = register_llama("llama", config, stop).expect("stage duplicate");
    let registry = match registry.register(duplicate) {
        Ok(_) => panic!("duplicate group accepted"),
        Err(rejection) => rejection.into_parts().0,
    };
    let factory = registry
        .freeze()
        .unwrap_or_else(|_| panic!("freeze"))
        .into_factories()
        .1;
    let driver = RefCell::new(factory.create()?);
    let mut routes = RegistryBuilder::new();
    register_routes(&mut routes, &driver)?;
    let bridge = Bridge::new(routes.freeze(), |_: &Header, allowed: &bool| {
        if *allowed {
            Ok(())
        } else {
            Err(AdmissionDenied)
        }
    });
    let dispatch = |op: Operation, payload, allowed| {
        bridge.dispatch(
            Envelope::new(Header::new(op.route(), "check").unwrap(), payload),
            &allowed,
        )
    };
    let start = || Request::Start {
        model: "example".into(),
    };
    assert!(matches!(
        dispatch(Operation::Start, start(), false),
        Err(DispatchError::Admission(_))
    ));
    assert!(matches!(
        dispatch(Operation::Models, start(), true),
        Err(DispatchError::PayloadMismatch(_))
    ));
    assert!(!lock.exists());
    assert!(matches!(
        dispatch(Operation::Start, start(), true),
        Err(DispatchError::Handler(forge_llama::Error::ModelUnavailable))
    ));
    assert!(
        matches!(dispatch(Operation::Models, Request::Models, true)?.payload, Reply::Models(models) if models.data[0].id == "example")
    );
    assert!(
        matches!(dispatch(Operation::Status, Request::Status, true)?.payload, Reply::Status(status) if status.state == "stopped")
    );
    assert!(!lock.exists());
    fs::write(directory.join("example.gguf"), b"GGUF")?;
    assert!(matches!(
        dispatch(Operation::Start, start(), true),
        Err(DispatchError::Handler(forge_llama::Error::GpuUnavailable))
    ));
    assert!(!lock.exists());
    Ok(())
}
