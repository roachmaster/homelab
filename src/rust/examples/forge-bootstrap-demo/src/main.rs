use forge_bootstrap_demo::{bootstrap, sum_route, AppConfig, Command, DemoContext};
use forge_ingress_contract::{Envelope, Header};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(feature = "messages"))]
    let config = AppConfig::default();
    #[cfg(feature = "messages")]
    let config = AppConfig {
        messages: Some(Default::default()),
        ..AppConfig::default()
    };
    let application = bootstrap(config)?;
    let context = DemoContext { allow: true }; // Demonstration only, not authentication.
    let request = Envelope::new(
        Header::new(sum_route(), "sum-1")?.with_correlation_id("demo-1")?,
        Command::Sum {
            left: 20,
            right: 22,
        },
    );
    println!("{:?}", application.dispatch(request, &context)?);

    #[cfg(feature = "messages")]
    {
        use forge_bootstrap_demo::messages::greeting_route;
        use forge_demo_messages::GreetingInput;
        let request = Envelope::new(
            Header::new(greeting_route(), "greet-1")?,
            Command::Greet(GreetingInput {
                destination: "memory:demo".into(),
                name: "Forge".into(),
            }),
        );
        println!("{:?}", application.dispatch(request, &context)?);
    }
    Ok(())
}
