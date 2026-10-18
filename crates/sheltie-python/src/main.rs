//! `sheltie-python`: computes a change's footprint for Sheltie.

fn main() -> anyhow::Result<()> {
    sheltie_adapter::cli::run(&sheltie_adapter::Generic::<sheltie_python::Python>::new())
}
