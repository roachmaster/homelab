use forge_core::bootstrap::Registry;
use forge_core::bridge::{Bridge, DispatchError, RegistryBuilder};
use forge_core::ingress::{AdmissionDenied, Envelope, Header, RouteKey};
use forge_core::provider::Factory;
use forge_llama::{
    register_llama, register_routes, Config, Error, MailExpertPaging, Message, Operation,
    Registration, Reply, Request, Role,
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let path = base.join(format!(
                "forge-llama-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            if fs::create_dir(&path).is_ok() {
                return Self(fs::canonicalize(path).unwrap());
            }
        }
        panic!("fixture names exhausted")
    }
    fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn valid_config(fixture: &Fixture) -> Config {
    Config {
        server: PathBuf::from("/bin/false"),
        backend_port: 18181,
        device: "SYCL0".into(),
        library_path: None,
        gpu_lock: fixture.join("gpu.lock"),
        gpu_device: None,
        token_file: fixture.join("token"),
        models: BTreeMap::from([("example".into(), fixture.join("example.gguf"))]),
        context_size: 4096,
        startup_seconds: 10,
        prompt_seconds: 10,
        mail_expert_paging: None,
    }
}

fn assert_invalid(config: Config) {
    let stop = Arc::new(AtomicBool::new(false));
    match register_llama("llama", config, stop) {
        Err(Registration::Config(Error::InvalidConfig)) => {}
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

#[test]
fn error_codes_are_exact_for_every_variant() {
    let cases = [
        (Error::InvalidConfig, "invalid_config"),
        (Error::UnknownModel, "unknown_model"),
        (Error::ModelUnavailable, "model_unavailable"),
        (Error::GpuUnavailable, "gpu_unavailable"),
        (Error::InvalidPrompt, "invalid_prompt"),
        (Error::MailProfileMismatch, "mail_profile_mismatch"),
        (Error::Stopped, "runtime_stopped"),
        (Error::ModelMismatch, "model_not_loaded"),
        (Error::Busy, "runtime_busy"),
        (Error::StartFailed, "startup_failed"),
        (Error::Backend, "backend_error"),
        (Error::Timeout, "backend_timeout"),
        (Error::Canceled, "request_canceled"),
        (Error::Shutdown, "shutting_down"),
    ];
    for (error, code) in cases {
        assert_eq!(error.code(), code);
        assert_eq!(error.to_string(), code);
    }
}

#[test]
fn operations_map_to_exact_route_names() {
    let cases = [
        (Operation::Models, "llama.models"),
        (Operation::Status, "llama.status"),
        (Operation::Start, "llama.start"),
        (Operation::Stop, "llama.stop"),
        (Operation::Chat, "llama.chat"),
        (Operation::CountPrompt, "llama.prompt.count"),
    ];
    for (operation, name) in cases {
        let route = operation.route();
        assert_eq!(route, RouteKey::new(name, 1).unwrap());
    }
}

#[test]
fn requests_report_their_operation_kind() {
    use forge_core::ingress::Payload;
    let chat = forge_llama::Chat {
        model: None,
        messages: vec![Message {
            role: Role::User,
            content: "hi".into(),
        }],
        max_tokens: None,
        temperature: None,
        stream: false,
        mail_profile: None,
        cancellation: Arc::new(AtomicBool::new(false)),
    };
    let prompt = forge_llama::Prompt {
        model: "example".into(),
        messages: vec![Message {
            role: Role::User,
            content: "hi".into(),
        }],
    };
    let cases = [
        (Request::Models, Operation::Models),
        (Request::Status, Operation::Status),
        (
            Request::Start {
                model: "example".into(),
            },
            Operation::Start,
        ),
        (Request::Stop, Operation::Stop),
        (Request::Chat(chat), Operation::Chat),
        (Request::CountPrompt(prompt), Operation::CountPrompt),
    ];
    for (request, operation) in cases {
        assert!(request.kind() == operation);
    }
}

#[test]
fn registration_rejects_every_invalid_field() {
    let fixture = Fixture::new();
    let mut invalid = valid_config(&fixture);
    invalid.server = PathBuf::from("relative/server");
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.gpu_lock = PathBuf::from("relative/gpu.lock");
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.token_file = PathBuf::from("relative/token");
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.gpu_device = Some(PathBuf::from("relative/gpu"));
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.backend_port = 0;
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.device = String::new();
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.device = "SYCL-0".into();
    assert_invalid(invalid);
    for context_size in [511, 65537] {
        let mut invalid = valid_config(&fixture);
        invalid.context_size = context_size;
        assert_invalid(invalid);
    }
    for startup_seconds in [0, 181] {
        let mut invalid = valid_config(&fixture);
        invalid.startup_seconds = startup_seconds;
        assert_invalid(invalid);
    }
    for prompt_seconds in [0, 301] {
        let mut invalid = valid_config(&fixture);
        invalid.prompt_seconds = prompt_seconds;
        assert_invalid(invalid);
    }
    let mut invalid = valid_config(&fixture);
    invalid.models = BTreeMap::new();
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.models = (0..33)
        .map(|n| {
            (
                format!("model-{n}"),
                fixture.join(format!("model-{n}.gguf")),
            )
        })
        .collect();
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.models = BTreeMap::from([("bad id".into(), fixture.join("example.gguf"))]);
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.models = BTreeMap::from([("example".into(), PathBuf::from("relative/example.gguf"))]);
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.models = BTreeMap::from([("example".into(), fixture.join("example.bin"))]);
    assert_invalid(invalid);
}

#[test]
fn registration_rejects_mail_paging_mismatches() {
    let fixture = Fixture::new();
    let paging = || MailExpertPaging {
        profile_id: "mail-1".into(),
        context_size: 4096,
        generation_tokens: 512,
        margin_tokens: 64,
        prompt_seconds: 60,
    };
    // Mail model without paging, and paging without the mail model.
    let mut invalid = valid_config(&fixture);
    invalid
        .models
        .insert("gpt-oss-120b".into(), fixture.join("gpt-oss-120b.gguf"));
    assert_invalid(invalid);
    let mut invalid = valid_config(&fixture);
    invalid.mail_expert_paging = Some(paging());
    assert_invalid(invalid);
    // Paging profile rules, each broken in turn, with the mail model present.
    let mut base = valid_config(&fixture);
    base.models
        .insert("gpt-oss-120b".into(), fixture.join("gpt-oss-120b.gguf"));
    base.mail_expert_paging = Some(paging());
    assert!(register_llama("llama", base.clone(), Arc::new(AtomicBool::new(false))).is_ok());
    let mut invalid = base.clone();
    invalid.device = "CPU".into();
    assert_invalid(invalid);
    let mut invalid = base.clone();
    invalid.mail_expert_paging.as_mut().unwrap().profile_id = "bad id".into();
    assert_invalid(invalid);
    for context_size in [1023, 8193] {
        let mut invalid = base.clone();
        invalid.mail_expert_paging.as_mut().unwrap().context_size = context_size;
        assert_invalid(invalid);
    }
    for generation_tokens in [0, 2049] {
        let mut invalid = base.clone();
        invalid
            .mail_expert_paging
            .as_mut()
            .unwrap()
            .generation_tokens = generation_tokens;
        assert_invalid(invalid);
    }
    for margin_tokens in [0, 129] {
        let mut invalid = base.clone();
        invalid.mail_expert_paging.as_mut().unwrap().margin_tokens = margin_tokens;
        assert_invalid(invalid);
    }
    let mut invalid = base.clone();
    let profile = invalid.mail_expert_paging.as_mut().unwrap();
    profile.generation_tokens = 2048;
    profile.margin_tokens = 128;
    profile.context_size = 2048 + 128;
    assert_invalid(invalid);
    for prompt_seconds in [0, 1801] {
        let mut invalid = base.clone();
        invalid.mail_expert_paging.as_mut().unwrap().prompt_seconds = prompt_seconds;
        assert_invalid(invalid);
    }
}

fn write_token(fixture: &Fixture) -> PathBuf {
    let secret = fixture.join("token");
    let mut token = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&secret)
        .unwrap();
    std::io::Write::write_all(&mut token, b"ab0123456789cd0123456789ef0123456789ab\n").unwrap();
    secret
}

type LlamaBridge =
    Bridge<'static, Request, bool, Reply, Error, fn(&Header, &bool) -> Result<(), AdmissionDenied>>;

struct Harness {
    bridge: LlamaBridge,
}

fn harness(fixture: &Fixture) -> Harness {
    // The driver is leaked so the bridge can hold it for 'static; tests are
    // short-lived and each harness builds exactly one driver.
    let config = valid_config(fixture);
    write_token(fixture);
    let stop = Arc::new(AtomicBool::new(false));
    let group = register_llama("llama", config, stop).expect("stage");
    let factory = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories()
        .1;
    let driver: &'static RefCell<forge_llama::Driver> =
        Box::leak(Box::new(RefCell::new(factory.create().unwrap())));
    let mut routes = RegistryBuilder::new();
    register_routes(&mut routes, driver).unwrap();
    let admission: fn(&Header, &bool) -> Result<(), AdmissionDenied> =
        |_: &Header, allowed: &bool| {
            if *allowed {
                Ok(())
            } else {
                Err(AdmissionDenied)
            }
        };
    let bridge = Bridge::new(routes.freeze(), admission);
    Harness { bridge }
}

fn dispatch(
    harness: &Harness,
    op: Operation,
    payload: Request,
) -> Result<serde_json::Value, DispatchError<Error>> {
    let response = harness.bridge.dispatch(
        Envelope::new(Header::new(op.route(), "check").unwrap(), payload),
        &true,
    )?;
    Ok(serde_json::to_value(&response.payload).unwrap())
}

fn user_chat() -> Request {
    Request::Chat(forge_llama::Chat {
        model: None,
        messages: vec![Message {
            role: Role::User,
            content: "hi".into(),
        }],
        max_tokens: None,
        temperature: None,
        stream: false,
        mail_profile: None,
        cancellation: Arc::new(AtomicBool::new(false)),
    })
}

#[test]
fn driver_reports_models_and_stopped_status_without_launching() {
    let fixture = Fixture::new();
    let harness = harness(&fixture);
    let models = dispatch(&harness, Operation::Models, Request::Models).unwrap();
    assert_eq!(models["object"], "list");
    assert_eq!(models["data"][0]["id"], "example");
    assert_eq!(models["data"][0]["object"], "model");
    let status = dispatch(&harness, Operation::Status, Request::Status).unwrap();
    assert_eq!(status["state"], "stopped");
    assert!(status["model"].is_null());
    // Repeated dispatches are identical.
    assert_eq!(
        dispatch(&harness, Operation::Models, Request::Models).unwrap(),
        models
    );
    assert_eq!(
        dispatch(&harness, Operation::Status, Request::Status).unwrap(),
        status
    );
}

#[test]
fn driver_rejects_unknown_models_and_missing_files_before_launch() {
    let fixture = Fixture::new();
    let harness = harness(&fixture);
    assert_eq!(
        dispatch(
            &harness,
            Operation::Start,
            Request::Start {
                model: "absent".into(),
            }
        ),
        Err(DispatchError::Handler(Error::UnknownModel))
    );
    assert_eq!(
        dispatch(
            &harness,
            Operation::Start,
            Request::Start {
                model: "example".into(),
            }
        ),
        Err(DispatchError::Handler(Error::ModelUnavailable))
    );
    assert!(!fixture.join("gpu.lock").exists());
}

#[test]
fn driver_rejects_chat_without_a_running_model_and_bad_prompts() {
    let fixture = Fixture::new();
    let harness = harness(&fixture);
    assert_eq!(
        dispatch(&harness, Operation::Chat, user_chat()),
        Err(DispatchError::Handler(Error::Stopped))
    );
    let empty = Request::Chat(forge_llama::Chat {
        model: None,
        messages: vec![],
        max_tokens: None,
        temperature: None,
        stream: false,
        mail_profile: None,
        cancellation: Arc::new(AtomicBool::new(false)),
    });
    assert_eq!(
        dispatch(&harness, Operation::Chat, empty),
        Err(DispatchError::Handler(Error::InvalidPrompt))
    );
    let no_user = Request::Chat(forge_llama::Chat {
        model: None,
        messages: vec![Message {
            role: Role::System,
            content: "be nice".into(),
        }],
        max_tokens: None,
        temperature: None,
        stream: false,
        mail_profile: None,
        cancellation: Arc::new(AtomicBool::new(false)),
    });
    assert_eq!(
        dispatch(&harness, Operation::Chat, no_user),
        Err(DispatchError::Handler(Error::InvalidPrompt))
    );
    let prompt = Request::CountPrompt(forge_llama::Prompt {
        model: "example".into(),
        messages: vec![Message {
            role: Role::User,
            content: "hi".into(),
        }],
    });
    assert_eq!(
        dispatch(&harness, Operation::CountPrompt, prompt),
        Err(DispatchError::Handler(Error::ModelMismatch))
    );
}
