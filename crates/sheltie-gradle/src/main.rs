//! `sheltie-gradle`: computes a change's footprint for Sheltie in a Gradle repository.

fn main() -> anyhow::Result<()> {
    sheltie_adapter::cli::run(&sheltie_adapter::Generic::<sheltie_gradle::Gradle>::new())
}
