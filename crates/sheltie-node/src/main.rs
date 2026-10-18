//! `sheltie-node`: computes a change's footprint for Sheltie in JavaScript and TypeScript
//! repositories.

fn main() -> anyhow::Result<()> {
    sheltie_adapter::cli::run(&sheltie_adapter::Generic::<sheltie_node::Node>::new())
}
