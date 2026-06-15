use std::path::PathBuf;

#[derive(clap::Parser)]
pub struct Args {
    /// Bind address for the HTTP server
    #[clap(short, long, default_value = "127.0.0.1:8080")]
    pub bind: String,

    #[clap(long, default_value = ".")]
    pub data_dir: PathBuf,

    #[command(flatten)]
    pub verbosity: clap_verbosity_flag::Verbosity,
}
