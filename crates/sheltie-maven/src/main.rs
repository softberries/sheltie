//! `sheltie-maven`: computes a change's footprint for Sheltie in a Maven repository.

fn main() -> anyhow::Result<()> {
    sheltie_adapter::cli::run(&sheltie_adapter::Generic::<sheltie_maven::Maven>::new())
}
