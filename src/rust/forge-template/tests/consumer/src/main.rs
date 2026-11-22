#[cfg(feature = "rendering")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use forge_core::bootstrap::Registry;
    use forge_core::provider::{Factory, InstanceProvider, Singleton};
    use forge_template::{
        assembly::{register_renderer, RenderConfig},
        Limits, MethodRegistry, Schema, Value,
    };
    let config = RenderConfig::Embedded {
        source: "Hello, {{ .to_uppercase() }}!",
        schema: Schema::Text,
        limits: Limits::default(),
    };
    let group = register_renderer("consumer.greeting", config, MethodRegistry::new())?;
    let frozen = Registry::new().register(group)?.freeze()?;
    let ((), factory) = frozen.into_factories();
    let provider = Singleton::new(factory.create()?);
    println!(
        "{}",
        provider.acquire()?.render(&Value::Text("Forge".into()))?
    );
    Ok(())
}

#[cfg(not(feature = "rendering"))]
fn main() {
    println!("Template capability omitted.");
}
