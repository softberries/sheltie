//! `sheltie-cargo`: computes a change's footprint for Sheltie.

fn main() -> anyhow::Result<()> {
    sheltie_adapter::cli::run(&sheltie_cargo::Cargo)
}
