use std::path::PathBuf;

#[derive(Debug, clap::Parser)]
pub struct Args {
    /// Bind address for the HTTP server
    #[clap(short, long, default_value = "127.0.0.1:8080")]
    pub bind: String,

    #[clap(long, default_value = ".")]
    pub data_dir: PathBuf,

    #[command(flatten)]
    pub verbosity: clap_verbosity_flag::Verbosity,

    /// Path to the Chrome / Chromium executable for scraping
    #[arg(long = "chromium-bin", short = 'c')]
    pub chromium_binary: Option<PathBuf>,

    #[command(flatten)]
    pub apn: Apn,
}

#[derive(Debug, clap::Args)]
pub struct Apn {
    #[command(flatten)]
    pub certificate: Option<ApnCertificate>,
    #[command(flatten)]
    pub token: Option<ApnToken>,

    #[arg(long, default_value_t = false)]
    pub apn_sandbox: bool,
}

#[derive(Debug, clap::Args)]
#[group(requires = "apn_p12", multiple = true)]
#[group(conflicts_with = "apn_p8")]
pub struct ApnCertificate {
    /// Path to a PKCS12 file as private key
    #[arg(long, required = false)]
    pub apn_p12: PathBuf,

    /// Password for the private key
    #[arg(long, required = false)]
    pub apn_p12_password: Option<String>,
}

#[derive(Debug, clap::Args)]
#[group(requires = "apn_p8", multiple = true)]
pub struct ApnToken {
    /// Path to a PKCS8 file as private key
    #[arg(long, required = false)]
    pub apn_p8: PathBuf,

    /// Team ID
    #[arg(long, required = false)]
    pub apn_p8_team_id: String,

    /// Key ID
    #[arg(long, required = false)]
    pub apn_p8_key_id: String,
}
