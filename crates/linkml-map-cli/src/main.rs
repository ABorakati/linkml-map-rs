use clap::Parser;

#[derive(Parser)]
#[command(name = "linkml-tr-rs")]
#[command(version = "0.1.0")]
#[command(about = "LinkML transformation engine (Rust)", long_about = None)]
struct Args {
    // Parser will auto-generate --version from command attributes
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _args = Args::parse();
    Ok(())
}
