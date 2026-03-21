#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    mu::run().await
}
