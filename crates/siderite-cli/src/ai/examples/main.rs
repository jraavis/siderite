use super::model::Product;
use siderite::prelude::*;
use siderite_cli::{AppCli, CliSettings};

/// The command line: `siderite run`, `check`, `migrate`, ... all go
/// through `AppCli`, which knows the app factory and the models.
#[tokio::main]
pub async fn main() -> std::process::ExitCode {
    let settings = match siderite::config::load() {
        Ok(settings) => CliSettings::from(&settings),
        Err(err) => {
            eprintln!("{err}");
            return std::process::ExitCode::from(1);
        }
    };
    AppCli::new(|| App::new().title("Shop"))
        .models(&[Product::META])
        .settings(settings)
        .migrations_dir("migrations")
        .run()
        .await
}
