use clap::Parser;

#[derive(Parser)]
#[command(
    name = "docsbase",
    version,
    about = "Local-first docs memory MCP server"
)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
