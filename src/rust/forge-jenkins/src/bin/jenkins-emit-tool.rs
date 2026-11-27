//! jenkins-emit-tool: preview-by-default CLI bridge translating
//! verified Jenkins webhooks into Forge dispatch calls.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (code, stdout, stderr) =
        forge_jenkins::emit_tool::execute(&argv, &|name| std::env::var(name).ok());
    print!("{stdout}");
    eprint!("{stderr}");
    std::process::exit(code);
}
